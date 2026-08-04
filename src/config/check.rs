//! Validation. Every problem is reported, not just the first.
//!
//! `config.md` section 8's table is the specification, and every row that is
//! decidable without touching a filesystem is checked here. The rest - does a
//! watched root exist, is the store's volume mounted, was an alias present in the
//! last backup - belong to the slice that has the filesystem and the store, because
//! this layer does no IO.

use crate::config::raw::{RawConfig, RawProfile, RawSchedule, RawWatch, Unknown};
use crate::config::rules::RuleTree;
use crate::config::{
    Config, LogLevel, Profile, Remote, Schedule, TimeOfDay, WatchEntry, Weekday, parse_interval,
};
use crate::primitives::names::{ProfileName, RemoteName, RootAlias};
use crate::primitives::path::AbsPath;
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::path::Path;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Severity {
    Error,
    Warning,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Diagnostic {
    pub severity: Severity,
    pub profile: Option<String>,
    pub kind: DiagnosticKind,
}

/// Typed rather than a formatted string, so a test asserts on the finding instead of
/// on the prose describing it, and slice 11's renderer has one place to read from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DiagnosticKind {
    UnknownKey {
        table: String,
        key: String,
    },
    MissingProfileName,
    InvalidProfileName {
        value: String,
        reason: String,
    },
    DuplicateProfile {
        name: String,
    },
    DuplicateRemote {
        name: String,
    },
    MissingRemoteName,
    InvalidRemoteName {
        value: String,
        reason: String,
    },
    MissingRemotePath {
        name: String,
    },
    EmptyWatch,
    BadPath {
        value: String,
        reason: String,
    },
    InvalidAlias {
        value: String,
        reason: String,
    },
    AliasCollision {
        alias: String,
        first: String,
        second: String,
    },
    NestedWatchedRoot {
        inner: String,
        outer: String,
    },
    ConflictingRules {
        path: String,
    },
    DuplicateStorePath {
        path: String,
    },
    PathInsideWatchedRoot {
        what: String,
        path: String,
        root: String,
    },
    NoRemotes,
    NoSchedule,
    InvalidSchedule {
        reason: String,
    },
    InvalidGlob {
        pattern: String,
        reason: String,
    },
    InvalidLogLevel {
        value: String,
    },
    IgnoreOutsideEveryRoot {
        path: String,
    },
    RedundantWatch {
        path: String,
        covered_by: String,
    },
}

impl fmt::Display for DiagnosticKind {
    #[allow(clippy::too_many_lines)]
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownKey { table, key } => write!(f, "unknown key '{key}' in {table}"),
            Self::MissingProfileName => write!(f, "profile has no name"),
            Self::InvalidProfileName { value, reason } => {
                write!(f, "invalid profile name '{value}': {reason}")
            }
            Self::DuplicateProfile { name } => write!(f, "duplicate profile name '{name}'"),
            Self::DuplicateRemote { name } => {
                write!(f, "duplicate remote name '{name}' in this profile")
            }
            Self::MissingRemoteName => write!(f, "remote has no name"),
            Self::InvalidRemoteName { value, reason } => {
                write!(f, "invalid remote name '{value}': {reason}")
            }
            Self::MissingRemotePath { name } => write!(f, "remote '{name}' has no path"),
            Self::EmptyWatch => write!(f, "profile watches nothing"),
            Self::BadPath { value, reason } => write!(f, "'{value}': {reason}"),
            Self::InvalidAlias { value, reason } => {
                write!(f, "invalid alias for '{value}': {reason}")
            }
            Self::AliasCollision {
                alias,
                first,
                second,
            } => write!(
                f,
                "alias collision: {first} and {second} both resolve to '{alias}'"
            ),
            Self::NestedWatchedRoot { inner, outer } => {
                write!(f, "watched root {inner} is inside watched root {outer}")
            }
            Self::ConflictingRules { path } => write!(
                f,
                "{path} is both ignored and watched or re-included; one of them must go"
            ),
            Self::DuplicateStorePath { path } => {
                write!(f, "two profiles resolve to the same store_path {path}")
            }
            Self::PathInsideWatchedRoot { what, path, root } => {
                write!(f, "the {what} {path} is inside watched root {root}")
            }
            Self::NoRemotes => write!(
                f,
                "profile has no remotes; set local_only = true to confirm this is intended"
            ),
            Self::NoSchedule => write!(f, "profile has no schedule, so it only runs manually"),
            Self::InvalidSchedule { reason } => write!(f, "invalid schedule: {reason}"),
            Self::InvalidGlob { pattern, reason } => {
                write!(f, "invalid glob '{pattern}': {reason}")
            }
            Self::InvalidLogLevel { value } => write!(f, "invalid log_level '{value}'"),
            Self::IgnoreOutsideEveryRoot { path } => write!(
                f,
                "ignore path {path} is not under any watched root, so it can never fire"
            ),
            Self::RedundantWatch { path, covered_by } => {
                write!(f, "watched root {path} is already covered by {covered_by}")
            }
        }
    }
}

