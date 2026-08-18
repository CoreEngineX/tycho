//! The rule tree: one algorithm, deepest match wins, ties broken by tier.
//!
//! `config.md` section 5 exists because an earlier draft specified two algorithms
//! that disagreed. Its eight-row truth table is the specification and the test suite.

use crate::primitives::path::AbsPath;
use globset::{Candidate, Glob, GlobSet, GlobSetBuilder};
use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsStr;
use std::ops::Bound;
use std::path::Path;

/// One default ignore. A `Name` is a whole path component compared for equality, so
/// `out` cannot reach `output`; a `Glob` is a pattern matched against one component.
/// Which of the two an entry is belongs in the type, because an entry that was meant
/// as a name and silently behaved as a pattern would take files nobody named.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Junk {
    Name(&'static str),
    Glob(&'static str),
}

/// Writes the list as its two groups rather than as thirty wrapped constructors.
macro_rules! junk {
    (names: $($name:literal),+ ; globs: $($glob:literal),+ $(,)?) => {
        &[$(Junk::Name($name),)+ $(Junk::Glob($glob),)+]
    };
}

/// Applied unless `use_default_ignores = false`. Load-bearing rather than cosmetic:
/// the global cargo target directory on the first machine is 38 GB, and committing
/// it once puts it in history permanently.
pub const DEFAULT_JUNK: &[Junk] = junk! {
    names:
        "node_modules", "target", "build", ".build", "dist", "out",
        ".next", ".nuxt", ".svelte-kit", "DerivedData", ".gradle", ".kotlin",
        "Pods", "__pycache__", ".venv", "venv", ".cache",
        ".ruff_cache", ".pytest_cache", ".mypy_cache", ".ipynb_checkpoints",
        ".DS_Store", "Thumbs.db", "xcuserdata";
    globs:
        "*.o", "*.pyc", "*.class", "*.xcuserstate",
};

const _: () = names_are_not_patterns(DEFAULT_JUNK);

/// A name carrying a glob metacharacter would read as strict and match like a
/// pattern, and one carrying a separator is two components and so matches nothing.
/// Both are the failure this list cannot afford, so both are compile errors.
const fn names_are_not_patterns(list: &[Junk]) {
    let mut entry = 0;
    while entry < list.len() {
        if let Junk::Name(name) = list[entry] {
            assert!(!name.is_empty(), "a junk name cannot be empty");
            let bytes = name.as_bytes();
            let mut byte = 0;
            while byte < bytes.len() {
                assert!(
                    !matches!(
                        bytes[byte],
                        b'*' | b'?' | b'[' | b']' | b'{' | b'}' | b'/' | b'\\'
                    ),
                    "a junk name is one path component compared for equality, \
                     so it cannot contain a glob metacharacter or a separator"
                );
                byte += 1;
            }
        }
        entry += 1;
    }
}

/// Tier, strongest last. A tie at equal depth is broken by this and nothing else.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Tier {
    Junk,
    Glob,
    ExplicitPath,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Verdict {
    Capture,
    Skip,
}

/// Where a rule came from, and how much authority that gives it when two rules
/// name the same path at the same tier and depth. Ordered: junk loses to a local
/// file, a deeper local file beats a shallower one, and the operator's own config
/// beats every local file, so a repository's rules can always be overridden
/// without editing the repository.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Origin {
    Junk,
    /// Depth of the directory holding the `.tycho` that declared the rule.
    Local(usize),
    Global,
}

/// One rule's identity in the tree's arena, so a `Decision` costs a copy rather
/// than a clone of the rule's text in the hottest loop in the program.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct RuleId(u32);

/// What an id resolves to when a person needs to read the rule.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RuleMeta {
    /// As written in its file, not as expanded.
    pub text: String,
    pub source: Source,
}

/// The file a rule was written in. A `Local` carries the line so `rules explain`
/// can point at it; the junk list is compiled in and has no file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Source {
    Junk,
    Global,
    Local { file: AbsPath, line: u32 },
}

/// Which rule decided, and how deep it matched. Returned rather than a bare verdict
/// so `--dry-run` can name the rule that excluded a path and `config check` can
/// report a rule that matched nothing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Decision {
    pub verdict: Verdict,
    pub tier: Tier,
    pub depth: usize,
    pub origin: Origin,
    /// `None` when no rule matched anywhere along the path.
    pub rule: Option<RuleId>,
}

impl Decision {
    /// Resolution's starting point: nothing has matched, so the verdict is skip.
    pub const NONE: Self = Self {
        verdict: Verdict::Skip,
        tier: Tier::Junk,
        depth: 0,
        origin: Origin::Junk,
        rule: None,
    };
}

