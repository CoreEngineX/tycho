//! Absolute paths, and the expansion rules a config value goes through to become
//! one.

use std::borrow::Borrow;
use std::ffi::OsString;
use std::fmt;
use std::path::{Component, Path, PathBuf};

const ALLOWED_VARS: [&str; 2] = ["HOME", "USER"];

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct AbsPath(PathBuf);

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum PathError {
    #[error("the path is empty")]
    Empty,
    #[error("'{0}' is not absolute after expansion")]
    Relative(String),
    #[error("'{0}' contains a '..' component; write the path out in full")]
    ParentComponent(String),
    #[error("'~' cannot be expanded: no home directory")]
    NoHome,
    #[error("'~{0}' is not supported; only a bare '~' expands")]
    NamedHome(String),
    #[error("'${0}' is not expandable; only {ALLOWED} expand")]
    VariableNotAllowed(String),
    #[error("'${0}' is unset or empty")]
    VariableUnset(String),
    #[error("'{0}' has a '${{' that is never closed")]
    UnterminatedBrace(String),
    #[error("'{0}' is not a single name, so it would point outside the directory holding it")]
    NotALeaf(String),
}

const ALLOWED: &str = "$HOME and $USER";

impl AbsPath {
    /// Expands and validates a config value.
    ///
    /// # Errors
    ///
    /// Every way the value can fail to name an absolute path, one variant each, so
    /// `config check` can report all of a file's problems at once.
    pub fn parse(input: &str) -> Result<Self, PathError> {
        Self::parse_with(input, std::env::home_dir().as_deref(), |name| {
            std::env::var(name).ok()
        })
    }

    /// The same expansion against a supplied environment, so `doctor` can re-resolve
    /// a root under the launchd agent's environment and compare.
    ///
    /// # Errors
    ///
    /// As [`AbsPath::parse`].
    pub fn parse_with(
        input: &str,
        home: Option<&Path>,
        var: impl Fn(&str) -> Option<String>,
    ) -> Result<Self, PathError> {
        if input.is_empty() {
            return Err(PathError::Empty);
        }
        let expanded = expand_vars(&expand_home(input, home)?, &var)?;
        Self::clean(Path::new(&expanded), input)
    }

    /// For a path a walk produced, which is already absolute and need not be UTF-8.
    ///
    /// # Errors
    ///
    /// If the path is relative or contains a `..` component.
    pub fn from_absolute(path: &Path) -> Result<Self, PathError> {
        let shown = path.display().to_string();
        Self::clean(path, &shown)
    }

    fn clean(path: &Path, shown: &str) -> Result<Self, PathError> {
        let mut out = PathBuf::new();
        for component in path.components() {
            match component {
                Component::Prefix(prefix) => out.push(prefix.as_os_str()),
                Component::RootDir => out.push(Component::RootDir),
                Component::Normal(part) => out.push(part),
                Component::CurDir => {}
                Component::ParentDir => {
                    return Err(PathError::ParentComponent(shown.to_owned()));
                }
            }
        }
        if !out.is_absolute() {
            return Err(PathError::Relative(shown.to_owned()));
        }
        Ok(Self(out))
    }

    #[must_use]
    pub fn as_path(&self) -> &Path {
        &self.0
    }

    #[must_use]
    pub fn into_path_buf(self) -> PathBuf {
        self.0
    }

    /// Whether `other` is this path or lies beneath it. Compares components, so
    /// `/a` does not contain `/ab`.
    #[must_use]
    pub fn contains(&self, other: &Self) -> bool {
        other.0.starts_with(&self.0)
    }
}

impl fmt::Display for AbsPath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.pad(&self.0.to_string_lossy())
    }
}

/// Lets a map keyed by `AbsPath` be looked up with a `&Path`, which the rule tree
/// does once per ancestor of every candidate. Sound because every ordering and
/// equality impl here delegates to the `PathBuf` inside.
impl Borrow<Path> for AbsPath {
    fn borrow(&self) -> &Path {
        &self.0
    }
}

