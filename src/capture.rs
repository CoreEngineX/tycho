//! Layer 4. Hashing plain files, fetching each repository's refs into the store, and
//! building the overlay of what git history alone cannot restore.
//!
//! The overlay is why this project exists. A sync incident in July 2026 deleted files
//! under a repository, and git restored every one of them except `CLAUDE.md`, which
//! was gitignored: the only unprotected file was the one git could not resurrect.

use crate::config::rules::RuleTree;
use crate::git::refs::{Refspec, list_refs};
use crate::plan::{Plan, RepoHead, RepoRoot, RootPlan};
use crate::primitives::names::{BranchName, RefName};
use crate::primitives::oid::Oid;
use crate::primitives::path::{AbsPath, TreePath, has_git_component};
use crate::primitives::refs::find_collisions;
use crate::store::Store;
use crate::sys::fs::{FileKind, classify_path};
use crate::sys::process::{Git, RunError, Timeout};
use std::ffi::OsString;
use std::path::Path;

/// What a repository looks like right now, for the dry run's head and state columns.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Inspection {
    pub head: RepoHead,
    pub modified: usize,
    pub untracked: usize,
    /// The repository's object database size. On a machine where nearly everything
    /// is inside a repository, this is the bulk of what a run reads, and a plan that
    /// reported only loose files would understate it by orders of magnitude.
    pub objects: u64,
}

