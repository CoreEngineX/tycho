//! The rule tree: one algorithm, deepest match wins, ties broken by tier.
//!
//! `config.md` section 5 exists because an earlier draft specified two algorithms
//! that disagreed. Its eight-row truth table is the specification and the test suite.

use crate::primitives::path::AbsPath;
use globset::{Candidate, Glob, GlobSet, GlobSetBuilder};
use std::collections::{BTreeMap, BTreeSet};
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

/// Which rule decided, and how deep it matched. Returned rather than a bare verdict
/// so `--dry-run` can name the rule that excluded a path and `config check` can
/// report a rule that matched nothing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Decision {
    pub verdict: Verdict,
    pub tier: Tier,
    pub depth: usize,
    pub rule: String,
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
    explicit: BTreeMap<AbsPath, (Verdict, String)>,
    globs: GlobSet,
    glob_patterns: Vec<String>,
    junk_names: BTreeSet<&'static str>,
    junk_globs: GlobSet,
    junk_glob_patterns: Vec<&'static str>,
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
        let mut explicit = BTreeMap::new();
        for path in &rules.watch {
            explicit.insert(path.clone(), (Verdict::Capture, path.to_string()));
        }
        for path in &rules.reinclude {
            explicit.insert(path.clone(), (Verdict::Capture, path.to_string()));
        }
        // Last wins only if the same path appears twice with different verdicts,
        // which `check` reports as an error before this is ever built.
        for path in &rules.ignore_paths {
            explicit.insert(path.clone(), (Verdict::Skip, path.to_string()));
        }

        let mut junk_names = BTreeSet::new();
        let mut junk_glob_patterns = Vec::new();
        for entry in rules.junk {
            match *entry {
                Junk::Name(name) => {
                    junk_names.insert(name);
                }
                Junk::Glob(pattern) => junk_glob_patterns.push(pattern),
            }
        }

        Ok(Self {
            explicit,
            globs: compile(&rules.ignore_globs)?,
            glob_patterns: rules.ignore_globs.clone(),
            junk_names,
            junk_globs: compile(&junk_glob_patterns)?,
            junk_glob_patterns,
        })
    }

    /// Evaluates every rule against the path and each of its ancestors, and returns
    /// the deepest match, ties broken by tier.
    ///
    /// A path no rule matches is skipped. The walk starts at watched roots so that
    /// should not arise, but the function is total either way.
    #[must_use]
    pub fn resolve(&self, path: &Path) -> Decision {
        let mut best = Decision {
            verdict: Verdict::Skip,
            tier: Tier::Junk,
            depth: 0,
            rule: String::new(),
        };
        let mut prefix = std::path::PathBuf::new();

        for (index, component) in path.components().enumerate() {
            prefix.push(component);
            let depth = index + 1;
            let candidate = Candidate::new(&prefix);

            if let Some((verdict, rule)) = self.explicit.get(prefix.as_path()) {
                consider(
                    &mut best,
                    Decision {
                        verdict: *verdict,
                        tier: Tier::ExplicitPath,
                        depth,
                        rule: rule.clone(),
                    },
                );
            }
            if let Some(name) = component.as_os_str().to_str()
                && let Some(hit) = self.junk_names.get(name)
            {
                consider(
                    &mut best,
                    Decision {
                        verdict: Verdict::Skip,
                        tier: Tier::Junk,
                        depth,
                        rule: (*hit).to_owned(),
                    },
                );
            }
            if let Some(rule) = matched(&self.globs, &self.glob_patterns, &candidate) {
                consider(
                    &mut best,
                    Decision {
                        verdict: Verdict::Skip,
                        tier: Tier::Glob,
                        depth,
                        rule,
                    },
                );
            }
            if let Some(rule) = matched(&self.junk_globs, &self.junk_glob_patterns, &candidate) {
                consider(
                    &mut best,
                    Decision {
                        verdict: Verdict::Skip,
                        tier: Tier::Junk,
                        depth,
                        rule,
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
            .any(|(_, (verdict, _))| *verdict == Verdict::Capture)
    }

    /// Records every glob and junk pattern that matched anywhere along the path, so
    /// `--dry-run` can list the rules that matched nothing at all - the row that
    /// earns that command, since a typo'd rule is otherwise a silent no-op.
    pub fn record_hits(&self, path: &Path, hits: &mut Hits) {
        let mut prefix = std::path::PathBuf::new();
        for component in path.components() {
            prefix.push(component);
            let candidate = Candidate::new(&prefix);
            for index in self.globs.matches_candidate(&candidate) {
                hits.globs.insert(index);
            }
        }
    }

    #[must_use]
    pub fn glob_patterns(&self) -> &[String] {
        &self.glob_patterns
    }
}

/// Which of the rules a person wrote fired during a walk. The junk list is not
/// tracked: `unfired` excludes it on purpose, so recording it would be state with
/// no reader.
#[derive(Debug, Default)]
pub struct Hits {
    pub globs: BTreeSet<usize>,
}

fn matched<S: AsRef<str>>(
    matcher: &GlobSet,
    patterns: &[S],
    candidate: &Candidate<'_>,
) -> Option<String> {
    let index = *matcher.matches_candidate(candidate).first()?;
    Some(patterns[index].as_ref().to_owned())
}

fn consider(best: &mut Decision, candidate: Decision) {
    if candidate.depth > best.depth || (candidate.depth == best.depth && candidate.tier > best.tier)
    {
        *best = candidate;
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
        assert_eq!(decision.rule, "target");
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
        assert_eq!(decision.rule, "*.log");
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
        assert_eq!(decision.rule, "*.o", "the deeper junk glob should win");
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
        assert_eq!(tree.resolve(home("A/out/x.js").as_path()).rule, "out");
        assert_eq!(tree.resolve(home("A/src/x.o").as_path()).rule, "*.o");
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
