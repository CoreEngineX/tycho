//! Layer 5. Paths, launchd plist generation, notifications, and the Full Disk Access
//! probe.

pub mod launchd;
pub mod log;
pub mod notify;

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

/// Where the data Apple's conventions do cover lives. Joined by hand rather than
/// through `directories`, because `store.md`'s path table is the contract and a
/// crate would have to be verified against it anyway.
///
/// # Errors
///
/// If there is no home directory.
pub fn data_dir() -> Result<AbsPath, PathError> {
    AbsPath::parse("~/Library/Application Support/tycho")
}

/// `<data>/store/<profile>.git`, unless the profile overrides the directory.
///
/// # Errors
///
/// If there is no home directory.
pub fn store_path(profile: &str, override_dir: Option<&AbsPath>) -> Result<AbsPath, PathError> {
    let dir = match override_dir {
        Some(dir) => dir.clone(),
        None => AbsPath::parse("~/Library/Application Support/tycho/store")?,
    };
    AbsPath::from_absolute(&dir.as_path().join(format!("{profile}.git")))
}

/// # Errors
///
/// If there is no home directory.
pub fn state_path() -> Result<AbsPath, PathError> {
    AbsPath::parse("~/Library/Application Support/tycho/state.json")
}

/// # Errors
///
/// If there is no home directory.
pub fn log_dir() -> Result<AbsPath, PathError> {
    AbsPath::parse("~/Library/Logs/tycho")
}