impl Inspection {
    /// The dry run's `state` column: `clean`, or the dominant kind of dirtiness.
    #[must_use]
    pub fn state(&self) -> String {
        match (self.modified, self.untracked) {
            (0, 0) => "clean".to_owned(),
            (0, untracked) => format!("{untracked} untracked"),
            (modified, 0) => format!("{modified} modified"),
            (modified, untracked) => format!("{modified} modified, {untracked} untracked"),
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum CaptureError {
    #[error(transparent)]
    Run(#[from] RunError),
    #[error(transparent)]
    Repo(#[from] crate::git::RepoError),
    #[error(
        "{repo} uses the {found} object format and this store is sha1; it cannot be fetched, and skipping it would lose it silently"
    )]
    ObjectFormat { repo: String, found: String },
    #[error(
        "{repo} has refs that collide once stored: {detail}; fetching both would let the second silently clobber the first"
    )]
    RefCollision { repo: String, detail: String },
    #[error("'{path}' cannot be stored in the tree: {reason}")]
    NotStorable { path: String, reason: String },
}

/// One repository's contribution to the backup tree.
#[derive(Debug, Default)]
pub struct Contribution {
    /// Files on disk to hash: the overlay.
    pub files: Vec<(AbsPath, TreePath)>,
    /// Content Tycho generates rather than reads: `REPO.txt`.
    pub generated: Vec<(TreePath, Vec<u8>)>,
    pub warnings: Vec<String>,
}

impl Contribution {
    fn absorb(&mut self, other: Self) {
        self.files.extend(other.files);
        self.generated.extend(other.generated);
        self.warnings.extend(other.warnings);
    }
}

/// Reads a repository's head and working-tree state without writing to it.
///
/// `--no-optional-locks` comes from the runner, and it is what makes "Tycho never
/// writes to a source" true rather than aspirational: plain `git status` rewrites
/// `.git/index` as a cache update, so without it every dry run would modify every
/// repository it looked at.
///
/// # Errors
///
/// If git cannot be run at all. A repository with no commits is `Unborn`, which is
/// an answer rather than a failure.
pub fn inspect(repo: &Path) -> Result<Inspection, RunError> {
    let git = Git::at(repo);

    let branch = git.run(
        &["symbolic-ref", "--quiet", "--short", "HEAD"],
        Timeout::QUICK,
    )?;
    let sha = git.run(&["rev-parse", "HEAD"], Timeout::QUICK)?;

    let head = if sha.status.success() {
        let oid = Oid::parse(String::from_utf8_lossy(&sha.stdout).trim()).ok();
        match (oid, branch.status.success()) {
            (Some(sha), true) => {
                let name = String::from_utf8_lossy(&branch.stdout).trim().to_owned();
                BranchName::parse(&name).map_or(RepoHead::Detached { sha }, |name| {
                    RepoHead::Branch { name, sha }
                })
            }
            (Some(sha), false) => RepoHead::Detached { sha },
            (None, _) => RepoHead::Unborn,
        }
    } else {
        RepoHead::Unborn
    };

    let status = git.run(
        &["status", "--porcelain=v1", "-z", "--untracked-files=all"],
        Timeout::WORK,
    )?;

    let mut modified = 0;
    let mut untracked = 0;
    for record in status
        .stdout
        .split(|&byte| byte == 0)
        .filter(|record| record.len() > 2)
    {
        if record.starts_with(b"??") {
            untracked += 1;
        } else {
            modified += 1;
        }
    }

    Ok(Inspection {
        head,
        modified,
        untracked,
        objects: object_bytes(&git)?,
    })
}

/// Everything one repository contributes: its history into the store's refs, its
/// overlay, and its provenance.
///
/// # Errors
///
/// If the object format differs, refs collide, or git fails. All three fail the run
/// rather than skipping the repository, because a repository silently left out is
/// indistinguishable from one that was never there.
pub fn capture(
    store: &Store,
    repo: &RepoRoot,
    rules: &RuleTree,
    nested: &[AbsPath],
) -> Result<Contribution, CaptureError> {
    let source = repo.source.as_path();
    let git = Git::at(source);

    check_object_format(&git, source)?;
    check_collisions(source, &repo.key)?;

    store.repo().fetch_refs(
        source,
        &Refspec::forced("refs/*", &format!("refs/tycho/{}/*", repo.key)),
    )?;

    let mut contribution = capture_stashes(store, &git, repo);
    contribution.absorb(overlay(source, repo, rules, nested));

    let inspection = inspect(source)?;
    contribution.generated.push((
        tree_path(&format!(".tycho/repos/{}/REPO.txt", repo.key))?,
        repo_txt(&git, &inspection, source).into_bytes(),
    ));
    Ok(contribution)
}

/// A SHA-256 repository cannot be fetched into a SHA-1 store, and the reverse is
/// equally true. `store.md` makes this a hard per-repo error rather than a warning.
fn check_object_format(git: &Git<'_>, source: &Path) -> Result<(), CaptureError> {
    let out = git.run(&["rev-parse", "--show-object-format"], Timeout::QUICK)?;
    let found = String::from_utf8_lossy(&out.stdout).trim().to_owned();
    if out.status.success() && found != "sha1" {
        return Err(CaptureError::ObjectFormat {
            repo: source.display().to_string(),
            found,
        });
    }
    Ok(())
}

/// Loose refs are files, so on a case-insensitive volume a repository carrying both
/// `Feature` and `feature` maps two branches onto one path.
///
/// **Git never notices, on NTFS.** The comment here used to say it errors on first
/// exposure and goes silent afterwards. Measured on Windows, every fetch exits 0 with
/// no message and the store keeps whichever of the two it wrote last, so the captured
/// branch *oscillates*: five consecutive fetches of the same unchanged source gave
/// `feature`, `Feature`, `feature`, `Feature`, `feature`. Each run silently drops the
/// other branch, and which one is in a given backup is decided by nothing.
///
/// So this runs before the fetch and is the only thing standing between a colliding
/// repository and a backup whose contents change identity between runs. A source can
/// carry both on NTFS even though `git branch` refuses to create the second: they
/// arrive together in `packed-refs`, which is one file, from a clone of a
/// case-sensitive filesystem.
fn check_collisions(source: &Path, key: &str) -> Result<(), CaptureError> {
    let refs = list_refs(source)?;
    let destinations: Vec<RefName> = refs
        .keys()
        .filter_map(|name| {
            let rest = name.as_str().strip_prefix("refs/")?;
            RefName::parse(&format!("refs/tycho/{key}/{rest}")).ok()
        })
        .collect();

    let collisions = find_collisions(&destinations);
    if collisions.is_empty() {
        return Ok(());
    }
    let detail = collisions
        .iter()
        .map(|group| {
            group
                .names
                .iter()
                .map(RefName::as_str)
                .collect::<Vec<_>>()
                .join(" and ")
        })
        .collect::<Vec<_>>()
        .join("; ");
    Err(CaptureError::RefCollision {
        repo: source.display().to_string(),
        detail,
    })
}

/// `refs/*` already carried `refs/stash`, which is the top entry. The rest of the
/// stack lives in a reflog that fetch never transfers, so each is fetched by object
/// id - which works over local transport even though nothing references them.
///
/// They land under `stashes/` rather than `stash/`: `refs/tycho/<key>/stash` is
/// already a leaf ref, and git's ref store refuses to hold a leaf and a directory of
/// the same name.
fn capture_stashes(store: &Store, git: &Git<'_>, repo: &RepoRoot) -> Contribution {
    let mut contribution = Contribution::default();
    for index in 0.. {
        let entry = format!("stash@{{{index}}}");
        let Ok(out) = git.run(
            &["rev-parse", "--verify", "--quiet", &entry],
            Timeout::QUICK,
        ) else {
            break;
        };
        if !out.status.success() {
            break;
        }
        let Ok(oid) = Oid::parse(String::from_utf8_lossy(&out.stdout).trim()) else {
            break;
        };
        let spec = Refspec::forced(
            &oid.to_string(),
            &format!("refs/tycho/{}/stashes/{index}", repo.key),
        );
        if let Err(error) = store.repo().fetch_refs(repo.source.as_path(), &spec) {
            contribution
                .warnings
                .push(format!("{}: {entry}: {error}", repo.key));
            break;
        }
    }
    contribution
}

/// The part git cannot restore from history: uncommitted, untracked and gitignored
/// files, filtered through the profile's rule tree.
fn overlay(source: &Path, repo: &RepoRoot, rules: &RuleTree, nested: &[AbsPath]) -> Contribution {
    let mut contribution = Contribution::default();
    let Ok(out) = Git::at(source).run(
        &[
            "status",
            "--porcelain=v1",
            "-z",
            "--untracked-files=all",
            "--ignored=traditional",
        ],
        Timeout::WORK,
    ) else {
        contribution
            .warnings
            .push(format!("{}: could not read status", repo.key));
        return contribution;
    };

    // `Git::run` hands back the output whatever the exit status, so the guard above
    // only catches a process that could not start. A `git status` that ran and failed
    // - a half-synced cloud folder, a transient read error on the removable drives
    // this is aimed at - would otherwise fall through to parsing empty stdout and
    // return an empty overlay with no warning at all. The overlay is the whole reason
    // this project exists, so a silent one is the failure it is built to prevent.
    if !out.status.success() {
        contribution.warnings.push(format!(
            "{}: git status failed, so no uncommitted or gitignored file was captured \
             from it: {}",
            repo.key,
            String::from_utf8_lossy(&out.stderr).trim()
        ));
        return contribution;
    }

    let mut fields = out
        .stdout
        .split(|&byte| byte == 0)
        .filter(|field| !field.is_empty());
    while let Some(record) = fields.next() {
        if record.len() < 4 {
            continue;
        }
        let (status, path) = record.split_at(3);
        // A rename record is followed by its old path, which is not a second entry.
        if status.starts_with(b"R") || status.starts_with(b"C") {
            let _ = fields.next();
        }
        let Some(relative) = crate::primitives::encode::path_from_git(path) else {
            contribution.warnings.push(format!(
                "{}: git named a path this platform cannot represent: {}",
                source.display(),
                String::from_utf8_lossy(path)
            ));
            continue;
        };
        let full = source.join(relative);
        if path.ends_with(b"/") {
            expand(&mut contribution, &full, source, repo, rules, nested);
        } else {
            take(&mut contribution, &full, source, repo, rules);
        }
    }
    contribution
}

/// Git reports an untracked directory - including an untracked nested repository -
/// as one collapsed entry and does not descend into it. Copying that wholesale would
/// put the nested repository's `.git` into the store tree as loose files, so Tycho
/// walks it itself and stops at any repository root the plan already found.
fn expand(
    contribution: &mut Contribution,
    dir: &Path,
    source: &Path,
    repo: &RepoRoot,
    rules: &RuleTree,
    nested: &[AbsPath],
) {
    let mut stack = vec![dir.to_path_buf()];
    while let Some(current) = stack.pop() {
        if nested.iter().any(|root| root.as_path() == current) {
            continue;
        }
        let Ok(listing) = std::fs::read_dir(&current) else {
            contribution.warnings.push(format!(
                "{}: could not read {}",
                repo.key,
                current.display()
            ));
            continue;
        };
        for item in listing.flatten() {
            let path = item.path();
            if has_git_component(&path) {
                continue;
            }
            match classify_path(&path) {
                Ok(FileKind::Directory) => {
                    if rules.captures(&path) || rules.may_contain_captures(&path) {
                        stack.push(path);
                    }
                }
                Ok(_) => take(contribution, &path, source, repo, rules),
                Err(_) => contribution.warnings.push(format!(
                    "{}: could not read {}",
                    repo.key,
                    path.display()
                )),
            }
        }
    }
}

fn take(
    contribution: &mut Contribution,
    path: &Path,
    source: &Path,
    repo: &RepoRoot,
    rules: &RuleTree,
) {
    // `--ignored` reports every gitignored path, which on this machine includes tens
    // of gigabytes of build output. This filter is the only reason the overlay does
    // not swallow it.
    if !rules.captures(path) {
        return;
    }
    let Ok(kind) = classify_path(path) else {
        return;
    };
    if kind.mode().is_none() {
        return;
    }
    let Ok(relative) = path.strip_prefix(source) else {
        return;
    };
    let mut joined = OsString::from(format!(".tycho/repos/{}/overlay/", repo.key));
    joined.push(relative.as_os_str());

    match (
        TreePath::parse(Path::new(&joined)),
        AbsPath::from_absolute(path),
    ) {
        (Ok(stored), Ok(source)) => contribution.files.push((source, stored)),
        (Err(error), _) => contribution
            .warnings
            .push(format!("{}: {error}", path.display())),
        (_, Err(error)) => contribution
            .warnings
            .push(format!("{}: {error}", path.display())),
    }
}

/// One screen of plain text, carrying the branch list because nothing is ever pruned:
/// liveness is recorded here rather than by a ref disappearing.
fn repo_txt(git: &Git<'_>, inspection: &Inspection, source: &Path) -> String {
    let field = |args: &[&str]| {
        git.run(args, Timeout::QUICK)
            .ok()
            .filter(|out| out.status.success())
            .map(|out| String::from_utf8_lossy(&out.stdout).trim().to_owned())
            .unwrap_or_default()
    };
    let list = |args: &[&str]| {
        let text = field(args);
        let names: Vec<&str> = text
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .collect();
        if names.is_empty() {
            "none".to_owned()
        } else {
            names.join(", ")
        }
    };

    let origin = field(&["config", "--get", "remote.origin.url"]);
    let head = match &inspection.head {
        RepoHead::Branch { name, sha } => format!("{name} @ {}", sha.short()),
        RepoHead::Detached { sha } => format!("detached @ {}", sha.short()),
        RepoHead::Unborn => "unborn".to_owned(),
    };
    let stashes = field(&["stash", "list"])
        .lines()
        .filter(|line| !line.is_empty())
        .count();

    format!(
        "origin    {}\npath      {}\nhead      {head}\nstate     {}\nbranches  {}\ntags      {}\nstash     {stashes} entries\nseen      {}\n",
        if origin.is_empty() { "none" } else { &origin },
        source.display(),
        inspection.state(),
        list(&["for-each-ref", "--format=%(refname:short)", "refs/heads"]),
        list(&["for-each-ref", "--format=%(refname:short)", "refs/tags"]),
        jiff::Timestamp::now()
            .to_zoned(jiff::tz::TimeZone::UTC)
            .strftime("%F %H:%M UTC"),
    )
}

/// What a captured repository's `REPO.txt` records, read back.
///
/// **This is the only record of which branch was checked out that survives reaching a
/// remote.** The obvious alternative - a symref under `refs/tycho/<key>/` - does not
/// work, because push and fetch carry a symref's resolved value and drop its symbolic
/// nature, so the branch *name* would never leave the machine. A restore reads from a
/// remote, so it reads this.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Recorded {
    /// What to check out. `None` for a repository that had no commits.
    pub head: Option<String>,
    pub stashes: usize,
    pub origin: Option<String>,
    pub path: Option<String>,
}

