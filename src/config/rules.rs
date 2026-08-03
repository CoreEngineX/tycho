//! The rule tree: one algorithm, deepest match wins, ties broken by tier.
//!
//! `config.md` section 5 exists because an earlier draft specified two algorithms
//! that disagreed. Its eight-row truth table is the specification and the test suite.

use crate::primitives::path::AbsPath;
use globset::{Candidate, Glob, GlobSet, GlobSetBuilder};
use std::collections::BTreeMap;
use std::path::Path;

/// Applied unless `use_default_ignores = false`. Load-bearing rather than cosmetic:
/// the global cargo target directory on the first machine is 38 GB, and committing
/// it once puts it in history permanently.
pub const DEFAULT_JUNK: [&str; 20] = [
    "node_modules",
    "target",
    "build",
    ".build",
    "dist",
    ".next",
    ".nuxt",
    ".svelte-kit",
    "DerivedData",
    ".gradle",
    "Pods",
    "__pycache__",
    ".venv",
    "venv",
    "*.o",
    "*.pyc",
    "*.class",
    ".DS_Store",
    "Thumbs.db",
    "*.xcuserstate",
];

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
    junk: GlobSet,
    junk_patterns: Vec<String>,
}

/// The inputs, already expanded and validated by layer 0.
#[derive(Debug, Default)]
pub struct RuleSet {
    pub watch: Vec<AbsPath>,
    pub ignore_paths: Vec<AbsPath>,
    pub reinclude: Vec<AbsPath>,
    pub ignore_globs: Vec<String>,
    pub junk: Vec<String>,
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

        Ok(Self {
            explicit,
            globs: compile(&rules.ignore_globs)?,
            glob_patterns: rules.ignore_globs.clone(),
            junk: compile(&rules.junk)?,
            junk_patterns: rules.junk.clone(),
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
            for (matcher, patterns, tier) in [
                (&self.globs, &self.glob_patterns, Tier::Glob),
                (&self.junk, &self.junk_patterns, Tier::Junk),
            ] {
                if let Some(index) = matcher.matches_candidate(&candidate).first() {
                    consider(
                        &mut best,
                        Decision {
                            verdict: Verdict::Skip,
                            tier,
                            depth,
                            rule: patterns[*index].clone(),
                        },
                    );
                }
            }
        }
        best
    }

    /// Whether a path is captured. The whole of the rule tree from a caller's view.
    #[must_use]
    pub fn captures(&self, path: &Path) -> bool {
        self.resolve(path).verdict == Verdict::Capture
    }
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
fn compile(patterns: &[String]) -> Result<GlobSet, RuleError> {
    let mut builder = GlobSetBuilder::new();
    for pattern in patterns {
        let anchored = if pattern.contains('/') {
            pattern.clone()
        } else {
            format!("**/{pattern}")
        };
        let glob = Glob::new(&anchored).map_err(|source| RuleError::BadGlob {
            pattern: pattern.clone(),
            source,
        })?;
        builder.add(glob);
    }
    builder.build().map_err(|source| RuleError::BadGlob {
        pattern: patterns.join(", "),
        source,
    })
}

#[cfg(test)]
mod tests {
    use super::{DEFAULT_JUNK, RuleSet, RuleTree, Tier, Verdict};
    use crate::primitives::path::AbsPath;
    use std::path::Path;

    /// The truth table uses `~` for readability; the algorithm compares component
    /// counts of prefixes of one candidate, so a constant offset cannot reorder them.
    fn home(rest: &str) -> AbsPath {
        AbsPath::parse_with(&format!("~/{rest}"), Some(Path::new("/h")), |_| None)
            .expect("a valid path")
    }

    fn paths(rest: &[&str]) -> Vec<AbsPath> {
        rest.iter().map(|item| home(item)).collect()
    }

    fn tree(rules: RuleSet) -> RuleTree {
        RuleTree::build(&rules).expect("the patterns compile")
    }

    fn captured(tree: &RuleTree, candidate: &str) -> bool {
        tree.captures(home(candidate).as_path())
    }

    /// Row 1: watch `~/A` (d1), candidate `~/A/x.md` (d2). Winner: watch, d1.
    #[test]
    fn row_1_a_watched_root_captures_what_is_under_it() {
        let tree = tree(RuleSet {
            watch: paths(&["A"]),
            ..RuleSet::default()
        });
        assert!(captured(&tree, "A/x.md"));
    }

    /// Row 2: + ignore `~/A/s` (d2), candidate `~/A/s/t.bin`. Winner: ignore, d2.
    #[test]
    fn row_2_a_deeper_ignore_beats_the_watch_above_it() {
        let tree = tree(RuleSet {
            watch: paths(&["A"]),
            ignore_paths: paths(&["A/s"]),
            ..RuleSet::default()
        });
        assert!(!captured(&tree, "A/s/t.bin"));
    }

    /// Row 3: + reinclude `~/A/s/keep` (d3), candidate `~/A/s/keep/k.pem`.
    #[test]
    fn row_3_a_deeper_reinclude_beats_the_ignore_above_it() {
        let tree = tree(RuleSet {
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
        let tree = tree(RuleSet {
            watch: paths(&["A"]),
            junk: vec!["target".to_owned()],
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
        let tree = tree(RuleSet {
            watch: paths(&["A"]),
            reinclude: paths(&["A/p/target"]),
            junk: vec!["target".to_owned()],
            ..RuleSet::default()
        });
        assert!(captured(&tree, "A/p/target/x.o"));
    }

    /// Row 6: glob `**/*.xcarchive` matches `~/A/b/Foo.xcarchive` at d3.
    #[test]
    fn row_6_a_glob_ignores_what_it_matches() {
        let tree = tree(RuleSet {
            watch: paths(&["A"]),
            ignore_globs: vec!["**/*.xcarchive".to_owned()],
            ..RuleSet::default()
        });
        assert!(!captured(&tree, "A/b/Foo.xcarchive"));
    }

    /// Row 7: + reinclude of the file itself (d3). Reinclude beats glob by tier.
    #[test]
    fn row_7_a_reinclude_beats_a_glob_at_the_same_depth_by_tier() {
        let tree = tree(RuleSet {
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
        let tree = tree(RuleSet {
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
        tree(RuleSet {
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
        let tree = tree(RuleSet {
            watch: paths(&["A"]),
            reinclude: paths(&["A/p/target"]),
            junk: DEFAULT_JUNK.iter().map(|s| (*s).to_owned()).collect(),
            ..RuleSet::default()
        });
        let decision = tree.resolve(home("A/p/target/x.o").as_path());
        assert_eq!(decision.verdict, Verdict::Skip);
        assert_eq!(decision.rule, "*.o", "the deeper junk glob should win");
        // A file the junk list does not name comes back.
        assert!(captured(&tree, "A/p/target/keep.txt"));
    }

    #[test]
    fn a_path_no_rule_matches_is_skipped() {
        let tree = tree(RuleSet {
            watch: paths(&["A"]),
            ..RuleSet::default()
        });
        assert!(!captured(&tree, "B/x.md"));
    }

    #[test]
    fn a_watch_at_a_shallow_depth_never_rescues_a_deeper_ignore() {
        let tree = tree(RuleSet {
            watch: paths(&["A"]),
            junk: vec!["node_modules".to_owned()],
            ..RuleSet::default()
        });
        assert!(!captured(&tree, "A/b/c/d/e/f/node_modules/pkg/index.js"));
    }

    #[test]
    fn a_glob_matches_a_path_that_is_not_utf_8() {
        use std::ffi::OsStr;
        use std::os::unix::ffi::OsStrExt;

        let tree = tree(RuleSet {
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