#[derive(Debug, thiserror::Error)]
pub enum RuleError {
    #[error("'{pattern}' is not a valid glob: {source}")]
    BadGlob {
        pattern: String,
        #[source]
        source: globset::Error,
    },
}

/// Explicit paths resolve by lookup, globs by two batch matchers. The doc calls this
/// a tree; the semantics are a rule set with depth resolution, and a trie whose only
/// justification was its name would be a worse thing to maintain.
#[derive(Debug)]
pub struct RuleTree {
    explicit: BTreeMap<AbsPath, (Verdict, RuleId, Origin)>,
    globs: GlobSet,
    glob_ids: Vec<RuleId>,
    junk_names: BTreeMap<&'static str, RuleId>,
    junk_globs: GlobSet,
    junk_glob_ids: Vec<RuleId>,
    local_globs: Vec<LocalGlobSet>,
    metas: Vec<RuleMeta>,
}

/// One local file's compiled globs. The patterns are anchored to the declaring
/// directory when they are built, so matching needs no base check here.
#[derive(Debug)]
struct LocalGlobSet {
    /// The declaring directory's depth, which is the rules' origin.
    depth: usize,
    set: GlobSet,
    ids: Vec<RuleId>,
}

/// The inputs, already expanded and validated by layer 0.
#[derive(Debug, Default)]
pub struct RuleSet {
    pub watch: Vec<AbsPath>,
    pub ignore_paths: Vec<AbsPath>,
    pub reinclude: Vec<AbsPath>,
    pub ignore_globs: Vec<String>,
    pub junk: &'static [Junk],
}

impl RuleTree {
    /// # Errors
    ///
    /// If a glob or junk pattern does not compile.
    pub fn build(rules: &RuleSet) -> Result<Self, RuleError> {
        let mut metas = Vec::new();
        let mut explicit = BTreeMap::new();
        for path in &rules.watch {
            let id = intern(&mut metas, path.to_string(), Source::Global);
            explicit.insert(path.clone(), (Verdict::Capture, id, Origin::Global));
        }
        for path in &rules.reinclude {
            let id = intern(&mut metas, path.to_string(), Source::Global);
            explicit.insert(path.clone(), (Verdict::Capture, id, Origin::Global));
        }
        // Last wins only if the same path appears twice with different verdicts,
        // which `check` reports as an error before this is ever built.
        for path in &rules.ignore_paths {
            let id = intern(&mut metas, path.to_string(), Source::Global);
            explicit.insert(path.clone(), (Verdict::Skip, id, Origin::Global));
        }

        let mut junk_names = BTreeMap::new();
        let mut junk_glob_patterns = Vec::new();
        let mut junk_glob_ids = Vec::new();
        for entry in rules.junk {
            match *entry {
                Junk::Name(name) => {
                    junk_names.insert(name, intern(&mut metas, name.to_owned(), Source::Junk));
                }
                Junk::Glob(pattern) => {
                    junk_glob_patterns.push(pattern);
                    junk_glob_ids.push(intern(&mut metas, pattern.to_owned(), Source::Junk));
                }
            }
        }
        let glob_ids = rules
            .ignore_globs
            .iter()
            .map(|pattern| intern(&mut metas, pattern.clone(), Source::Global))
            .collect();

        Ok(Self {
            explicit,
            globs: compile(&rules.ignore_globs)?,
            glob_ids,
            junk_names,
            junk_globs: compile(&junk_glob_patterns)?,
            junk_glob_ids,
            local_globs: Vec::new(),
            metas,
        })
    }

    /// Folds one local rule file into the tree. `base` is the directory holding
    /// the `.tycho`; `file` is the rules file itself, recorded as each rule's
    /// source so `--dry-run` and `rules explain` can point at it.
    ///
    /// # Errors
    ///
    /// If a glob does not compile.
    pub fn add_local(
        &mut self,
        base: &AbsPath,
        file: &AbsPath,
        rules: &crate::config::local::LocalRules,
    ) -> Result<(), RuleError> {
        let depth = base.as_path().components().count();
        let origin = Origin::Local(depth);
        for (entries, verdict) in [
            (&rules.ignore, Verdict::Skip),
            (&rules.reinclude, Verdict::Capture),
        ] {
            for entry in entries {
                let source = Source::Local {
                    file: file.clone(),
                    line: entry.line,
                };
                let id = intern(&mut self.metas, entry.text.clone(), source);
                // The higher origin keeps a contested path: the operator's config
                // outranks any local file, and a deeper local file outranks a
                // shallower one.
                let keep = self
                    .explicit
                    .get(entry.path.as_path())
                    .is_none_or(|(_, _, existing)| origin > *existing);
                if keep {
                    self.explicit
                        .insert(entry.path.clone(), (verdict, id, origin));
                }
            }
        }
        if !rules.globs.is_empty() {
            let mut ids = Vec::new();
            let mut patterns = Vec::new();
            for glob in &rules.globs {
                let source = Source::Local {
                    file: file.clone(),
                    line: glob.line,
                };
                ids.push(intern(&mut self.metas, glob.text.clone(), source));
                patterns.push(anchored(base, &glob.text));
            }
            let set = compile(&patterns)?;
            self.local_globs.push(LocalGlobSet { depth, set, ids });
        }
        Ok(())
    }