/// Reads what `repo_txt` wrote.
///
/// Writer and reader are pinned to each other by a round-trip test rather than by the
/// format holding still on its own.
#[must_use]
pub fn parse_repo_txt(text: &str) -> Recorded {
    let mut found = Recorded::default();
    for line in text.lines() {
        let Some((field, value)) = line.split_once(char::is_whitespace) else {
            continue;
        };
        let value = value.trim();
        match field {
            // `main @ de4c69c`, `detached @ de4c69c`, or `unborn`. The short sha is
            // enough: git resolves an unambiguous prefix, and a branch name is what a
            // restore wants anyway.
            "head" => {
                found.head = match value.split_once(" @ ") {
                    Some(("detached", sha)) => Some(sha.trim().to_owned()),
                    Some((branch, _)) => Some(branch.trim().to_owned()),
                    None => None,
                };
            }
            "stash" => {
                found.stashes = value
                    .split_whitespace()
                    .next()
                    .and_then(|count| count.parse().ok())
                    .unwrap_or_default();
            }
            "origin" if value != "none" => found.origin = Some(value.to_owned()),
            "path" => found.path = Some(value.to_owned()),
            _ => {}
        }
    }
    found
}

fn tree_path(text: &str) -> Result<TreePath, CaptureError> {
    TreePath::parse(Path::new(text)).map_err(|error| CaptureError::NotStorable {
        path: text.to_owned(),
        reason: error.to_string(),
    })
}