impl Diagnostic {
    /// The suggestion `config check` prints under the message, where there is one.
    #[must_use]
    pub fn hint(&self) -> Option<String> {
        match &self.kind {
            DiagnosticKind::AliasCollision { first, .. } => Some(format!(
                "give one an explicit name: {{ path = \"{first}\", name = \"...\" }}"
            )),
            DiagnosticKind::NoRemotes => Some("local_only = true".to_owned()),
            _ => None,
        }
    }
}

struct Collector {
    profile: Option<String>,
    out: Vec<Diagnostic>,
}

impl Collector {
    fn error(&mut self, kind: DiagnosticKind) {
        self.out.push(Diagnostic {
            severity: Severity::Error,
            profile: self.profile.clone(),
            kind,
        });
    }

    fn warn(&mut self, kind: DiagnosticKind) {
        self.out.push(Diagnostic {
            severity: Severity::Warning,
            profile: self.profile.clone(),
            kind,
        });
    }

    fn unknown_keys(&mut self, table: &str, unknown: &Unknown) {
        for key in unknown.keys() {
            self.error(DiagnosticKind::UnknownKey {
                table: table.to_owned(),
                key: key.clone(),
            });
        }
    }
}

pub(crate) fn validate(
    raw: &RawConfig,
    home: Option<&Path>,
    var: &impl Fn(&str) -> Option<String>,
) -> crate::config::Parsed {
    let mut collector = Collector {
        profile: None,
        out: Vec::new(),
    };
    collector.unknown_keys("the top-level table", &raw.unknown);
    collector.unknown_keys("[settings]", &raw.settings.unknown);

    let log_level = raw
        .settings
        .log_level
        .as_deref()
        .map_or_else(LogLevel::default, |text| {
            LogLevel::parse(text).unwrap_or_else(|_| {
                collector.error(DiagnosticKind::InvalidLogLevel {
                    value: text.to_owned(),
                });
                LogLevel::default()
            })
        });

    let mut profiles = Vec::new();
    let mut seen_names = BTreeSet::new();
    let mut store_paths: BTreeMap<AbsPath, String> = BTreeMap::new();

    for raw_profile in &raw.profile {
        collector.profile = raw_profile.name.clone();
        collector.unknown_keys("[[profile]]", &raw_profile.unknown);
        if let Some(profile) = validate_profile(raw_profile, home, var, &mut collector) {
            if !seen_names.insert(profile.name.clone()) {
                collector.error(DiagnosticKind::DuplicateProfile {
                    name: profile.name.to_string(),
                });
            }
            if let Some(store) = &profile.store_path
                && let Some(first) = store_paths.insert(store.clone(), profile.name.to_string())
            {
                collector.error(DiagnosticKind::DuplicateStorePath {
                    path: format!("{store} (also {first})"),
                });
            }
            profiles.push(profile);
        }
    }
    collector.profile = None;

    crate::config::Parsed {
        config: Config {
            log_level,
            profiles,
        },
        diagnostics: collector.out,
    }
}