    /// Evaluates every rule against the path and each of its ancestors, and returns
    /// the deepest match, ties broken by tier then origin.
    ///
    /// A path no rule matches is skipped. The walk starts at watched roots so that
    /// should not arise, but the function is total either way.
    #[must_use]
    pub fn resolve(&self, path: &Path) -> Decision {
        let mut best = Decision::NONE;
        let mut prefix = std::path::PathBuf::new();
        for (index, component) in path.components().enumerate() {
            prefix.push(component);
            best = self.step(best, &prefix, component.as_os_str(), index + 1);
        }
        best
    }

    /// One resolution step: the rules matching `child` itself, weighed against the
    /// best its parent's chain already produced. The walk carries each directory's
    /// decision down its stack and pays one step per entry; [`Self::resolve`] is
    /// the fold of this over a path's components, so the two cannot disagree.
    #[must_use]
    pub fn descend(&self, parent: Decision, child: &Path, depth: usize) -> Decision {
        let component = child.file_name().unwrap_or_default();
        self.step(parent, child, component, depth)
    }

    fn step(&self, mut best: Decision, prefix: &Path, component: &OsStr, depth: usize) -> Decision {
        if let Some((verdict, id, origin)) = self.explicit.get(prefix) {
            consider(
                &mut best,
                Decision {
                    verdict: *verdict,
                    tier: Tier::ExplicitPath,
                    depth,
                    origin: *origin,
                    rule: Some(*id),
                },
            );
        }
        if let Some(name) = component.to_str()
            && let Some(id) = self.junk_names.get(name)
        {
            consider(
                &mut best,
                Decision {
                    verdict: Verdict::Skip,
                    tier: Tier::Junk,
                    depth,
                    origin: Origin::Junk,
                    rule: Some(*id),
                },
            );
        }
        let candidate = Candidate::new(prefix);
        if let Some(id) = matched(&self.globs, &self.glob_ids, &candidate) {
            consider(
                &mut best,
                Decision {
                    verdict: Verdict::Skip,
                    tier: Tier::Glob,
                    depth,
                    origin: Origin::Global,
                    rule: Some(id),
                },
            );
        }
        if let Some(id) = matched(&self.junk_globs, &self.junk_glob_ids, &candidate) {
            consider(
                &mut best,
                Decision {
                    verdict: Verdict::Skip,
                    tier: Tier::Junk,
                    depth,
                    origin: Origin::Junk,
                    rule: Some(id),
                },
            );
        }
        for local in &self.local_globs {
            if let Some(id) = matched(&local.set, &local.ids, &candidate) {
                consider(
                    &mut best,
                    Decision {
                        verdict: Verdict::Skip,
                        tier: Tier::Glob,
                        depth,
                        origin: Origin::Local(local.depth),
                        rule: Some(id),
                    },
                );
            }
        }
        best
    }

    /// Whether a path is captured. The whole of the rule tree from a caller's view.
    #[must_use]
    pub fn captures(&self, path: &Path) -> bool {
        self.resolve(path).verdict == Verdict::Capture
    }

    /// Whether anything beneath `dir` could still be captured, which is the only
    /// safe basis for pruning a walk.
    ///
    /// A walk that stops descending wherever the verdict is `Skip` silently breaks
    /// re-inclusion: `ignore ~/A/s` with `reinclude ~/A/s/keep` is the documented
    /// carve-out, and pruning at `~/A/s` means `keep` is never reached. Globs and
    /// junk are ignores at every depth, so only an explicit capture rule below the
    /// directory can rescue anything.
    #[must_use]
    pub fn may_contain_captures(&self, dir: &Path) -> bool {
        self.explicit
            .range::<Path, _>((Bound::Included(dir), Bound::Unbounded))
            .take_while(|(path, _)| path.as_path().starts_with(dir))
            .any(|(_, (verdict, _, _))| *verdict == Verdict::Capture)
    }

    /// Records every glob and junk pattern that matched anywhere along the path, so
    /// `--dry-run` can list the rules that matched nothing at all - the row that
    /// earns that command, since a typo'd rule is otherwise a silent no-op.
    pub fn record_hits(&self, path: &Path, hits: &mut Hits) {
        let mut prefix = std::path::PathBuf::new();
        for component in path.components() {
            prefix.push(component);
            self.record_hit(&prefix, hits);
        }
    }