/// Every path the plan found as a repository root, for the overlay expansion to stop
/// at.
#[must_use]
pub fn nested_roots(plan: &Plan) -> Vec<AbsPath> {
    plan.roots
        .iter()
        .flat_map(RootPlan::repos)
        .map(|repo| repo.source.clone())
        .collect()
}

/// `count-objects -v` reports `size` and `size-pack` in kibibytes.
fn object_bytes(git: &Git<'_>) -> Result<u64, RunError> {
    let out = git.run(&["count-objects", "-v"], Timeout::QUICK)?;
    if !out.status.success() {
        return Ok(0);
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let mut total = 0;
    for line in text.lines() {
        if let Some(value) = line
            .strip_prefix("size: ")
            .or_else(|| line.strip_prefix("size-pack: "))
            && let Ok(kib) = value.trim().parse::<u64>()
        {
            total += kib * 1024;
        }
    }
    Ok(total)
}

#[cfg(test)]
mod repo_txt_tests {
    use super::{Inspection, parse_repo_txt, repo_txt};
    use crate::plan::RepoHead;
    use crate::primitives::names::BranchName;
    use crate::primitives::oid::Oid;
    use crate::sys::process::{Git, Timeout};
    use std::path::Path;

    fn sha() -> Oid {
        Oid::parse("de4c69c206bebbb88fab211f6378462444d3c8ae").expect("a sha")
    }

    fn inspection(head: RepoHead) -> Inspection {
        Inspection {
            head,
            modified: 0,
            untracked: 0,
            objects: 0,
        }
    }

    /// Writer and reader pinned to each other. `REPO.txt` is the only record of which
    /// branch was checked out that survives reaching a remote, so a format drift that
    /// only the reader noticed would be a restore checking out the wrong thing.
    fn round_trip(head: RepoHead) -> super::Recorded {
        let dir = tempfile::tempdir().expect("temp dir");
        let git = Git::at(dir.path());
        let _ = git.run(&["init", "-q"], Timeout::QUICK);
        let text = repo_txt(&git, &inspection(head), Path::new("/somewhere/proj"));
        parse_repo_txt(&text)
    }

    #[test]
    fn a_branch_head_reads_back_as_its_branch_name() {
        let head = RepoHead::Branch {
            name: BranchName::parse("main").expect("a branch"),
            sha: sha(),
        };
        assert_eq!(round_trip(head).head.as_deref(), Some("main"));
    }

    /// A detached HEAD has no branch to check out, so the sha is what comes back.
    #[test]
    fn a_detached_head_reads_back_as_its_sha() {
        let recorded = round_trip(RepoHead::Detached { sha: sha() });
        assert_eq!(recorded.head.as_deref(), Some(sha().short().as_str()));
    }

    /// A repository with no commits has nothing to check out, and that is not an
    /// error - it is a repository somebody had just created.
    #[test]
    fn an_unborn_head_reads_back_as_nothing_to_check_out() {
        assert_eq!(round_trip(RepoHead::Unborn).head, None);
    }

    #[test]
    fn the_other_fields_survive_the_round_trip() {
        let recorded = round_trip(RepoHead::Unborn);
        assert_eq!(recorded.path.as_deref(), Some("/somewhere/proj"));
        assert_eq!(recorded.stashes, 0);
        assert_eq!(recorded.origin, None, "'none' is absence, not a URL");
    }

    #[test]
    fn a_recorded_stash_count_is_read_as_a_number() {
        let recorded = parse_repo_txt("head      main @ abc1234\nstash     3 entries\n");
        assert_eq!(recorded.stashes, 3);
        assert_eq!(recorded.head.as_deref(), Some("main"));
    }
}
