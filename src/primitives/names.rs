//! The validated names. Each has a `Result`-returning constructor and no other way
//! in, so a downstream module cannot hold one that git or launchd would reject.

use crate::primitives::encode::percent_component;
use std::ffi::OsStr;
use std::fmt;

const MAX_SLUG: usize = 64;
const RESERVED_PROFILE: &str = "catchup";
const RESERVED_ALIAS: &str = ".tycho";

/// A profile name. Becomes a store filename, a directory in every remote, and part
/// of a launchd label.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ProfileName(String);

/// A remote's label within a profile.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RemoteName(String);

/// The short name a watched root gets inside the store. Holds its encoded form,
/// because the same string is both a tree path component and part of the refname
/// key, and a second representation would be one that could drift.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RootAlias(String);

/// A branch as a source repository reports it. May contain `/`.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct BranchName(String);

/// A full refname under `refs/`.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RefName(String);

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum SlugError {
    #[error("the name is empty")]
    Empty,
    #[error("'{0}' is longer than {MAX_SLUG} characters")]
    TooLong(String),
    #[error("'{name}' must start with a lowercase letter or digit, not '{first}'")]
    BadStart { name: String, first: char },
    #[error("'{name}' contains '{ch}'; only lowercase letters, digits and '-' are allowed")]
    BadChar { name: String, ch: char },
    #[error("'{0}' is reserved")]
    Reserved(String),
}

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum AliasError {
    #[error("the alias is empty")]
    Empty,
    #[error("an explicit alias cannot contain a path separator")]
    Separator,
    #[error("'{0}' begins with a dot, which git rejects in a refname")]
    LeadingDot(String),
    #[error("'{0}' ends with '.lock', which git rejects in a refname")]
    LockSuffix(String),
    #[error("'{RESERVED_ALIAS}' is reserved for the store's own metadata")]
    Reserved,
}

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum RefNameError {
    #[error("the refname is empty")]
    Empty,
    #[error("'{0}' does not begin with 'refs/'")]
    NotUnderRefs(String),
    #[error("'{0}' has an empty component")]
    EmptyComponent(String),
    #[error("'{0}' contains '..'")]
    DoubleDot(String),
    #[error("'{0}' contains '@{{'")]
    AtBrace(String),
    #[error("'{0}' is a bare '@'")]
    BareAt(String),
    #[error("'{0}' ends with a dot")]
    TrailingDot(String),
    #[error("'{name}' contains '{ch}', which git rejects in a refname")]
    BadChar { name: String, ch: char },
    #[error("component '{0}' begins with a dot")]
    ComponentLeadingDot(String),
    #[error("component '{0}' ends with '.lock'")]
    ComponentLockSuffix(String),
}