    /// [`Self::record_hits`] for one path whose ancestors are already recorded. The
    /// walk records each entry it lists, so an entry's chain is covered by its
    /// parents' own recordings plus this.
    pub fn record_hit(&self, path: &Path, hits: &mut Hits) {
        let candidate = Candidate::new(path);
        for index in self.globs.matches_candidate(&candidate) {
            hits.globs.insert(index);
        }
        for local in &self.local_globs {
            for index in local.set.matches_candidate(&candidate) {
                hits.local.insert(local.ids[index]);
            }
        }
    }

    /// Every local glob rule's id, for the unfired-rule report.
    pub fn local_glob_ids(&self) -> impl Iterator<Item = RuleId> {
        self.local_globs
            .iter()
            .flat_map(|local| local.ids.iter().copied())
    }

    /// The global glob patterns in hit-index order, for the unfired-rule report.
    pub fn glob_patterns(&self) -> impl Iterator<Item = &str> {
        self.glob_ids.iter().map(|id| self.rule_text(*id))
    }

    /// The rule's text as a person wrote it.
    #[must_use]
    pub fn rule_text(&self, id: RuleId) -> &str {
        &self.metas[id.0 as usize].text
    }

    /// Which file the rule came from.
    #[must_use]
    pub fn rule_source(&self, id: RuleId) -> &Source {
        &self.metas[id.0 as usize].source
    }
}

/// Which of the rules a person wrote fired during a walk. The junk list is not
/// tracked: `unfired` excludes it on purpose, so recording it would be state with
/// no reader.
#[derive(Debug, Default)]
pub struct Hits {
    pub globs: BTreeSet<usize>,
    /// Local glob rules that fired, by id.
    pub local: BTreeSet<RuleId>,
}

fn matched(matcher: &GlobSet, ids: &[RuleId], candidate: &Candidate<'_>) -> Option<RuleId> {
    let index = *matcher.matches_candidate(candidate).first()?;
    Some(ids[index])
}

fn intern(metas: &mut Vec<RuleMeta>, text: String, source: Source) -> RuleId {
    metas.push(RuleMeta { text, source });
    RuleId((metas.len() - 1) as u32)
}

fn consider(best: &mut Decision, candidate: Decision) {
    if candidate.depth > best.depth
        || (candidate.depth == best.depth && candidate.tier > best.tier)
        || (candidate.depth == best.depth
            && candidate.tier == best.tier
            && candidate.origin > best.origin)
    {
        *best = candidate;
    }
}

/// A local glob rooted at its declaring directory: a bare basename pattern
/// matches at any depth beneath it, a pattern with `/` sits directly under it -
/// `compile`'s convention, rooted at the base instead of anywhere. The base is
/// escaped so a directory named `foo[1]` stays a literal.
fn anchored(base: &AbsPath, pattern: &str) -> String {
    // Candidates are matched with `/` separators on every platform, so the base
    // must use them too. Only Windows rewrites: a Unix filename may legally
    // contain `\`, and rewriting it would corrupt the path to fix nothing.
    #[cfg(windows)]
    let base = globset::escape(&base.as_path().to_string_lossy().replace('\\', "/"));
    #[cfg(unix)]
    let base = globset::escape(&base.as_path().to_string_lossy());
    if pattern.contains('/') {
        format!("{base}/{pattern}")
    } else {
        format!("{base}/**/{pattern}")
    }
}

/// A pattern with no separator matches a basename at any level, so it is anchored
/// with `**/`. That is what makes `*.log` match `~/A/s/keep/a.log` at the file's own
/// depth rather than not at all - truth table row 8.
fn compile<S: AsRef<str>>(patterns: &[S]) -> Result<GlobSet, RuleError> {
    let mut builder = GlobSetBuilder::new();
    for pattern in patterns {
        let pattern = pattern.as_ref();
        let anchored = if pattern.contains('/') {
            pattern.to_owned()
        } else {
            format!("**/{pattern}")
        };
        let glob = Glob::new(&anchored).map_err(|source| RuleError::BadGlob {
            pattern: pattern.to_owned(),
            source,
        })?;
        builder.add(glob);
    }
    builder.build().map_err(|source| RuleError::BadGlob {
        pattern: patterns
            .iter()
            .map(AsRef::as_ref)
            .collect::<Vec<_>>()
            .join(", "),
        source,
    })
}

#[cfg(test)]
mod tests {
    use super::{DEFAULT_JUNK, Junk, RuleSet, RuleTree, Tier, Verdict};
    use crate::primitives::path::AbsPath;
    use std::path::Path;

