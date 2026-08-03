//! Layer 3. The TOML schema, its validation, and the rule tree. Pure: no IO, so
//! rule resolution is testable with plain values.

pub mod check;
pub mod raw;
pub mod rules;

use crate::primitives::names::{ProfileName, RemoteName, RootAlias};
use crate::primitives::path::AbsPath;
use std::fmt;
use std::path::Path;
use std::time::Duration;

pub use check::{Diagnostic, DiagnosticKind, Severity};

/// A watched root, and the name it gets inside the store. A sum type rather than a
/// struct with an optional name, so "named" and "defaulted" cannot both be true.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WatchEntry {
    Bare(AbsPath),
    Named { path: AbsPath, name: RootAlias },
}

impl WatchEntry {
    #[must_use]
    pub fn path(&self) -> &AbsPath {
        match self {
            Self::Bare(path) | Self::Named { path, .. } => path,
        }
    }

    /// The alias this root's content is stored under: the explicit name, or the
    /// root's last path component.
    ///
    /// # Errors
    ///
    /// If the last component is not usable as an alias - a leading dot, a `.lock`
    /// suffix, or the reserved `.tycho`.
    pub fn alias(&self) -> Result<RootAlias, crate::primitives::names::AliasError> {
        match self {
            Self::Named { name, .. } => Ok(name.clone()),
            Self::Bare(path) => match path.as_path().file_name() {
                Some(component) => RootAlias::from_component(component),
                None => Err(crate::primitives::names::AliasError::Empty),
            },
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct TimeOfDay {
    pub hour: u8,
    pub minute: u8,
}

impl TimeOfDay {
    /// # Errors
    ///
    /// If the text is not `HH:MM` inside 00:00 to 23:59.
    pub fn parse(text: &str) -> Result<Self, String> {
        let Some((hour, minute)) = text.split_once(':') else {
            return Err(format!("'{text}' is not HH:MM"));
        };
        let hour: u8 = hour.parse().map_err(|_| format!("'{text}' is not HH:MM"))?;
        let minute: u8 = minute
            .parse()
            .map_err(|_| format!("'{text}' is not HH:MM"))?;
        if hour > 23 || minute > 59 {
            return Err(format!("'{text}' is not a time of day"));
        }
        Ok(Self { hour, minute })
    }
}

impl fmt::Display for TimeOfDay {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:02}:{:02}", self.hour, self.minute)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Weekday {
    Sunday,
    Monday,
    Tuesday,
    Wednesday,
    Thursday,
    Friday,
    Saturday,
}

impl Weekday {
    /// # Errors
    ///
    /// If the text is not an English weekday name.
    pub fn parse(text: &str) -> Result<Self, String> {
        match text.to_ascii_lowercase().as_str() {
            "sunday" => Ok(Self::Sunday),
            "monday" => Ok(Self::Monday),
            "tuesday" => Ok(Self::Tuesday),
            "wednesday" => Ok(Self::Wednesday),
            "thursday" => Ok(Self::Thursday),
            "friday" => Ok(Self::Friday),
            "saturday" => Ok(Self::Saturday),
            _ => Err(format!("'{text}' is not a day of the week")),
        }
    }

    /// launchd's `Weekday` key, where Sunday is 0.
    #[must_use]
    pub const fn launchd(self) -> u8 {
        match self {
            Self::Sunday => 0,
            Self::Monday => 1,
            Self::Tuesday => 2,
            Self::Wednesday => 3,
            Self::Thursday => 4,
            Self::Friday => 5,
            Self::Saturday => 6,
        }
    }
}

/// Exactly one, enforced by the type. `Daily` and `Weekly` become
/// `StartCalendarInterval`; `Every` becomes `StartInterval`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Schedule {
    Daily { at: TimeOfDay },
    Weekly { day: Weekday, at: TimeOfDay },
    Every(Duration),
}

/// Parses `6h`-style intervals: an integer and one of `s`, `m`, `h`, `d`.
///
/// # Errors
///
/// If the text is not that shape, or the count is zero.
pub fn parse_interval(text: &str) -> Result<Duration, String> {
    let trimmed = text.trim();
    let split = trimmed.len().saturating_sub(1);
    let (count, unit) = trimmed.split_at(split);
    let seconds = match unit {
        "s" => 1,
        "m" => 60,
        "h" => 3_600,
        "d" => 86_400,
        _ => return Err(format!("'{text}' must end in s, m, h or d")),
    };
    let count: u64 = count
        .parse()
        .map_err(|_| format!("'{text}' must start with a whole number"))?;
    if count == 0 {
        return Err(format!("'{text}' is not an interval"));
    }
    Ok(Duration::from_secs(count * seconds))
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum LogLevel {
    Error,
    Warn,
    #[default]
    Info,
    Debug,
}

impl LogLevel {
    /// # Errors
    ///
    /// If the text is not one of the four levels.
    pub fn parse(text: &str) -> Result<Self, String> {
        match text {
            "error" => Ok(Self::Error),
            "warn" => Ok(Self::Warn),
            "info" => Ok(Self::Info),
            "debug" => Ok(Self::Debug),
            _ => Err(format!("'{text}' is not a log level")),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Remote {
    pub name: RemoteName,
    pub path: AbsPath,
    pub optional: bool,
    pub behind_tolerance: u32,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Profile {
    pub name: ProfileName,
    pub watch: Vec<WatchEntry>,
    pub ignore_paths: Vec<AbsPath>,
    pub ignore_globs: Vec<String>,
    pub reinclude: Vec<AbsPath>,
    pub remotes: Vec<Remote>,
    pub schedule: Option<Schedule>,
    pub use_default_ignores: bool,
    pub store_path: Option<AbsPath>,
    pub local_only: bool,
}

impl Profile {
    /// The rule inputs for this profile, ready for `RuleTree::build`.
    #[must_use]
    pub fn rule_set(&self) -> rules::RuleSet {
        rules::RuleSet {
            watch: self
                .watch
                .iter()
                .map(|entry| entry.path().clone())
                .collect(),
            ignore_paths: self.ignore_paths.clone(),
            reinclude: self.reinclude.clone(),
            ignore_globs: self.ignore_globs.clone(),
            junk: if self.use_default_ignores {
                rules::DEFAULT_JUNK
                    .iter()
                    .map(|pattern| (*pattern).to_owned())
                    .collect()
            } else {
                Vec::new()
            },
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Config {
    pub log_level: LogLevel,
    pub profiles: Vec<Profile>,
}

/// What a parse produced, and everything wrong with it. A config with errors still
/// yields whatever profiles were readable, so `status` can render in a degraded mode
/// rather than showing nothing.
#[derive(Debug)]
pub struct Parsed {
    pub config: Config,
    pub diagnostics: Vec<Diagnostic>,
}

impl Parsed {
    #[must_use]
    pub fn has_errors(&self) -> bool {
        self.diagnostics
            .iter()
            .any(|diagnostic| diagnostic.severity == Severity::Error)
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("the config file is not valid TOML: {0}")]
    Toml(String),
    #[error(
        "this config was written by a newer tycho: it declares version {found}, and this build understands {understood}"
    )]
    Version { found: u32, understood: u32 },
}

/// Parses and validates against the real environment.
///
/// # Errors
///
/// If the text is not TOML, or declares a version this build does not understand.
pub fn parse(text: &str) -> Result<Parsed, ConfigError> {
    parse_with(text, std::env::home_dir().as_deref(), |name| {
        std::env::var(name).ok()
    })
}

/// The same, against a supplied environment - which is how `doctor` re-resolves
/// every root under the launchd agent's environment and compares.
///
/// # Errors
///
/// As [`parse`].
pub fn parse_with(
    text: &str,
    home: Option<&Path>,
    var: impl Fn(&str) -> Option<String>,
) -> Result<Parsed, ConfigError> {
    let probe: raw::VersionProbe =
        toml::from_str(text).map_err(|error| ConfigError::Toml(error.to_string()))?;
    if let Some(found) = probe.version
        && found > raw::VERSION
    {
        return Err(ConfigError::Version {
            found,
            understood: raw::VERSION,
        });
    }

    let raw: raw::RawConfig =
        toml::from_str(text).map_err(|error| ConfigError::Toml(error.to_string()))?;
    Ok(check::validate(&raw, home, &var))
}