/// A path inside the store's tree: relative, separated by `/`, with no `.git`
/// component. Invariant 8 says such a path is never written into the tree "asserted
/// rather than assumed", so the assertion lives in this constructor and nowhere else.
///
/// The separator is part of the type's contract because a git tree path is always
/// `/`-separated, on every platform. A `PathBuf` built from components is not: it
/// uses the host separator, which on Windows is `\`, and `update-index --index-info`
/// answers a `\` in a path by printing `Ignoring path` to stderr and **exiting 0**.
/// That turns a whole run into an empty tree at exit 0, so the normalisation is here
/// rather than at the three call sites that encode one.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct TreePath(PathBuf);

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum TreePathError {
    #[error("a tree path cannot be empty")]
    Empty,
    #[error("'{0}' is absolute; a tree path is relative to the tree root")]
    Absolute(String),
    #[error("'{0}' contains a '.' or '..' component")]
    DotComponent(String),
    #[error("'{0}' contains a '.git' component, which must never reach the tree")]
    GitComponent(String),
}

impl TreePath {
    /// # Errors
    ///
    /// If the path is empty, absolute, contains `.` or `..`, or contains a `.git`
    /// component in any case.
    pub fn parse(path: &Path) -> Result<Self, TreePathError> {
        let shown = || path.display().to_string();
        if path.as_os_str().is_empty() {
            return Err(TreePathError::Empty);
        }
        if path.is_absolute() {
            return Err(TreePathError::Absolute(shown()));
        }
        if has_git_component(path) {
            return Err(TreePathError::GitComponent(shown()));
        }
        let mut components = 0;
        for component in path.components() {
            match component {
                Component::Normal(_) => components += 1,
                Component::CurDir | Component::ParentDir => {
                    return Err(TreePathError::DotComponent(shown()));
                }
                Component::RootDir | Component::Prefix(_) => {
                    return Err(TreePathError::Absolute(shown()));
                }
            }
        }
        if components == 0 {
            return Err(TreePathError::Empty);
        }
        Ok(Self(PathBuf::from(join_with_slash(path))))
    }

    #[must_use]
    pub fn as_path(&self) -> &Path {
        &self.0
    }
}

impl fmt::Display for TreePath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.pad(&self.0.to_string_lossy())
    }
}

/// Joins already-validated `Normal` components with `/`, never rewriting a byte
/// inside one.
///
/// That distinction is the whole reason this is not a `replace('\\', "/")`: a
/// backslash is a legal byte in a Unix filename and a reserved one in an NTFS name,
/// so rewriting bytes would corrupt a Unix path to fix a Windows one. Joining leaves
/// both correct, and on Unix produces exactly what `components().collect()` did.
fn join_with_slash(path: &Path) -> OsString {
    let mut out = OsString::new();
    for (index, component) in path.components().enumerate() {
        if index > 0 {
            out.push("/");
        }
        out.push(component.as_os_str());
    }
    out
}

/// Whether any component is a `.git` marker, which must never reach the store tree.
#[must_use]
pub fn has_git_component(path: &Path) -> bool {
    path.components().any(|component| match component {
        Component::Normal(part) => part.as_encoded_bytes().eq_ignore_ascii_case(b".git"),
        _ => false,
    })
}

fn expand_home(input: &str, home: Option<&Path>) -> Result<String, PathError> {
    let Some(rest) = input.strip_prefix('~') else {
        return Ok(input.to_owned());
    };
    if !rest.is_empty() && !rest.starts_with('/') {
        let name = rest.split('/').next().unwrap_or(rest);
        return Err(PathError::NamedHome(name.to_owned()));
    }
    let home = home.ok_or(PathError::NoHome)?;
    Ok(format!("{}{rest}", home.display()))
}

