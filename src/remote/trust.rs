//! Opting a remote out of git's ownership guard, for one process and no longer.
//!
//! Git refuses a repository on a filesystem that records no ownership - exFAT and
//! FAT32, which is what an external drive usually is. The refusal is
//! `safe.directory`, and it exists because a repository on removable media can be
//! attacker-controlled: `git push` to a local path runs `receive-pack` there, and
//! that runs the remote's own hooks.
//!
//! So Tycho does not decide this. A remote carries `trust_ownership = true` or it does
//! not, and the default is not to. What this module removes is only the *cost* of
//! saying yes: the remedy git prints is `git config --global --add safe.directory
//! <path>`, which is permanent, machine-wide, and applies to every tool the user runs.
//! This applies the same exception to Tycho's own git invocations and nothing else.

use crate::config::Config;
use crate::primitives::path::AbsPath;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

/// Writes the scoped config and installs it for this process.
///
/// Takes the whole config rather than one profile's remotes because
/// [`crate::sys::process::trust_config`] is process-wide and accepts one file: writing
/// it per profile left a profile that trusts nothing running under the *previous*
/// profile's file, having opted into nothing.
///
/// Does nothing when no remote asked, so the ordinary case adds no file and no
/// behaviour.
///
/// # Errors
///
/// If the file cannot be written.
pub fn install(config: &Config, dir: &Path) -> std::io::Result<Option<PathBuf>> {
    let mut directories = Vec::new();
    for profile in &config.profiles {
        for remote in profile.remotes.iter().filter(|r| r.trust_ownership) {
            // Resolved first, because `remote.path` may carry the single `*` that
            // names a cloud folder whose account is in its name. Git matches a
            // `safe.directory` value literally - `*` is special only as the whole
            // value - so the unresolved form names nothing and the guard still fires.
            // Skipped rather than guessed when the glob does not resolve: the folder
            // is not there yet, and `publish` will report that on its own.
            if let Ok(folder) = super::resolve(&remote.path) {
                directories.push(super::repo_path(&folder, profile.name.as_str()));
            }
        }
    }
    if directories.is_empty() {
        return Ok(None);
    }

    let mut text = String::new();
    // The user's own global config first, so trusting a remote costs them none of
    // their settings. `GIT_CONFIG_GLOBAL` replaces rather than adds, and an included
    // `safe.directory` is honoured - measured, because git restricts where that key
    // may be read from and an include is the kind of indirection that restriction
    // targets.
    for theirs in global_configs() {
        let _ = writeln!(text, "[include]\n\tpath = {}", value(&theirs));
    }
    text.push_str("[safe]\n");
    for repo in &directories {
        let _ = writeln!(text, "\tdirectory = {}", value(repo));
        // Git reports the path with backslashes on Windows and a forward-slash entry
        // does not match it - measured there, where the forward-slash form alone let
        // the classification through and the push still failed. Windows-only because
        // `\` is a legal byte in a Unix filename, so rewriting one here would name a
        // second path that does not exist.
        #[cfg(windows)]
        {
            let swapped = PathBuf::from(repo.display().to_string().replace('\\', "/"));
            let _ = writeln!(text, "\tdirectory = {}", value(&swapped));
        }
    }

    std::fs::create_dir_all(dir)?;
    let path = dir.join("trusted-remotes.gitconfig");
    crate::sys::fs::write_atomic(&path, text.as_bytes())?;
    crate::sys::process::trust_config(path.clone());
    Ok(Some(path))
}

/// A path as a git config value: always quoted.
///
/// Quoting rather than escaping the characters that need it, because there are four
/// of them and enumerating them is how one gets missed. Unquoted, `#` and `;` start a
/// comment and trailing whitespace is stripped, so `/Volumes/Backup #2/` truncates to
/// `/Volumes/Backup` and matches nothing; a `\` is git's own escape, so a Windows path
/// loses every separator to the character after it. Inside quotes only `\` and `"`
/// carry meaning. Measured: git accepts a quoted `safe.directory` value.
fn value(path: &Path) -> String {
    let escaped = path
        .display()
        .to_string()
        .replace('\\', r"\\")
        .replace('"', "\\\"");
    format!("\"{escaped}\"")
}

/// Every file git reads as global config, in the order git reads them.
///
/// **Both**, not the first that exists. Git reads `$XDG_CONFIG_HOME/git/config` and
/// then `~/.gitconfig`, and `GIT_CONFIG_GLOBAL` suppresses both - so returning one
/// silently drops the other's settings for the run, and `~/.config/git/config`
/// alongside `~/.gitconfig` is an ordinary layout rather than an exotic one.
fn global_configs() -> Vec<PathBuf> {
    let xdg = std::env::var_os("XDG_CONFIG_HOME")
        .map(|dir| PathBuf::from(dir).join("git/config"))
        .or_else(|| {
            AbsPath::parse("~/.config/git/config")
                .ok()
                .map(AbsPath::into_path_buf)
        });
    let home = std::env::home_dir().map(|home| home.join(".gitconfig"));

    xdg.into_iter()
        .chain(home)
        .filter(|path| path.exists())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::value;
    use std::path::Path;

    /// A raw `C:\Users\me` in a config value loses `\U` and `\m` to git's own escape
    /// handling, and the entry then matches nothing.
    #[test]
    fn a_windows_path_is_escaped_for_the_config_parser() {
        assert_eq!(value(Path::new(r"C:\Users\me")), r#""C:\\Users\\me""#);
    }

    /// The characters an unquoted value would lose the path to: `#` and `;` begin a
    /// comment, and trailing whitespace is stripped. All three are legal in a
    /// `/Volumes` name.
    #[test]
    fn a_comment_character_or_a_trailing_space_survives_quoting() {
        assert_eq!(
            value(Path::new("/Volumes/Backup #2")),
            "\"/Volumes/Backup #2\""
        );
        assert_eq!(value(Path::new("/Volumes/a;b")), "\"/Volumes/a;b\"");
        assert_eq!(value(Path::new("/Volumes/trail ")), "\"/Volumes/trail \"");
    }
}
