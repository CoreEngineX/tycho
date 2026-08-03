//! Layer 4. The walk, entry classification, and the sanity gate that fails a run
//! whose root yielded nothing.

use crate::config::Profile;
use crate::config::rules::{Decision, Hits, RuleTree, Tier, Verdict};
use crate::primitives::encode::{FileMode, percent_component};
use crate::primitives::names::{AliasError, BranchName, RootAlias};
use crate::primitives::oid::Oid;
use crate::primitives::path::{AbsPath, TreePath, TreePathError};
use crate::sys::fs::{FileKind, SkipReason, classify_path};
use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fs;
use std::path::Path;

/// A root must lose more than half its entries before the gate fires, and only once
/// it had enough for the ratio to mean anything. Three files becoming one is noise;
/// eight thousand becoming three thousand is an event worth confirming once.
pub const SHRINK_RATIO: f64 = 0.5;
pub const SHRINK_FLOOR: usize = 100;

/// How many repositories the dry run lists before it truncates.
pub const REPO_TABLE_ROWS: usize = 10;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlainFile {
    pub source: AbsPath,
    pub stored: TreePath,
    pub mode: FileMode,
    pub size: u64,
}

/// A captured repository. `key` is the store's identifier for it: the root alias,
/// then the repository's path relative to that root, each component percent-encoded
/// so it is legal in a refname.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RepoRoot {
    pub source: AbsPath,
    pub key: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Entry {
    Plain(PlainFile),
    Repo(RepoRoot),
}

/// Where a repository's `HEAD` points. A repository with no commits is `Unborn` -
/// not an error, and not representable as a missing branch name.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RepoHead {
    Branch { name: BranchName, sha: Oid },
    Detached { sha: Oid },
    Unborn,
}

/// A path the walk could not read, or deliberately did not store. Leaf failures are
/// warnings: one unreadable file is not a reason to lose the other fifty thousand.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Warning {
    Unreadable { path: String, reason: String },
    Skipped { path: String, reason: SkipReason },
    NotStorable { path: String, reason: String },
}

#[derive(Clone, Debug, thiserror::Error)]
pub enum PlanError {
    #[error("watched root {root} could not be read: {reason}")]
    RootUnreadable { root: String, reason: String },
    #[error(
        "watched root {root} yielded nothing to back up; refusing to record an empty backup for it"
    )]
    RootEmpty { root: String },
    #[error(
        "watched root {root} dropped from {before} entries to {after}; pass --allow-shrink if that is intended"
    )]
    RootShrank {
        root: String,
        before: usize,
        after: usize,
    },
    #[error("watched root {root} has no usable alias: {reason}")]
    BadAlias { root: String, reason: String },
}

#[derive(Clone, Debug)]
pub struct RootPlan {
    pub alias: String,
    pub root: AbsPath,
    pub entries: Vec<Entry>,
    pub warnings: Vec<Warning>,
    pub files: usize,
    pub bytes: u64,
}

impl RootPlan {
    /// Plain files plus repositories. What the sanity gate counts, because a root
    /// entirely inside a repository has no plain files and is still not empty.
    #[must_use]
    pub fn capturable(&self) -> usize {
        self.entries.len()
    }

    pub fn repos(&self) -> impl Iterator<Item = &RepoRoot> {
        self.entries.iter().filter_map(|entry| match entry {
            Entry::Repo(repo) => Some(repo),
            Entry::Plain(_) => None,
        })
    }
}

/// Why a rule appears in the dry run's excluded table. The two halves answer
/// different questions and an earlier version of this conflated them.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum ExcludeReason {
    IgnoreRule,
    GlobRule,
    DefaultJunk,
    MatchedNothing,
}

impl ExcludeReason {
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::IgnoreRule => "ignore rule",
            Self::GlobRule => "glob rule",
            Self::DefaultJunk => "default junk",
            Self::MatchedNothing => "matched nothing",
        }
    }
}