fn expand_vars(input: &str, var: &impl Fn(&str) -> Option<String>) -> Result<String, PathError> {
    let mut out = String::with_capacity(input.len());
    let mut rest = input;
    while let Some(at) = rest.find('$') {
        out.push_str(&rest[..at]);
        let after = &rest[at + 1..];
        let (name, tail) = if let Some(braced) = after.strip_prefix('{') {
            let close = braced
                .find('}')
                .ok_or_else(|| PathError::UnterminatedBrace(input.to_owned()))?;
            (&braced[..close], &braced[close + 1..])
        } else {
            let end = after
                .find(|c: char| !c.is_ascii_alphanumeric() && c != '_')
                .unwrap_or(after.len());
            (&after[..end], &after[end..])
        };
        if !ALLOWED_VARS.contains(&name) {
            return Err(PathError::VariableNotAllowed(name.to_owned()));
        }
        match var(name) {
            Some(value) if !value.is_empty() => out.push_str(&value),
            _ => return Err(PathError::VariableUnset(name.to_owned())),
        }
        rest = tail;
    }
    out.push_str(rest);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::{AbsPath, PathError, TreePath, TreePathError, has_git_component};
    use std::path::Path;

    fn env(name: &str) -> Option<String> {
        match name {
            "HOME" => Some(HOME.to_owned()),
            "USER" => Some("tester".to_owned()),
            _ => None,
        }
    }

    /// A path needs a drive prefix to be absolute on Windows, so the fixtures carry
    /// one and the expectations are written in the host's own separator - which is
    /// what `AbsPath` normalises to and therefore what it must print.
    #[cfg(unix)]
    const HOME: &str = "/Users/tester";
    #[cfg(windows)]
    const HOME: &str = r"C:\Users\tester";

    #[cfg(unix)]
    const ROOT: &str = "/data";
    #[cfg(windows)]
    const ROOT: &str = r"C:\data";

    fn joined(parts: &[&str]) -> String {
        let mut out = std::path::PathBuf::from(HOME);
        out.extend(parts);
        out.display().to_string()
    }

    fn parse(input: &str) -> Result<AbsPath, PathError> {
        AbsPath::parse_with(input, Some(Path::new(HOME)), env)
    }

    fn parsed(input: &str) -> String {
        parse(input).expect("valid path").to_string()
    }

    #[test]
    fn expands_tilde() {
        assert_eq!(parsed("~/Books"), joined(&["Books"]));
        assert_eq!(parsed("~"), HOME);
        assert_eq!(parsed("~/"), HOME);
    }

    #[test]
    fn expands_only_the_allowed_variables() {
        assert_eq!(parsed("$HOME/Books"), joined(&["Books"]));
        assert_eq!(parsed("${HOME}/Books"), joined(&["Books"]));
        assert_eq!(
            parsed(&format!("{ROOT}/${{USER}}/x")),
            std::path::PathBuf::from(ROOT)
                .join("tester")
                .join("x")
                .display()
                .to_string()
        );
    }

    #[test]
    fn a_variable_outside_the_allow_list_is_an_error() {
        assert_eq!(
            parse("$WORKSPACE/code"),
            Err(PathError::VariableNotAllowed("WORKSPACE".to_owned()))
        );
    }

    #[test]
    fn an_unset_or_empty_variable_never_becomes_an_empty_string() {
        assert_eq!(
            AbsPath::parse_with("$HOME/code", Some(Path::new(HOME)), |_| Some(String::new())),
            Err(PathError::VariableUnset("HOME".to_owned()))
        );
        assert_eq!(
            AbsPath::parse_with("$HOME/code", Some(Path::new(HOME)), |_| None),
            Err(PathError::VariableUnset("HOME".to_owned()))
        );
    }

    #[test]
    fn rejects_what_cannot_be_resolved() {
        assert_eq!(parse(""), Err(PathError::Empty));
        assert_eq!(
            parse("Books/x"),
            Err(PathError::Relative("Books/x".to_owned()))
        );
        assert_eq!(
            parse("~/A/../B"),
            Err(PathError::ParentComponent("~/A/../B".to_owned()))
        );
        assert_eq!(
            parse("${HOME/x"),
            Err(PathError::UnterminatedBrace("${HOME/x".to_owned()))
        );
        assert_eq!(
            parse("~other/x"),
            Err(PathError::NamedHome("other".to_owned()))
        );
        assert_eq!(
            AbsPath::parse_with("~/x", None, env),
            Err(PathError::NoHome)
        );
    }

    #[test]
    fn normalises_redundant_components() {
        let want = std::path::PathBuf::from(ROOT)
            .join("b")
            .display()
            .to_string();
        assert_eq!(parsed(&format!("{ROOT}//b/")), want);
        assert_eq!(parsed(&format!("{ROOT}/./b")), want);
    }

    #[test]
    fn containment_compares_components_not_prefixes() {
        let a = parse(ROOT).expect("valid");
        assert!(a.contains(&parse(&format!("{ROOT}/b")).expect("valid")));
        assert!(a.contains(&a));
        assert!(!a.contains(&parse(&format!("{ROOT}x")).expect("valid")));
    }

    #[test]
    fn from_absolute_takes_a_walked_path() {
        let full = std::path::PathBuf::from(ROOT).join("b");
        let path = AbsPath::from_absolute(&full).expect("absolute");
        assert_eq!(path.to_string(), full.display().to_string());
        assert_eq!(
            AbsPath::from_absolute(Path::new("b")),
            Err(PathError::Relative("b".to_owned()))
        );
    }

    /// A rooted path with no drive is absolute on Unix and relative on Windows,
    /// which is not a quirk to paper over: `\A` there names `A` on whichever drive
    /// the process happens to be on, and a backup root that moves with the current
    /// directory is exactly what `AbsPath` exists to refuse.
    #[cfg(windows)]
    #[test]
    fn a_rooted_path_with_no_drive_is_refused() {
        assert_eq!(
            AbsPath::parse_with("/data/x", Some(Path::new(HOME)), env),
            Err(PathError::Relative("/data/x".to_owned()))
        );
    }

    #[test]
    fn a_tree_path_is_relative_and_carries_no_git_component() {
        for good in [
            "a.md",
            "CoreEngineX/org/notes.md",
            ".tycho/repos/x/REPO.txt",
        ] {
            assert!(TreePath::parse(Path::new(good)).is_ok(), "{good}");
        }
        assert_eq!(TreePath::parse(Path::new("")), Err(TreePathError::Empty));
        assert!(matches!(
            TreePath::parse(Path::new("/a/b")),
            Err(TreePathError::Absolute(_))
        ));
        assert!(matches!(
            TreePath::parse(Path::new("a/../b")),
            Err(TreePathError::DotComponent(_))
        ));
        for bad in [".git", "a/.git/b", "a/.GIT", ".Git/config"] {
            assert!(
                matches!(
                    TreePath::parse(Path::new(bad)),
                    Err(TreePathError::GitComponent(_))
                ),
                "{bad}"
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn a_tree_path_keeps_hostile_bytes_intact() {
        use std::ffi::OsStr;
        use std::os::unix::ffi::OsStrExt;

        let raw = OsStr::from_bytes(b"a\nb/c\td.md");
        let path = TreePath::parse(Path::new(raw)).expect("valid");
        assert_eq!(path.as_path().as_os_str(), raw);
    }

    /// A newline and a tab are legal in an NTFS name; `\` and `/` are not, so those
    /// are the hostile bytes a Windows tree path can actually carry.
    #[cfg(windows)]
    #[test]
    fn a_tree_path_keeps_hostile_bytes_intact() {
        let path = TreePath::parse(Path::new("a\nb/c\td.md")).expect("valid");
        assert_eq!(path.as_path().as_os_str(), "a\nb/c\td.md");
    }

    /// `update-index --index-info` answers a `\` separator by printing `Ignoring
    /// path` and exiting 0, which turns a run into an empty tree at exit 0. A tree
    /// path built from a native separator must never reach an encoder carrying one.
    #[test]
    fn a_tree_path_is_slash_separated_whatever_the_host_uses() {
        let from_slash = TreePath::parse(Path::new("a/b/c.md")).expect("valid");
        assert_eq!(from_slash.to_string(), "a/b/c.md");
        assert!(
            !from_slash
                .as_path()
                .as_os_str()
                .as_encoded_bytes()
                .contains(&b'\\')
        );

        let joined = Path::new("root").join("sub").join("c.md");
        let from_join = TreePath::parse(&joined).expect("valid");
        assert_eq!(from_join.to_string(), "root/sub/c.md");
    }

    #[test]
    fn detects_a_git_component_case_insensitively() {
        assert!(has_git_component(Path::new("/a/.git/config")));
        assert!(has_git_component(Path::new("/a/.GIT")));
        assert!(!has_git_component(Path::new("/a/git/x")));
        assert!(!has_git_component(Path::new("/a/b.git/x")));
    }
}
