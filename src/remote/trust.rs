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

use crate::config::Remote;
use crate::primitives::path::AbsPath;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

/// Writes the scoped config and installs it for this process.
///
/// Does nothing when no remote asked, so the ordinary case adds no file and no
/// behaviour.
///
/// # Errors
///
/// If the file cannot be written.
pub fn install(remotes: &[Remote], profile: &str, dir: &Path) -> std::io::Result<Option<PathBuf>> {
    let trusted: Vec<&Remote> = remotes.iter().filter(|r| r.trust_ownership).collect();
    if trusted.is_empty() {
        return Ok(None);
    }

    let mut text = String::new();
    // The user's own global config first, so trusting a remote costs them none of
    // their settings. `GIT_CONFIG_GLOBAL` replaces rather than adds.
    if let Some(theirs) = global_config()
        && theirs.exists()
    {
        let _ = writeln!(text, "[include]\n\tpath = {}", escape(&theirs));
    }
    text.push_str("[safe]\n");
    for remote in &trusted {
        // Both spellings: git matches `safe.directory` literally, and it reports the
        // path with backslashes on Windows while a config file written with forward
        // slashes will not match. Measured - the forward-slash form alone let the
        // classification through and the push still failed.
        let repo = super::repo_path(remote.path.as_path(), profile);
        let _ = writeln!(text, "\tdirectory = {}", escape(&repo));
        let swapped = repo.display().to_string().replace('\\', "/");
        let _ = writeln!(text, "\tdirectory = {swapped}");
    }

    std::fs::create_dir_all(dir)?;
    let path = dir.join("trusted-remotes.gitconfig");
    crate::sys::fs::write_atomic(&path, text.as_bytes())?;
    crate::sys::process::trust_config(path.clone());
    Ok(Some(path))
}

/// A git config value ends at a newline and takes `\` as an escape, so a Windows path
/// written raw would lose every separator to the next character.
fn escape(path: &Path) -> String {
    path.display().to_string().replace('\\', "\\\\")
}

/// Where the user's own global config lives, asked of git rather than assumed: it is
/// `~/.gitconfig` or `$XDG_CONFIG_HOME/git/config` depending on which exists.
fn global_config() -> Option<PathBuf> {
    let home = std::env::home_dir()?;
    let candidate = home.join(".gitconfig");
    if candidate.exists() {
        return Some(candidate);
    }
    AbsPath::parse("~/.config/git/config")
        .ok()
        .map(AbsPath::into_path_buf)
}

#[cfg(test)]
mod tests {
    use super::escape;
    use std::path::Path;

    /// A raw `C:\Users\me` in a config value loses `\U` and `\m` to git's own escape
    /// handling, and the entry then matches nothing.
    #[test]
    fn a_windows_path_is_escaped_for_the_config_parser() {
        assert_eq!(escape(Path::new(r"C:\Users\me")), r"C:\\Users\\me");
        assert_eq!(escape(Path::new("/home/me")), "/home/me");
    }
}
