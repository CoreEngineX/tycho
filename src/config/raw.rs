//! The TOML shape, deserialized leniently.
//!
//! Every field that becomes a layer 0 newtype stays a `String` here, because
//! `config.md` requires every problem reported at once and serde stops at the first
//! error. Validation is a separate pass that accumulates diagnostics.
//!
//! Unknown keys are captured rather than rejected by `deny_unknown_fields`, which
//! would abort at the first one and contradict the multi-error report the same
//! document specifies. An unrecognised key is still a hard error - a silently
//! ignored `wacth =` is the config-file form of the bug that made this project
//! necessary - it is just reported alongside everything else.

use serde::Deserialize;
use std::collections::BTreeMap;

/// The schema version this binary understands.
pub const VERSION: u32 = 1;

pub type Unknown = BTreeMap<String, toml::Value>;

/// Read on its own first. Otherwise a config written by a newer Tycho fails as an
/// unknown key and points at the wrong thing, turning a forward-compatible addition
/// into a total backup outage.
#[derive(Debug, Deserialize)]
pub struct VersionProbe {
    pub version: Option<u32>,
}

#[derive(Debug, Default, Deserialize)]
pub struct RawConfig {
    pub version: Option<u32>,
    #[serde(default)]
    pub settings: RawSettings,
    #[serde(default)]
    pub profile: Vec<RawProfile>,
    #[serde(flatten)]
    pub unknown: Unknown,
}

#[derive(Debug, Default, Deserialize)]
pub struct RawSettings {
    pub log_level: Option<String>,
    #[serde(flatten)]
    pub unknown: Unknown,
}

#[derive(Debug, Default, Deserialize)]
pub struct RawProfile {
    pub name: Option<String>,
    #[serde(default)]
    pub watch: Vec<RawWatch>,
    #[serde(default)]
    pub ignore: Vec<String>,
    #[serde(default)]
    pub reinclude: Vec<String>,
    #[serde(default)]
    pub remotes: Vec<RawRemote>,
    pub schedule: Option<RawSchedule>,
    pub use_default_ignores: Option<bool>,
    pub store_path: Option<String>,
    pub local_only: Option<bool>,
    #[serde(flatten)]
    pub unknown: Unknown,
}

/// A bare string or `{ path, name }`, matching `WatchEntry`'s two shapes.
#[derive(Debug, Deserialize)]
#[serde(untagged)]
pub enum RawWatch {
    Bare(String),
    Named {
        path: String,
        name: String,
        #[serde(flatten)]
        unknown: Unknown,
    },
}

#[derive(Debug, Deserialize)]
pub struct RawRemote {
    pub name: Option<String>,
    pub path: Option<String>,
    pub optional: Option<bool>,
    pub behind_tolerance: Option<u32>,
    pub trust_ownership: Option<bool>,
    #[serde(flatten)]
    pub unknown: Unknown,
}

/// Externally tagged, so `{ weekly = { .. } }` maps onto the variant directly and a
/// table carrying two of the three is rejected by serde rather than by a check that
/// could be forgotten - which is what `scheduling.md` means by enforcing it in the
/// type.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RawSchedule {
    Daily { at: String },
    Weekly { day: String, at: String },
    Every(String),
}

impl RawWatch {
    #[must_use]
    pub fn path(&self) -> &str {
        match self {
            Self::Bare(path) | Self::Named { path, .. } => path,
        }
    }
}