    /// A path is only absolute on Windows with a drive prefix, so the fixture home
    /// carries one. It adds a component on that platform and nothing else: the
    /// algorithm compares component counts of prefixes of one candidate, so a
    /// constant offset cannot reorder them.
    #[cfg(unix)]
    const HOME: &str = "/h";
    #[cfg(windows)]
    const HOME: &str = r"C:\h";

    /// The truth table uses `~` for readability.
    fn home(rest: &str) -> AbsPath {
        AbsPath::parse_with(&format!("~/{rest}"), Some(Path::new(HOME)), |_| None)
            .expect("a valid path")
    }

    fn paths(rest: &[&str]) -> Vec<AbsPath> {
        rest.iter().map(|item| home(item)).collect()
    }

    fn tree(rules: &RuleSet) -> RuleTree {
        RuleTree::build(rules).expect("the patterns compile")
    }

    fn captured(tree: &RuleTree, candidate: &str) -> bool {
        tree.captures(home(candidate).as_path())
    }

    /// Row 1: watch `~/A` (d1), candidate `~/A/x.md` (d2). Winner: watch, d1.
    #[test]
    fn row_1_a_watched_root_captures_what_is_under_it() {
        let tree = tree(&RuleSet {
            watch: paths(&["A"]),
            ..RuleSet::default()
        });
        assert!(captured(&tree, "A/x.md"));
    }

    /// Row 2: + ignore `~/A/s` (d2), candidate `~/A/s/t.bin`. Winner: ignore, d2.
    #[test]
    fn row_2_a_deeper_ignore_beats_the_watch_above_it() {
        let tree = tree(&RuleSet {
            watch: paths(&["A"]),
            ignore_paths: paths(&["A/s"]),
            ..RuleSet::default()
        });
        assert!(!captured(&tree, "A/s/t.bin"));
    }

    /// Row 3: + reinclude `~/A/s/keep` (d3), candidate `~/A/s/keep/k.pem`.
    #[test]
    fn row_3_a_deeper_reinclude_beats_the_ignore_above_it() {
        let tree = tree(&RuleSet {
            watch: paths(&["A"]),
            ignore_paths: paths(&["A/s"]),
            reinclude: paths(&["A/s/keep"]),
            ..RuleSet::default()
        });
        assert!(captured(&tree, "A/s/keep/k.pem"));
    }

    /// Row 4: watch `~/A` (d1), junk `target`, candidate `~/A/p/target/x.o`.
    #[test]
    fn row_4_junk_matches_at_the_depth_of_the_component_it_names() {
        let tree = tree(&RuleSet {
            watch: paths(&["A"]),
            junk: &[Junk::Name("target")],
            ..RuleSet::default()
        });
        let decision = tree.resolve(home("A/p/target/x.o").as_path());
        assert_eq!(decision.verdict, Verdict::Skip);
        assert_eq!(decision.tier, Tier::Junk);
        assert_eq!(
            tree.rule_text(decision.rule.expect("a junk rule fired")),
            "target"
        );
    }

    /// Row 5: + reinclude `~/A/p/target` (d3). Reinclude beats junk at equal depth.
    #[test]
    fn row_5_a_reinclude_beats_junk_at_the_same_depth_by_tier() {
        let tree = tree(&RuleSet {
            watch: paths(&["A"]),
            reinclude: paths(&["A/p/target"]),
            junk: &[Junk::Name("target")],
            ..RuleSet::default()
        });
        assert!(captured(&tree, "A/p/target/x.o"));
    }

    /// Row 6: glob `**/*.xcarchive` matches `~/A/b/Foo.xcarchive` at d3.
    #[test]
    fn row_6_a_glob_ignores_what_it_matches() {
        let tree = tree(&RuleSet {
            watch: paths(&["A"]),
            ignore_globs: vec!["**/*.xcarchive".to_owned()],
            ..RuleSet::default()
        });
        assert!(!captured(&tree, "A/b/Foo.xcarchive"));
    }

    /// Row 7: + reinclude of the file itself (d3). Reinclude beats glob by tier.
    #[test]
    fn row_7_a_reinclude_beats_a_glob_at_the_same_depth_by_tier() {
        let tree = tree(&RuleSet {
            watch: paths(&["A"]),
            reinclude: paths(&["A/b/Foo.xcarchive"]),
            ignore_globs: vec!["**/*.xcarchive".to_owned()],
            ..RuleSet::default()
        });
        assert!(captured(&tree, "A/b/Foo.xcarchive"));
    }