fn validate_profile(
    raw: &RawProfile,
    home: Option<&Path>,
    var: &impl Fn(&str) -> Option<String>,
    collector: &mut Collector,
) -> Option<Profile> {
    let Some(raw_name) = raw.name.as_deref() else {
        collector.error(DiagnosticKind::MissingProfileName);
        return None;
    };
    let name = match ProfileName::parse(raw_name) {
        Ok(name) => name,
        Err(reason) => {
            collector.error(DiagnosticKind::InvalidProfileName {
                value: raw_name.to_owned(),
                reason: reason.to_string(),
            });
            return None;
        }
    };

    let mut watch = Vec::new();
    let mut aliases: BTreeMap<String, String> = BTreeMap::new();
    for entry in &raw.watch {
        if let RawWatch::Named { unknown, .. } = entry {
            collector.unknown_keys("a watch entry", unknown);
        }
        let Some(path) = resolve(entry.path(), home, var, collector) else {
            continue;
        };
        let entry = match entry {
            RawWatch::Bare(_) => WatchEntry::Bare(path),
            RawWatch::Named { name, .. } => match RootAlias::explicit(name) {
                Ok(alias) => WatchEntry::Named { path, name: alias },
                Err(reason) => {
                    collector.error(DiagnosticKind::InvalidAlias {
                        value: name.clone(),
                        reason: reason.to_string(),
                    });
                    continue;
                }
            },
        };
        match entry.alias() {
            Ok(alias) => {
                let shown = entry.path().to_string();
                if let Some(first) = aliases.insert(alias.to_string(), shown.clone()) {
                    collector.error(DiagnosticKind::AliasCollision {
                        alias: alias.to_string(),
                        first,
                        second: shown,
                    });
                }
            }
            Err(reason) => collector.error(DiagnosticKind::InvalidAlias {
                value: entry.path().to_string(),
                reason: reason.to_string(),
            }),
        }
        watch.push(entry);
    }

    if watch.is_empty() {
        collector.error(DiagnosticKind::EmptyWatch);
    }

    // A watched root inside another has no answer for which alias its content is
    // stored under, so it is an error rather than a resolution rule.
    for (index, inner) in watch.iter().enumerate() {
        for (other, outer) in watch.iter().enumerate() {
            if index != other && outer.path().contains(inner.path()) && outer.path() != inner.path()
            {
                collector.error(DiagnosticKind::NestedWatchedRoot {
                    inner: inner.path().to_string(),
                    outer: outer.path().to_string(),
                });
            }
        }
    }

    // An entry beginning with ~, $ or / is an explicit path; anything else in
    // `ignore` is a glob. `watch` and `reinclude` take paths only.
    let mut ignore_paths = Vec::new();
    let mut ignore_globs = Vec::new();
    for entry in &raw.ignore {
        if looks_like_path(entry) {
            if let Some(path) = resolve(entry, home, var, collector) {
                ignore_paths.push(path);
            }
        } else {
            ignore_globs.push(entry.clone());
        }
    }
    let mut reinclude = Vec::new();
    for entry in &raw.reinclude {
        if let Some(path) = resolve(entry, home, var, collector) {
            reinclude.push(path);
        }
    }

    // Rule 3 of the algorithm: two explicit rules at equal depth must be the same
    // path, so a conflict reduces to one path appearing on both sides.
    let ignored: BTreeSet<&AbsPath> = ignore_paths.iter().collect();
    for path in reinclude.iter().chain(watch.iter().map(WatchEntry::path)) {
        if ignored.contains(path) {
            collector.error(DiagnosticKind::ConflictingRules {
                path: path.to_string(),
            });
        }
    }

    for path in &ignore_paths {
        if !watch.iter().any(|entry| entry.path().contains(path)) {
            collector.warn(DiagnosticKind::IgnoreOutsideEveryRoot {
                path: path.to_string(),
            });
        }
    }

    let mut remotes = Vec::new();
    let mut seen_remotes = BTreeSet::new();
    for raw_remote in &raw.remotes {
        collector.unknown_keys("a remote entry", &raw_remote.unknown);
        let Some(raw_name) = raw_remote.name.as_deref() else {
            collector.error(DiagnosticKind::MissingRemoteName);
            continue;
        };
        let name = match RemoteName::parse(raw_name) {
            Ok(name) => name,
            Err(reason) => {
                collector.error(DiagnosticKind::InvalidRemoteName {
                    value: raw_name.to_owned(),
                    reason: reason.to_string(),
                });
                continue;
            }
        };
        let Some(raw_path) = raw_remote.path.as_deref() else {
            collector.error(DiagnosticKind::MissingRemotePath {
                name: raw_name.to_owned(),
            });
            continue;
        };
        let Some(path) = resolve(raw_path, home, var, collector) else {
            continue;
        };
        if !seen_remotes.insert(name.clone()) {
            collector.error(DiagnosticKind::DuplicateRemote {
                name: name.to_string(),
            });
        }
        remotes.push(Remote {
            name,
            path,
            optional: raw_remote.optional.unwrap_or(false),
            behind_tolerance: raw_remote.behind_tolerance.unwrap_or(4),
            trust_ownership: raw_remote.trust_ownership.unwrap_or(false),
        });
    }

    let local_only = raw.local_only.unwrap_or(false);
    if remotes.is_empty() && !local_only {
        collector.error(DiagnosticKind::NoRemotes);
    }

    let schedule = match &raw.schedule {
        None => {
            collector.warn(DiagnosticKind::NoSchedule);
            None
        }
        Some(raw_schedule) => match to_schedule(raw_schedule) {
            Ok(schedule) => Some(schedule),
            Err(reason) => {
                collector.error(DiagnosticKind::InvalidSchedule { reason });
                None
            }
        },
    };

    let store_path = raw
        .store_path
        .as_deref()
        .and_then(|text| resolve(text, home, var, collector));

    // Backing up the store, the remotes or the log into themselves is unbounded.
    for (what, path) in store_path
        .iter()
        .map(|path| ("store_path", path))
        .chain(remotes.iter().map(|remote| ("remote path", &remote.path)))
    {
        for root in &watch {
            if root.path().contains(path) {
                collector.error(DiagnosticKind::PathInsideWatchedRoot {
                    what: what.to_owned(),
                    path: path.to_string(),
                    root: root.path().to_string(),
                });
            }
        }
    }

    let profile = Profile {
        name,
        watch,
        ignore_paths,
        ignore_globs,
        reinclude,
        remotes,
        schedule,
        use_default_ignores: raw.use_default_ignores.unwrap_or(true),
        store_path,
        local_only,
    };

    // Compiling here is what turns an unusable glob into a config error rather than
    // a run-time surprise.
    if let Err(error) = RuleTree::build(&profile.rule_set()) {
        collector.error(DiagnosticKind::InvalidGlob {
            pattern: error.to_string(),
            reason: "globset rejected it".to_owned(),
        });
    }

    for (index, inner) in profile.watch.iter().enumerate() {
        for (other, outer) in profile.watch.iter().enumerate() {
            if index != other
                && outer.path().contains(inner.path())
                && outer.path() != inner.path()
                && !profile
                    .ignore_paths
                    .iter()
                    .any(|ignored| outer.path().contains(ignored))
            {
                collector.warn(DiagnosticKind::RedundantWatch {
                    path: inner.path().to_string(),
                    covered_by: outer.path().to_string(),
                });
            }
        }
    }

    Some(profile)
}

