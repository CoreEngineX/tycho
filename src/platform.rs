//! Layer 5. Paths, launchd plist generation, notifications, and the Full Disk Access
//! probe.

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