    /// Row 8, the one people get wrong: glob `*.log` matches the filename at d4,
    /// which is deeper than the reinclude at d3.
    #[test]
    fn row_8_a_glob_on_a_filename_outranks_a_reincluded_directory() {
        let tree = tree(&RuleSet {
            watch: paths(&["A"]),
            ignore_paths: paths(&["A/s"]),
            reinclude: paths(&["A/s/keep"]),
            ignore_globs: vec!["*.log".to_owned()],
            ..RuleSet::default()
        });
        let decision = tree.resolve(home("A/s/keep/a.log").as_path());
        assert_eq!(decision.verdict, Verdict::Skip);
        assert_eq!(
            tree.rule_text(decision.rule.expect("the glob fired")),
            "*.log"
        );
        // The sibling that no glob matches is still captured.
        assert!(captured(&tree, "A/s/keep/a.md"));
        // And naming the file itself, at its own depth, is how you keep it.
        let tree = tree_with_file_reincluded();
        assert!(captured(&tree, "A/s/keep/a.log"));
    }

    fn tree_with_file_reincluded() -> RuleTree {
        tree(&RuleSet {
            watch: paths(&["A"]),
            ignore_paths: paths(&["A/s"]),
            reinclude: paths(&["A/s/keep", "A/s/keep/a.log"]),
            ignore_globs: vec!["*.log".to_owned()],
            ..RuleSet::default()
        })
    }

    /// Row 5 with the real junk list rather than the single rule the table names.
    /// `*.o` matches at d4 and beats the reinclude at d3, so the file stays skipped.
    /// That is row 8's principle applied consistently, and it is the question this
    /// design will be asked.
    #[test]
    fn a_reincluded_directory_does_not_rescue_files_a_deeper_junk_glob_matches() {
        let tree = tree(&RuleSet {
            watch: paths(&["A"]),
            reinclude: paths(&["A/p/target"]),
            junk: DEFAULT_JUNK,
            ..RuleSet::default()
        });
        let decision = tree.resolve(home("A/p/target/x.o").as_path());
        assert_eq!(decision.verdict, Verdict::Skip);
        assert_eq!(
            tree.rule_text(decision.rule.expect("a junk rule fired")),
            "*.o",
            "the deeper junk glob should win"
        );
        // A file the junk list does not name comes back.
        assert!(captured(&tree, "A/p/target/keep.txt"));
    }

    /// One algorithm from a caller's view, but not one rule: a name is compared to a
    /// whole component, a glob is matched against one. Both report themselves.
    #[test]
    fn a_junk_name_is_compared_whole_and_a_junk_glob_is_matched() {
        let tree = tree(&RuleSet {
            watch: paths(&["A"]),
            junk: &[Junk::Name("out"), Junk::Glob("*.o")],
            ..RuleSet::default()
        });
        let named = tree.resolve(home("A/out/x.js").as_path());
        assert_eq!(tree.rule_text(named.rule.expect("the name fired")), "out");
        let globbed = tree.resolve(home("A/src/x.o").as_path());
        assert_eq!(tree.rule_text(globbed.rule.expect("the glob fired")), "*.o");
        assert!(captured(&tree, "A/output/x.js"), "a name is not a prefix");
        assert!(
            captured(&tree, "A/src/x.object"),
            "a glob is not a substring"
        );
    }

    /// The junk list is the only list here whose failure mode is a file the user
    /// wanted and never finds again, so each entry has to be a name a tool owns
    /// rather than one a person would pick. The second half is the half that
    /// matters: near-misses on the same words stay captured.
    #[test]
    fn the_junk_list_names_tool_caches_and_not_the_words_around_them() {
        let tree = tree(&RuleSet {
            watch: paths(&["A"]),
            junk: DEFAULT_JUNK,
            ..RuleSet::default()
        });

        for junk in [
            "A/.ruff_cache/0.16.2/1234",
            "A/.pytest_cache/v/cache/lastfailed",
            "A/.mypy_cache/3.14/x.json",
            "A/.ipynb_checkpoints/train-checkpoint.ipynb",
            "A/p/.kotlin/sessions/x",
            "A/P.xcodeproj/xcuserdata/me.xcuserdatad/xcschemes/x.plist",
            "A/lib/.swiftpm/xcode/xcuserdata/me.xcuserdatad/x.plist",
            "A/web/out/_next/static/build/main.js",
            "A/jvm/out/production/app/Main.class",
        ] {
            assert!(!captured(&tree, junk), "{junk} should be junk");
        }

        for wanted in [
            "A/cache/notes.md",
            "A/ruff_cache_notes.md",
            "A/.ruff_cache_of_mine.txt",
            "A/checkpoints/model.pt",
            "A/xcuserdata.md",
            "A/kotlin/Main.kt",
            "A/output/report.md",
            "A/checkout/receipt.pdf",
            "A/notes/out.md",
        ] {
            assert!(captured(&tree, wanted), "{wanted} is the user's own file");
        }
    }

