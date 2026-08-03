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
/// reasoning `notify` records.
#[cfg(target_os = "macos")]
pub use launchd as scheduler;
#[cfg(windows)]
pub use schtasks as scheduler;

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