impl ProfileName {
    /// # Errors
    ///
    /// If the name is outside `[a-z0-9][a-z0-9-]*`, too long, or reserved.
    pub fn parse(input: &str) -> Result<Self, SlugError> {
        validate_slug(input)?;
        if input == RESERVED_PROFILE {
            return Err(SlugError::Reserved(input.to_owned()));
        }
        Ok(Self(input.to_owned()))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl RemoteName {
    /// # Errors
    ///
    /// If the name is outside `[a-z0-9][a-z0-9-]*` or too long.
    pub fn parse(input: &str) -> Result<Self, SlugError> {
        validate_slug(input)?;
        Ok(Self(input.to_owned()))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl RootAlias {
    /// From an explicit `name = "..."` in the config.
    ///
    /// # Errors
    ///
    /// If it contains a path separator, is reserved, or encodes to something git
    /// rejects in a refname.
    pub fn explicit(input: &str) -> Result<Self, AliasError> {
        if input.contains('/') || input.contains('\\') {
            return Err(AliasError::Separator);
        }
        Self::encode(input.as_bytes())
    }

    /// From a watched root's last path component.
    ///
    /// # Errors
    ///
    /// As [`RootAlias::explicit`], minus the separator case.
    pub fn from_component(component: &OsStr) -> Result<Self, AliasError> {
        Self::encode(component.as_encoded_bytes())
    }

    fn encode(raw: &[u8]) -> Result<Self, AliasError> {
        if raw.is_empty() {
            return Err(AliasError::Empty);
        }
        if raw == RESERVED_ALIAS.as_bytes() {
            return Err(AliasError::Reserved);
        }
        let shown = String::from_utf8_lossy(raw).into_owned();
        if raw[0] == b'.' {
            return Err(AliasError::LeadingDot(shown));
        }
        if raw.ends_with(b".lock") {
            return Err(AliasError::LockSuffix(shown));
        }
        Ok(Self(percent_component(raw)))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl BranchName {
    /// # Errors
    ///
    /// If any component is one git would reject.
    pub fn parse(input: &str) -> Result<Self, RefNameError> {
        validate_ref_path(input)?;
        Ok(Self(input.to_owned()))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl RefName {
    /// Validates structurally and conservatively. `git check-ref-format` stays the
    /// authority and runs at layer 2 before use; enumerating git's rules by hand is
    /// how an earlier draft of `store.md` got them wrong.
    ///
    /// # Errors
    ///
    /// If the name is not under `refs/`, or breaks one of the rules checked here.
    pub fn parse(input: &str) -> Result<Self, RefNameError> {
        if !input.starts_with("refs/") {
            return Err(RefNameError::NotUnderRefs(input.to_owned()));
        }
        validate_ref_path(input)?;
        Ok(Self(input.to_owned()))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

fn validate_slug(input: &str) -> Result<(), SlugError> {
    let Some(first) = input.chars().next() else {
        return Err(SlugError::Empty);
    };
    if input.len() > MAX_SLUG {
        return Err(SlugError::TooLong(input.to_owned()));
    }
    if !first.is_ascii_lowercase() && !first.is_ascii_digit() {
        return Err(SlugError::BadStart {
            name: input.to_owned(),
            first,
        });
    }
    for ch in input.chars() {
        if !ch.is_ascii_lowercase() && !ch.is_ascii_digit() && ch != '-' {
            return Err(SlugError::BadChar {
                name: input.to_owned(),
                ch,
            });
        }
    }
    Ok(())
}

fn validate_ref_path(input: &str) -> Result<(), RefNameError> {
    if input.is_empty() {
        return Err(RefNameError::Empty);
    }
    let owned = || input.to_owned();
    if input.starts_with('/') || input.ends_with('/') || input.contains("//") {
        return Err(RefNameError::EmptyComponent(owned()));
    }
    if input.contains("..") {
        return Err(RefNameError::DoubleDot(owned()));
    }
    if input.contains("@{") {
        return Err(RefNameError::AtBrace(owned()));
    }
    if input == "@" {
        return Err(RefNameError::BareAt(owned()));
    }
    if input.ends_with('.') {
        return Err(RefNameError::TrailingDot(owned()));
    }
    for ch in input.chars() {
        if ch.is_control() || matches!(ch, ' ' | '~' | '^' | ':' | '?' | '*' | '[' | '\\') {
            return Err(RefNameError::BadChar { name: owned(), ch });
        }
    }
    for component in input.split('/') {
        if component.starts_with('.') {
            return Err(RefNameError::ComponentLeadingDot(component.to_owned()));
        }
        if component.ends_with(".lock") {
            return Err(RefNameError::ComponentLockSuffix(component.to_owned()));
        }
    }
    Ok(())
}

macro_rules! display {
    ($($ty:ty),*) => {$(
        impl fmt::Display for $ty {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                // pad, not write_str: the latter ignores width and alignment, so
                // every table column built from one of these would collapse.
                f.pad(&self.0)
            }
        }
    )*};
}

display!(ProfileName, RemoteName, RootAlias, BranchName, RefName);

#[cfg(test)]
mod tests {
    use super::{
        AliasError, BranchName, ProfileName, RefName, RefNameError, RemoteName, RootAlias,
        SlugError,
    };
    use std::ffi::OsStr;
    use std::os::unix::ffi::OsStrExt;

    #[test]
    fn profile_names_accept_the_documented_charset() {
        for name in ["coreenginex", "second-company", "a", "0", "a1-b2"] {
            assert!(ProfileName::parse(name).is_ok(), "{name}");
        }
    }

    #[test]
    fn profile_names_reject_everything_else() {
        assert_eq!(ProfileName::parse(""), Err(SlugError::Empty));
        assert!(matches!(
            ProfileName::parse("-lead"),
            Err(SlugError::BadStart { .. })
        ));
        assert!(matches!(
            ProfileName::parse("Work"),
            Err(SlugError::BadStart { .. })
        ));
        for bad in ["my.profile", "my/profile", "my profile", "my_profile"] {
            assert!(
                matches!(ProfileName::parse(bad), Err(SlugError::BadChar { .. })),
                "{bad}"
            );
        }
        assert!(matches!(
            ProfileName::parse(&"a".repeat(65)),
            Err(SlugError::TooLong(_))
        ));
    }

    #[test]
    fn catchup_is_reserved_for_the_shared_agent() {
        assert_eq!(
            ProfileName::parse("catchup"),
            Err(SlugError::Reserved("catchup".to_owned()))
        );
        assert!(ProfileName::parse("catchup-work").is_ok());
        assert!(RemoteName::parse("catchup").is_ok());
    }

    #[test]
    fn aliases_encode_rather_than_reject_a_charset_violation() {
        assert_eq!(
            RootAlias::explicit("work-docs").expect("valid").as_str(),
            "work-docs"
        );
        assert_eq!(
            RootAlias::explicit("my docs").expect("valid").as_str(),
            "my%20docs"
        );
        let raw = RootAlias::from_component(OsStr::from_bytes(b"caf\xc3\xa9")).expect("valid");
        assert_eq!(raw.as_str(), "caf%C3%A9");
    }

    #[test]
    fn aliases_reject_the_positional_cases_git_cannot_hold() {
        assert_eq!(RootAlias::explicit(""), Err(AliasError::Empty));
        assert_eq!(RootAlias::explicit("a/b"), Err(AliasError::Separator));
        assert_eq!(RootAlias::explicit(".tycho"), Err(AliasError::Reserved));
        assert_eq!(
            RootAlias::explicit(".ssh"),
            Err(AliasError::LeadingDot(".ssh".to_owned()))
        );
        assert_eq!(
            RootAlias::explicit("main.lock"),
            Err(AliasError::LockSuffix("main.lock".to_owned()))
        );
    }

    #[test]
    fn refnames_must_live_under_refs() {
        assert!(RefName::parse("refs/heads/main").is_ok());
        assert!(matches!(
            RefName::parse("heads/main"),
            Err(RefNameError::NotUnderRefs(_))
        ));
        assert!(matches!(
            RefName::parse("HEAD"),
            Err(RefNameError::NotUnderRefs(_))
        ));
    }

    #[test]
    fn refnames_reject_what_git_rejects() {
        let cases: [(&str, fn(&RefNameError) -> bool); 8] = [
            ("refs/heads//main", |e| {
                matches!(e, RefNameError::EmptyComponent(_))
            }),
            ("refs/heads/a..b", |e| {
                matches!(e, RefNameError::DoubleDot(_))
            }),
            ("refs/heads/a@{1}", |e| {
                matches!(e, RefNameError::AtBrace(_))
            }),
            ("refs/heads/main.", |e| {
                matches!(e, RefNameError::TrailingDot(_))
            }),
            ("refs/heads/a b", |e| {
                matches!(e, RefNameError::BadChar { .. })
            }),
            ("refs/heads/a~1", |e| {
                matches!(e, RefNameError::BadChar { .. })
            }),
            ("refs/heads/.hidden", |e| {
                matches!(e, RefNameError::ComponentLeadingDot(_))
            }),
            ("refs/heads/main.lock", |e| {
                matches!(e, RefNameError::ComponentLockSuffix(_))
            }),
        ];
        for (input, matches_expected) in cases {
            let error = RefName::parse(input).expect_err(input);
            assert!(matches_expected(&error), "{input} gave {error}");
        }
    }

    #[test]
    fn a_branch_name_may_contain_slashes() {
        assert_eq!(
            BranchName::parse("fix/parser-eof").expect("valid").as_str(),
            "fix/parser-eof"
        );
        assert!(BranchName::parse("a b").is_err());
    }
}