/// `~`, `$` and a leading separator on every platform; a drive letter as well on
/// Windows, where `C:/backups` is the ordinary way to write an absolute path.
///
/// Without the drive case such an entry falls through to the glob arm, where it
/// matches nothing and fires never - an ignore rule that silently does not apply,
/// which is the failure mode this whole classification exists to avoid.
fn looks_like_path(entry: &str) -> bool {
    if entry.starts_with(['~', '$', '/']) {
        return true;
    }
    cfg!(windows) && (entry.starts_with('\\') || has_drive_letter(entry))
}

fn has_drive_letter(entry: &str) -> bool {
    let mut chars = entry.chars();
    matches!(
        (chars.next(), chars.next()),
        (Some(letter), Some(':')) if letter.is_ascii_alphabetic()
    )
}

fn resolve(
    text: &str,
    home: Option<&Path>,
    var: &impl Fn(&str) -> Option<String>,
    collector: &mut Collector,
) -> Option<AbsPath> {
    match AbsPath::parse_with(text, home, var) {
        Ok(path) => Some(path),
        Err(reason) => {
            collector.error(DiagnosticKind::BadPath {
                value: text.to_owned(),
                reason: reason.to_string(),
            });
            None
        }
    }
}

fn to_schedule(raw: &RawSchedule) -> Result<Schedule, String> {
    match raw {
        RawSchedule::Daily { at } => Ok(Schedule::Daily {
            at: TimeOfDay::parse(at)?,
        }),
        RawSchedule::Weekly { day, at } => Ok(Schedule::Weekly {
            day: Weekday::parse(day)?,
            at: TimeOfDay::parse(at)?,
        }),
        RawSchedule::Every(text) => parse_interval(text).map(Schedule::Every),
    }
}
