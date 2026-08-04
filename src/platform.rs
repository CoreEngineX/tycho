//! Layer 5. Paths, launchd plist generation, notifications, and the Full Disk Access
//! probe.

pub mod launchd;
pub mod log;
pub mod notify;
#[cfg(windows)]
pub mod schtasks;

/// The lifecycle vocabulary is Tycho's own, not any one scheduler's: which agent,
/// what it runs, whether it is registered, and how its last invocation ended. It is
/// declared in `launchd` for want of a better home and re-exported here so the
/// Windows backend and `service` name it without importing the macOS module.
pub use launchd::{Agent, Ended, Job, Loaded};

/// The scheduler this platform actually has.
///
/// A `#[cfg]`-selected module rather than a trait: there is exactly one
/// implementation per target and it is chosen at compile time, which is the same
/// reasoning `notify` records. What a trait would have bought - a checked surface -
/// is bought instead by the private `contract` module below, at no run-time cost and
/// with no `dyn`.
#[cfg(target_os = "macos")]
pub use launchd as scheduler;
#[cfg(windows)]
pub use schtasks as scheduler;

/// The eleven items `service` and `doctor` call on [`scheduler`], declared once.
///
/// A module alias is duck-typed: nothing makes `launchd` and `schtasks` agree, so a
/// signature that drifted was a discovery on the *other* machine rather than an error
/// on this one. Coercing each to a function pointer of the declared type turns that
/// into a compile error on whichever host builds - which is the whole of what a trait
/// would have given, without an indirection for a choice made at compile time.
///
/// It costs nothing at run time: every item here is discarded, and a `const` binding
/// of a function pointer emits no code.
#[cfg(any(target_os = "macos", windows))]
mod contract {
    use super::{Agent, Job, Loaded, scheduler};
    use crate::config::Schedule;
    use crate::primitives::path::{AbsPath, PathError};
    use crate::sys::process::RunError;
    use std::path::{Path, PathBuf};

    const _: fn() -> Result<AbsPath, PathError> = scheduler::definitions_dir;
    const _: fn(&Agent) -> Result<PathBuf, PathError> = scheduler::definition_path;
    const _: fn(&Job<'_>, Schedule) -> String = scheduler::backup_definition;
    const _: fn(&Job<'_>) -> String = scheduler::catchup_definition;
    const _: fn(&Job<'_>) -> String = scheduler::probe_definition;
    const _: fn(&str) -> Vec<u8> = scheduler::encode;
    const _: fn(&[u8]) -> String = scheduler::decode;
    const _: fn(&Agent, &Path) -> Result<(), scheduler::Error> = scheduler::register;
    const _: fn(&Agent) -> Result<(), scheduler::Error> = scheduler::deregister;
    const _: fn(&Agent) -> Result<Loaded, RunError> = scheduler::state;
    const _: fn(&str) -> Option<Schedule> = scheduler::scheduled_in;

    /// `service` bridges a `state` failure into the scheduler's own error, which only
    /// works because both carry a `#[from] RunError`. Nothing else checked that.
    const _: fn(RunError) -> scheduler::Error = scheduler::Error::from;
}

use crate::primitives::path::{AbsPath, PathError};

/// The config file a human is expected to open and edit.
///
/// Built from the home directory plus `.config/tycho`, deliberately rather than from
/// the `directories` crate, whose `config_dir()` returns `~/Library/Application
/// Support` on macOS. The state, store and log paths do follow Apple's conventions;
/// a hand-edited file does not.
///
/// # Errors
///
/// If there is no home directory.
pub fn config_path() -> Result<AbsPath, PathError> {
    AbsPath::parse("~/.config/tycho/tycho.toml")
}

/// Where the data the platform's conventions do cover lives. Joined by hand rather
/// than through `directories`, because `store.md`'s path table is the contract and a
/// crate would have to be verified against it anyway.
///
/// `~/AppData/Local` on Windows rather than `Roaming`: the store is a git repository
/// that grows without bound, and Roaming is copied to a domain controller at every
/// logon. `store.md` section 2 records the split.
#[cfg(target_os = "macos")]
const DATA_DIR: &str = "~/Library/Application Support/tycho";
#[cfg(windows)]
const DATA_DIR: &str = "~/AppData/Local/tycho";

#[cfg(target_os = "macos")]
const LOG_DIR: &str = "~/Library/Logs/tycho";
#[cfg(windows)]
const LOG_DIR: &str = "~/AppData/Local/tycho/logs";

/// # Errors
///
/// If there is no home directory.
pub fn data_dir() -> Result<AbsPath, PathError> {
    AbsPath::parse(DATA_DIR)
}

/// `<data>/store/<profile>.git`, unless the profile overrides the directory.
///
/// # Errors
///
/// If there is no home directory.
pub fn store_path(profile: &str, override_dir: Option<&AbsPath>) -> Result<AbsPath, PathError> {
    let dir = match override_dir {
        Some(dir) => dir.clone(),
        None => AbsPath::parse(&format!("{DATA_DIR}/store"))?,
    };
    AbsPath::from_absolute(&dir.as_path().join(format!("{profile}.git")))
}

/// # Errors
///
/// If there is no home directory.
pub fn state_path() -> Result<AbsPath, PathError> {
    AbsPath::parse(&format!("{DATA_DIR}/state.json"))
}

/// # Errors
///
/// If there is no home directory.
pub fn log_dir() -> Result<AbsPath, PathError> {
    AbsPath::parse(LOG_DIR)
}