#[derive(Debug, Default)]
pub struct Plan {
    pub roots: Vec<RootPlan>,
    /// What the rules threw away, and what they failed to throw away. A rule that
    /// matched nothing is the row that earns `--dry-run`: a typo'd ignore path is
    /// otherwise a silent no-op that commits gigabytes into permanent history.
    ///
    /// Only rules a person wrote can be reported as matching nothing. Most of the
    /// twenty default junk patterns match nothing on any given tree, and listing
    /// them would bury the one row that matters.
    pub excluded: Vec<(String, ExcludeReason)>,
}

impl Plan {
    #[must_use]
    pub fn files(&self) -> usize {
        self.roots.iter().map(|root| root.files).sum()
    }

    #[must_use]
    pub fn bytes(&self) -> u64 {
        self.roots.iter().map(|root| root.bytes).sum()
    }

    #[must_use]
    pub fn repo_count(&self) -> usize {
        self.roots.iter().map(|root| root.repos().count()).sum()
    }

    pub fn warnings(&self) -> impl Iterator<Item = &Warning> {
        self.roots.iter().flat_map(|root| root.warnings.iter())
    }
}

/// Walks every watched root and applies the gate.
///
/// # Errors
///
/// If a root cannot be read, yields nothing, or shrank past the threshold without
/// `allow_shrink`.
pub fn build(
    profile: &Profile,
    tree: &RuleTree,
    previous: &BTreeMap<String, usize>,
    allow_shrink: bool,
) -> Result<Plan, PlanError> {
    let mut plan = Plan::default();
    let mut hits = Hits::default();
    let mut fired: BTreeMap<String, ExcludeReason> = BTreeMap::new();

    for entry in &profile.watch {
        let alias = entry
            .alias()
            .map_err(|reason: AliasError| PlanError::BadAlias {
                root: entry.path().to_string(),
                reason: reason.to_string(),
            })?;
        let root = walk_root(entry.path(), &alias, tree, &mut hits, &mut fired)?;

        if root.capturable() == 0 {
            return Err(PlanError::RootEmpty {
                root: root.root.to_string(),
            });
        }
        if let Some(before) = previous.get(&root.alias)
            && !allow_shrink
            && shrank(*before, root.capturable())
        {
            return Err(PlanError::RootShrank {
                root: root.root.to_string(),
                before: *before,
                after: root.capturable(),
            });
        }
        plan.roots.push(root);
    }

    plan.excluded = fired.into_iter().collect();
    plan.excluded.extend(unfired(profile, tree, &hits));
    Ok(plan)
}

/// Whether a drop is large enough to be worth confirming once.
#[must_use]
pub fn shrank(before: usize, after: usize) -> bool {
    #[allow(clippy::cast_precision_loss)]
    let ratio = after as f64 / before as f64;
    before >= SHRINK_FLOOR && ratio < SHRINK_RATIO
}