    /// The pruning question. Answering it with the verdict alone would stop the walk
    /// at `~/A/s` and lose the re-included subtree entirely.
    #[test]
    fn a_skipped_directory_still_reports_that_it_may_contain_captures() {
        let tree = tree(&RuleSet {
            watch: paths(&["A"]),
            ignore_paths: paths(&["A/s"]),
            reinclude: paths(&["A/s/keep"]),
            ..RuleSet::default()
        });
        let ignored = home("A/s");
        assert!(!captured(&tree, "A/s"), "the directory itself is skipped");
        assert!(
            tree.may_contain_captures(ignored.as_path()),
            "pruning here would lose the re-included subtree"
        );
        assert!(
            !tree.may_contain_captures(home("A/other").as_path()),
            "a skipped directory with nothing under it is prunable"
        );
    }

    #[test]
    fn hits_record_every_pattern_that_matched_anywhere() {
        let tree = tree(&RuleSet {
            watch: paths(&["A"]),
            ignore_globs: vec!["*.log".to_owned(), "*.never".to_owned()],
            junk: &[Junk::Name("target")],
            ..RuleSet::default()
        });
        let mut hits = super::Hits::default();
        tree.record_hits(home("A/p/target/a.log").as_path(), &mut hits);
        assert!(hits.globs.contains(&0), "*.log matched");
        assert!(!hits.globs.contains(&1), "*.never matched nothing");
    }

    fn add_local(tree: &mut RuleTree, base: &str, lists: Local<'_>) {
        use crate::config::local::{Entry, GlobEntry, LocalRules};
        let base_path = home(base);
        let file = home(&format!("{base}/.tycho/rules.toml"));
        let entry = |text: &&str| Entry {
            text: (*text).to_owned(),
            line: 1,
            path: home(&format!("{base}/{text}")),
        };
        let rules = LocalRules {
            ignore: lists.ignore.iter().map(entry).collect(),
            reinclude: lists.reinclude.iter().map(entry).collect(),
            globs: lists
                .globs
                .iter()
                .map(|text| GlobEntry {
                    text: (*text).to_owned(),
                    line: 1,
                })
                .collect(),
            unknown: Vec::new(),
        };
        tree.add_local(&base_path, &file, &rules)
            .expect("local rules compile");
    }

