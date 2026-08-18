//! Layer 3. `.tycho/rules.toml`: rules that live with the data they govern.
//!
//! Parsing is pure - the walk reads the file and hands the text here - so the
//! containment rules are testable without a filesystem, like the rest of the
//! rule engine.

use crate::primitives::path::AbsPath;
use serde::Deserialize;

/// The directory a watched tree scopes rules in.
pub const DIR_NAME: &str = ".tycho";
/// The rule file inside [`DIR_NAME`].
pub const FILE_NAME: &str = "rules.toml";

/// One path entry as written, resolved against the declaring directory.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    pub text: String,
    pub line: u32,
    /// The absolute path the entry names.
    pub path: AbsPath,
}

/// One glob entry as written. Anchoring to the declaring directory happens when
/// the tree compiles it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GlobEntry {
    pub text: String,
    pub line: u32,
}

/// A parsed rule file. Unknown keys are carried rather than dropped, so a run
/// can warn about them and `config check` can refuse them.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LocalRules {
    pub ignore: Vec<Entry>,
    pub reinclude: Vec<Entry>,
    pub globs: Vec<GlobEntry>,
    pub unknown: Vec<String>,
}

impl LocalRules {
    #[must_use]
    pub fn len(&self) -> usize {
        self.ignore.len() + self.reinclude.len() + self.globs.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

#[derive(Debug, thiserror::Error)]
pub enum LocalRuleError {
    #[error("not valid TOML: {reason}")]
    Malformed { reason: String },
    #[error("missing `version = 1`")]
    MissingVersion,
    #[error("version {found}; this tycho reads version 1")]
    WrongVersion { found: u32 },
    #[error(
        "line {line}: a local file cannot declare `watch` - it scopes a watch, it \
         cannot create one"
    )]
    WatchKey { line: u32 },
    #[error("line {line}: '{text}' {reason}")]
    Escapes {
        line: u32,
        text: String,
        reason: &'static str,
    },
}

/// The shape `rules.toml` deserialises into. No `#[serde(flatten)]` catch-all:
/// its buffering strips the span information the `Spanned` fields carry, so
/// unknown keys are found by a second parse into a plain table instead.
#[derive(Debug, Deserialize)]
struct Raw {
    version: Option<u32>,
    #[serde(default)]
    ignore: Vec<toml::Spanned<String>>,
    #[serde(default)]
    reinclude: Vec<toml::Spanned<String>>,
    #[serde(default)]
    globs: Vec<toml::Spanned<String>>,
    watch: Option<toml::Spanned<toml::Value>>,
}

const KNOWN_KEYS: [&str; 5] = ["version", "ignore", "reinclude", "globs", "watch"];

/// Parses one rule file. `base` is the directory holding the `.tycho`.
///
/// # Errors
///
/// If the text is not TOML, the version is missing or wrong, `watch` appears,
/// or any entry breaks containment.
pub fn parse(text: &str, base: &AbsPath) -> Result<LocalRules, LocalRuleError> {
    let raw: Raw = toml::from_str(text).map_err(|error| LocalRuleError::Malformed {
        reason: error.to_string(),
    })?;
    match raw.version {
        None => return Err(LocalRuleError::MissingVersion),
        Some(found) if found != crate::config::raw::VERSION => {
            return Err(LocalRuleError::WrongVersion { found });
        }
        Some(_) => {}
    }
    if let Some(watch) = raw.watch {
        return Err(LocalRuleError::WatchKey {
            line: line_of(text, watch.span().start),
        });
    }

    let table: toml::Table = toml::from_str(text).map_err(|error| LocalRuleError::Malformed {
        reason: error.to_string(),
    })?;
    let unknown = table
        .keys()
        .filter(|key| !KNOWN_KEYS.contains(&key.as_str()))
        .cloned()
        .collect();

    let mut rules = LocalRules {
        unknown,
        ..LocalRules::default()
    };
    for spanned in raw.ignore {
        rules.ignore.push(entry(text, base, spanned)?);
    }
    for spanned in raw.reinclude {
        rules.reinclude.push(entry(text, base, spanned)?);
    }
    for spanned in raw.globs {
        let line = line_of(text, spanned.span().start);
        let pattern = spanned.into_inner();
        contained(&pattern, line)?;
        rules.globs.push(GlobEntry {
            text: pattern,
            line,
        });
    }
    Ok(rules)
}

fn entry(
    source: &str,
    base: &AbsPath,
    spanned: toml::Spanned<String>,
) -> Result<Entry, LocalRuleError> {
    let line = line_of(source, spanned.span().start);
    let text = spanned.into_inner();
    let relative = contained(&text, line)?;
    let path = AbsPath::from_absolute(&base.as_path().join(relative)).map_err(|_| {
        LocalRuleError::Escapes {
            line,
            text: text.clone(),
            reason: "does not join into a usable path",
        }
    })?;
    Ok(Entry { text, line, path })
}