/// One traversal in two modes.
///
/// Outside a repository it stats every entry, so plain files become blobs. Inside
/// one it descends **directories only**, because that repository's tracked content is
/// captured as history and its uncommitted content comes from the overlay - so the
/// walk is there purely to find nested `.git` markers, which on this machine is the
/// normal case rather than an edge one.
fn walk_root(
    root: &AbsPath,
    alias: &RootAlias,
    tree: &RuleTree,
    hits: &mut Hits,
    fired: &mut BTreeMap<String, ExcludeReason>,
) -> Result<RootPlan, PlanError> {
    let mut plan = RootPlan {
        alias: alias.to_string(),
        root: root.clone(),
        entries: Vec::new(),
        warnings: Vec::new(),
        files: 0,
        bytes: 0,
    };
    let mut walk = Walk {
        root,
        alias,
        tree,
        hits,
        fired,
    };

    // A watch entry may name a file rather than a directory - the escape hatch for
    // re-including one file a glob would otherwise exclude.
    let kind = classify_path(root.as_path()).map_err(|error| PlanError::RootUnreadable {
        root: root.to_string(),
        reason: error.to_string(),
    })?;
    if kind != FileKind::Directory {
        take_file(&mut walk, &mut plan, root.as_path(), kind);
        return Ok(plan);
    }

    // An explicit stack rather than recursion, so a pathological depth is not a
    // stack overflow.
    let mut stack = vec![(root.as_path().to_path_buf(), false)];
    let mut first = true;

    while let Some((dir, inside_repo)) = stack.pop() {
        let listing = match fs::read_dir(&dir) {
            Ok(listing) => listing,
            Err(error) => {
                // The root's own read failing is the TCC case: it produced a green
                // empty backup, which is what the gate exists to prevent.
                if first {
                    return Err(PlanError::RootUnreadable {
                        root: root.to_string(),
                        reason: error.to_string(),
                    });
                }
                plan.warnings.push(Warning::Unreadable {
                    path: dir.display().to_string(),
                    reason: error.to_string(),
                });
                continue;
            }
        };
        first = false;

        for item in listing {
            let path = match item {
                Ok(item) => item.path(),
                Err(error) => {
                    plan.warnings.push(Warning::Unreadable {
                        path: dir.display().to_string(),
                        reason: error.to_string(),
                    });
                    continue;
                }
            };
            if is_git_marker(&path) {
                continue;
            }
            walk.tree.record_hits(&path, walk.hits);

            let kind = match classify_path(&path) {
                Ok(kind) => kind,
                Err(error) => {
                    plan.warnings.push(Warning::Unreadable {
                        path: path.display().to_string(),
                        reason: error.to_string(),
                    });
                    continue;
                }
            };

            if kind == FileKind::Directory {
                let decision = walk.tree.resolve(&path);
                if decision.verdict == Verdict::Skip {
                    note(walk.fired, &decision);
                    if !walk.tree.may_contain_captures(&path) {
                        continue;
                    }
                }
                if is_repository(&path) {
                    match repo_root(&path, root, alias) {
                        Ok(repo) => plan.entries.push(Entry::Repo(repo)),
                        Err(reason) => plan.warnings.push(Warning::NotStorable {
                            path: path.display().to_string(),
                            reason,
                        }),
                    }
                    stack.push((path, true));
                } else {
                    stack.push((path, inside_repo));
                }
                continue;
            }

            // Inside a repository only directories matter; its files are history.
            if inside_repo {
                continue;
            }
            take_file(&mut walk, &mut plan, &path, kind);
        }
    }

    plan.entries.sort_by(|a, b| source_of(a).cmp(source_of(b)));
    Ok(plan)
}

/// The walk's context, so the recursion carries one parameter rather than five.
struct Walk<'a> {
    root: &'a AbsPath,
    alias: &'a RootAlias,
    tree: &'a RuleTree,
    hits: &'a mut Hits,
    fired: &'a mut BTreeMap<String, ExcludeReason>,
}

fn take_file(walk: &mut Walk<'_>, plan: &mut RootPlan, path: &Path, kind: FileKind) {
    let (root, alias, tree) = (walk.root, walk.alias, walk.tree);
    tree.record_hits(path, walk.hits);
    let decision = tree.resolve(path);
    if decision.verdict == Verdict::Skip {
        note(walk.fired, &decision);
        return;
    }
    let Some(mode) = kind.mode() else {
        if let FileKind::Skip(reason) = kind {
            plan.warnings.push(Warning::Skipped {
                path: path.display().to_string(),
                reason,
            });
        }
        return;
    };
    let stored = match stored_path(path, root, alias) {
        Ok(stored) => stored,
        Err(reason) => {
            plan.warnings.push(Warning::NotStorable {
                path: path.display().to_string(),
                reason: reason.to_string(),
            });
            return;
        }
    };
    let source = match AbsPath::from_absolute(path) {
        Ok(source) => source,
        Err(reason) => {
            plan.warnings.push(Warning::NotStorable {
                path: path.display().to_string(),
                reason: reason.to_string(),
            });
            return;
        }
    };
    let size = fs::symlink_metadata(path).map_or(0, |metadata| metadata.len());
    plan.files += 1;
    plan.bytes += size;
    plan.entries.push(Entry::Plain(PlainFile {
        source,
        stored,
        mode,
        size,
    }));
}