    #[derive(Clone, Copy, Default)]
    struct Local<'a> {
        ignore: &'a [&'a str],
        reinclude: &'a [&'a str],
        globs: &'a [&'a str],
    }

    /// Row 9: identical path, tier and depth from two sources - the operator's
    /// config wins, so a repository's own rules can always be overridden.
    #[test]
    fn row_9_a_global_rule_beats_a_local_rule_on_an_exact_tie() {
        let mut ignoring = tree(&RuleSet {
            watch: paths(&["A"]),
            ignore_paths: paths(&["A/proj/data"]),
            ..RuleSet::default()
        });
        add_local(
            &mut ignoring,
            "A/proj",
            Local {
                reinclude: &["data"],
                ..Local::default()
            },
        );
        assert!(
            !captured(&ignoring, "A/proj/data/model.bin"),
            "the ignore holds"
        );

        let mut keeping = tree(&RuleSet {
            watch: paths(&["A"]),
            reinclude: paths(&["A/proj/data"]),
            ..RuleSet::default()
        });
        add_local(
            &mut keeping,
            "A/proj",
            Local {
                ignore: &["data"],
                ..Local::default()
            },
        );
        assert!(
            captured(&keeping, "A/proj/data/model.bin"),
            "the reinclude holds"
        );
    }

    /// Two local files name the same path: the deeper file is closer to the data,
    /// so it wins - in whichever order discovery found the two.
    #[test]
    fn a_deeper_local_file_beats_a_shallower_one() {
        for deeper_first in [false, true] {
            let mut tree = tree(&RuleSet {
                watch: paths(&["A"]),
                ..RuleSet::default()
            });
            let shallow = Local {
                ignore: &["sub/data"],
                ..Local::default()
            };
            let deep = Local {
                reinclude: &["data"],
                ..Local::default()
            };
            if deeper_first {
                add_local(&mut tree, "A/sub", deep);
                add_local(&mut tree, "A", shallow);
            } else {
                add_local(&mut tree, "A", shallow);
                add_local(&mut tree, "A/sub", deep);
            }
            assert!(
                captured(&tree, "A/sub/data/keep.txt"),
                "deeper_first: {deeper_first}"
            );
        }
    }

    /// A local glob is scoped: it matches beneath its directory - including
    /// directly beneath it, the zero-component case of `**` - and nowhere else.
    #[test]
    fn a_local_glob_stays_inside_its_directory() {
        let mut tree = tree(&RuleSet {
            watch: paths(&["A"]),
            ..RuleSet::default()
        });
        add_local(
            &mut tree,
            "A/proj",
            Local {
                globs: &["*.ckpt"],
                ..Local::default()
            },
        );
        assert!(!captured(&tree, "A/proj/model.ckpt"), "directly beneath");
        assert!(!captured(&tree, "A/proj/deep/nest/model.ckpt"), "any depth");
        assert!(
            captured(&tree, "A/other/model.ckpt"),
            "a sibling is out of reach"
        );
        assert!(
            captured(&tree, "A/model.ckpt"),
            "the parent is out of reach"
        );
    }

    /// A directory named like a glob must stay a literal, or its rules would
    /// reach into siblings their author never named.
    #[test]
    fn a_base_with_metacharacters_is_matched_literally() {
        let mut tree = tree(&RuleSet {
            watch: paths(&["A"]),
            ..RuleSet::default()
        });
        add_local(
            &mut tree,
            "A/pro[1]ject",
            Local {
                globs: &["*.ckpt"],
                ..Local::default()
            },
        );
        assert!(!captured(&tree, "A/pro[1]ject/model.ckpt"));
        assert!(captured(&tree, "A/pro1ject/model.ckpt"), "no class match");
    }

    #[test]
    fn local_glob_hits_are_recorded_by_id() {
        let mut tree = tree(&RuleSet {
            watch: paths(&["A"]),
            ..RuleSet::default()
        });
        add_local(
            &mut tree,
            "A/proj",
            Local {
                globs: &["*.log", "*.never"],
                ..Local::default()
            },
        );
        let ids: Vec<_> = tree.local_glob_ids().collect();
        let mut hits = super::Hits::default();
        tree.record_hits(home("A/proj/x.log").as_path(), &mut hits);
        assert!(hits.local.contains(&ids[0]), "*.log matched");
        assert!(!hits.local.contains(&ids[1]), "*.never matched nothing");
    }

    /// The walk resolves by descending one step per entry; a divergence between
    /// the two would make what the walk keeps differ from what `capture` keeps.
    #[test]
    fn resolve_is_the_fold_of_descend() {
        let tree = tree(&RuleSet {
            watch: paths(&["A"]),
            ignore_paths: paths(&["A/s"]),
            reinclude: paths(&["A/s/keep"]),
            ignore_globs: vec!["*.log".to_owned()],
            junk: DEFAULT_JUNK,
        });
        for candidate in [
            "A/x.md",
            "A/s/t.bin",
            "A/s/keep/k.pem",
            "A/s/keep/a.log",
            "A/p/target/x.o",
            "B/y.md",
        ] {
            let path = home(candidate);
            let mut folded = super::Decision::NONE;
            let mut prefix = std::path::PathBuf::new();
            for (index, component) in path.as_path().components().enumerate() {
                prefix.push(component);
                folded = tree.descend(folded, &prefix, index + 1);
            }
            assert_eq!(folded, tree.resolve(path.as_path()), "{candidate}");
        }
    }

    #[test]
    fn a_path_no_rule_matches_is_skipped() {
        let tree = tree(&RuleSet {
            watch: paths(&["A"]),
            ..RuleSet::default()
        });
        assert!(!captured(&tree, "B/x.md"));
    }

    #[test]
    fn a_watch_at_a_shallow_depth_never_rescues_a_deeper_ignore() {
        let tree = tree(&RuleSet {
            watch: paths(&["A"]),
            junk: &[Junk::Name("node_modules")],
            ..RuleSet::default()
        });
        assert!(!captured(&tree, "A/b/c/d/e/f/node_modules/pkg/index.js"));
    }

    /// Unix only because a path that is not UTF-8 cannot exist on Windows: the
    /// filesystem stores UTF-16 and Rust round-trips it through WTF-8, so there is no
    /// name here for these bytes to be.
    #[cfg(unix)]
    #[test]
    fn a_glob_matches_a_path_that_is_not_utf_8() {
        use std::ffi::OsStr;
        use std::os::unix::ffi::OsStrExt;

        let tree = tree(&RuleSet {
            watch: paths(&["A"]),
            ignore_globs: vec!["*.log".to_owned()],
            ..RuleSet::default()
        });
        let hostile = OsStr::from_bytes(b"/h/A/caf\xff/a.log");
        assert!(!tree.captures(Path::new(hostile)));
        let kept = OsStr::from_bytes(b"/h/A/caf\xff/a.md");
        assert!(tree.captures(Path::new(kept)));
    }

    #[test]
    fn an_invalid_glob_is_reported_rather_than_ignored() {
        let error = RuleTree::build(&RuleSet {
            ignore_globs: vec!["[".to_owned()],
            ..RuleSet::default()
        })
        .expect_err("an unclosed class is not a glob");
        assert!(error.to_string().contains('['), "{error}");
    }
}