/// The containment invariant: an entry may only name what sits at or below the
/// declaring directory. Every violation is an error rather than a warning,
/// because a rule that silently reached elsewhere would take or drop files
/// nobody named. Returns the text with a single trailing `/` stripped, the one
/// gitignore habit worth absorbing.
fn contained(text: &str, line: u32) -> Result<&str, LocalRuleError> {
    let escapes = |reason| {
        Err(LocalRuleError::Escapes {
            line,
            text: text.to_owned(),
            reason,
        })
    };
    if text.starts_with(['/', '\\', '~', '$']) {
        return escapes("is not relative to the directory that declares it");
    }
    if text.contains('\\') {
        return escapes("uses `\\`; entries use `/` on every platform");
    }
    if has_drive_letter(text) {
        return escapes("carries a drive letter, so it is not relative");
    }
    let trimmed = text.strip_suffix('/').unwrap_or(text);
    if trimmed.is_empty() || trimmed == "." {
        return escapes(
            "names the declaring directory itself, which is already captured - \
             that is what admitted this file",
        );
    }
    for component in trimmed.split('/') {
        if component == ".." {
            return escapes("escapes the directory that declares it");
        }
        if component.is_empty() || component == "." {
            return escapes("has an empty or `.` component");
        }
    }
    Ok(trimmed)
}

fn has_drive_letter(text: &str) -> bool {
    let mut chars = text.chars();
    matches!(
        (chars.next(), chars.next()),
        (Some(letter), Some(':')) if letter.is_ascii_alphabetic()
    )
}

#[allow(clippy::cast_possible_truncation)]
fn line_of(text: &str, offset: usize) -> u32 {
    (1 + text[..offset].matches('\n').count()) as u32
}

#[cfg(test)]
mod tests {
    use super::{LocalRuleError, parse};
    use crate::primitives::path::AbsPath;
    use std::path::Path;

    #[cfg(unix)]
    const HOME: &str = "/h";
    #[cfg(windows)]
    const HOME: &str = r"C:\h";

    fn base() -> AbsPath {
        AbsPath::parse_with("~/A/proj", Some(Path::new(HOME)), |_| None).expect("a valid path")
    }

    fn err(text: &str) -> LocalRuleError {
        parse(text, &base()).expect_err("this file is not valid")
    }

    #[test]
    fn entries_resolve_against_the_declaring_directory() {
        let rules = parse(
            "version = 1\nignore = [\"datasets\", \"out/\"]\nreinclude = [\"out/keep.db\"]\n",
            &base(),
        )
        .expect("a valid file");
        let datasets = base().as_path().join("datasets");
        assert_eq!(rules.ignore[0].path.as_path(), datasets);
        assert_eq!(rules.ignore[0].line, 2);
        // The trailing slash is a gitignore habit, not a different rule.
        assert_eq!(rules.ignore[1].path.as_path(), base().as_path().join("out"));
        assert_eq!(rules.reinclude[0].line, 3);
        assert_eq!(rules.len(), 3);
    }

    #[test]
    fn every_way_out_of_the_directory_is_an_error() {
        for (entry, hint) in [
            ("/etc/passwd", "relative"),
            ("~/elsewhere", "relative"),
            ("$HOME/x", "relative"),
            ("C:/x", "drive"),
            ("a\\b", "`\\`"),
            ("../sibling", "escapes"),
            ("a/../../b", "escapes"),
            ("", "declaring directory"),
            (".", "declaring directory"),
            ("a//b", "empty"),
            ("./a", "`.` component"),
        ] {
            let text = format!("version = 1\nignore = [{entry:?}]\n");
            let error = parse(&text, &base()).expect_err(entry).to_string();
            assert!(error.contains("line 2"), "{entry}: {error}");
            assert!(error.to_lowercase().contains(hint), "{entry}: {error}");
        }
    }

    #[test]
    fn watch_cannot_appear() {
        let error = err("version = 1\n\nwatch = [\"anything\"]\n");
        assert!(
            matches!(error, LocalRuleError::WatchKey { line: 3 }),
            "{error}"
        );
    }

    #[test]
    fn the_version_is_required_and_pinned() {
        assert!(matches!(
            err("ignore = [\"a\"]\n"),
            LocalRuleError::MissingVersion
        ));
        assert!(matches!(
            err("version = 2\n"),
            LocalRuleError::WrongVersion { found: 2 }
        ));
    }

    #[test]
    fn unknown_keys_are_carried_not_dropped() {
        let rules = parse("version = 1\nignor = [\"typo\"]\n", &base()).expect("still parses");
        assert_eq!(rules.unknown, vec!["ignor".to_owned()]);
        assert!(rules.is_empty());
    }

    #[test]
    fn a_glob_is_checked_for_containment_too() {
        let error = err("version = 1\nglobs = [\"../*.ckpt\"]\n");
        assert!(error.to_string().contains("escapes"), "{error}");
        let rules = parse(
            "version = 1\nglobs = [\"*.ckpt\", \"out/*.tmp\"]\n",
            &base(),
        )
        .expect("valid");
        assert_eq!(rules.globs[0].text, "*.ckpt");
        assert_eq!(rules.globs[1].line, 2);
    }

    #[test]
    fn malformed_toml_is_an_error_not_a_permissive_default() {
        assert!(matches!(
            err("version = "),
            LocalRuleError::Malformed { .. }
        ));
    }
}