/// `<alias>/<path relative to the root>`, raw bytes. Only the refname key is
/// encoded; the tree mirrors what is on disk.
fn stored_path(path: &Path, root: &AbsPath, alias: &RootAlias) -> Result<TreePath, TreePathError> {
    let mut joined = OsString::from(alias.as_str());
    if let Ok(relative) = path.strip_prefix(root.as_path())
        && !relative.as_os_str().is_empty()
    {
        joined.push("/");
        joined.push(relative.as_os_str());
    }
    TreePath::parse(Path::new(&joined))
}

/// The key is the alias, then each component of the repository's path relative to
/// the root, percent-encoded so the whole thing is legal inside a refname.
fn repo_root(path: &Path, root: &AbsPath, alias: &RootAlias) -> Result<RepoRoot, String> {
    let relative = path
        .strip_prefix(root.as_path())
        .map_err(|_| "the repository is not under its root".to_owned())?;
    let mut key = alias.as_str().to_owned();
    for component in relative.components() {
        key.push('/');
        key.push_str(&percent_component(component.as_os_str().as_encoded_bytes()));
    }
    let source = AbsPath::from_absolute(path).map_err(|error| error.to_string())?;
    Ok(RepoRoot { source, key })
}

/// Both forms count. Every platform repository on this machine is a submodule, so
/// its `.git` is a *file* holding a `gitdir:` pointer, and detecting only the
/// directory form would miss all of them.
fn is_repository(dir: &Path) -> bool {
    matches!(
        classify_path(&dir.join(".git")),
        Ok(FileKind::Directory | FileKind::Regular { .. })
    )
}

fn is_git_marker(path: &Path) -> bool {
    path.file_name()
        .is_some_and(|name| name.as_encoded_bytes().eq_ignore_ascii_case(b".git"))
}

fn source_of(entry: &Entry) -> &AbsPath {
    match entry {
        Entry::Plain(file) => &file.source,
        Entry::Repo(repo) => &repo.source,
    }
}

/// Records the rule that decided a skip, so the dry run can say what was thrown
/// away and by what.
fn note(fired: &mut BTreeMap<String, ExcludeReason>, decision: &Decision) {
    if decision.rule.is_empty() {
        return;
    }
    let reason = match decision.tier {
        Tier::ExplicitPath => ExcludeReason::IgnoreRule,
        Tier::Glob => ExcludeReason::GlobRule,
        Tier::Junk => ExcludeReason::DefaultJunk,
    };
    fired.insert(decision.rule.clone(), reason);
}

/// Rules a person wrote that matched nothing: an ignore or reinclude path that does
/// not exist, and a glob that never fired. The default junk list is excluded on
/// purpose - most of its twenty patterns match nothing on any given tree, and
/// listing them would bury the one row that matters.
fn unfired(profile: &Profile, tree: &RuleTree, hits: &Hits) -> Vec<(String, ExcludeReason)> {
    let mut out = Vec::new();
    for path in profile.ignore_paths.iter().chain(&profile.reinclude) {
        if fs::symlink_metadata(path.as_path()).is_err() {
            out.push((path.to_string(), ExcludeReason::MatchedNothing));
        }
    }
    for (index, pattern) in tree.glob_patterns().iter().enumerate() {
        if !hits.globs.contains(&index) {
            out.push((pattern.clone(), ExcludeReason::MatchedNothing));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::{SHRINK_FLOOR, shrank};

    #[test]
    fn a_small_root_shrinking_is_noise_and_a_large_one_is_an_event() {
        assert!(!shrank(3, 1), "three files becoming one is not a signal");
        assert!(!shrank(SHRINK_FLOOR - 1, 0));
        assert!(shrank(8_000, 3_000));
        assert!(
            !shrank(8_000, 5_000),
            "a drop to 62% is under the threshold"
        );
        assert!(shrank(100, 0));
        assert!(!shrank(100, 50), "exactly half is not below half");
        assert!(shrank(100, 49));
    }
}
