//! Desktop notifications: one of the four channels behind "failure is loud", and the
//! only one whose delivery depends on an authorisation that can silently be off.
//!
//! **A CLI has no notification identity of its own**, on any platform. macOS wants an
//! installed app bundle and Windows wants a registered `AppUserModelID`; a binary in
//! `~/.cargo/bin` has neither. The pattern that works on both is the same: shell out
//! to the platform's scripting host and borrow its identity. Here that is `osascript`,
//! so the banner is attributed to Script Editor and its delivery depends on Script
//! Editor's own notification permission. `doctor` reports that rather than hiding it.
//!
//! A crate does not fix this. `notify-rust` hits the same AUMID requirement on Windows
//! and binds the deprecated `NSUserNotification` on macOS; it moves the caveat behind
//! a dependency instead of removing it.
//!
//! This is why the notification is explicitly the **convenience**, not the contract.
//! The non-zero exit code, the red `status` line and the state-file record are what
//! actually carry a failure.

use crate::sys::process::{Timeout, command};

#[derive(Debug, thiserror::Error)]
pub enum NotifyError {
    #[error(transparent)]
    Run(#[from] crate::sys::process::RunError),
    #[error("{0} rejected the notification: {1}")]
    Rejected(String, String),
    /// Not an error the caller should treat as a failure - it means the platform's
    /// arm is not written yet. A typed variant rather than a silent `Ok(())`, so a
    /// future port finds a failing call rather than a lie.
    #[error("desktop notifications are not implemented for this platform yet")]
    Unsupported,
}

/// How loud a notification is. macOS has no severity on a banner, so this only
/// decides the wording - which is the honest place for it, since a person reading a
/// banner reads words, not a level.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Urgency {
    Failure,
    Warning,
    Info,
}

impl Urgency {
    const fn title(self) -> &'static str {
        match self {
            Self::Failure => "Tycho: backup failed",
            Self::Warning => "Tycho: needs attention",
            Self::Info => "Tycho",
        }
    }
}

/// Sends one notification.
///
/// # Errors
///
/// If the scripting host cannot be run or refuses. **Callers should not fail a run
/// over this**: a machine in a Focus mode suppresses delivery, which is expected.
#[cfg(target_os = "macos")]
pub fn notify(urgency: Urgency, body: &str) -> Result<(), NotifyError> {
    // AppleScript string literals take backslash and double-quote escapes and nothing
    // else, so a message carrying either would otherwise end the literal early and
    // turn the rest into script. Every body here is Tycho's own text, but a remote
    // name or a path reaches it, and those are not.
    let escape = |text: &str| text.replace('\\', r"\\").replace('"', "\\\"");
    let script = format!(
        "display notification \"{}\" with title \"{}\"",
        escape(body),
        escape(urgency.title())
    );

    let out = command("osascript", &["-e", &script], Timeout::QUICK)?;
    if out.status.success() {
        return Ok(());
    }
    Err(NotifyError::Rejected(
        "osascript".to_owned(),
        String::from_utf8_lossy(&out.stderr).trim().to_owned(),
    ))
}

/// The Windows arm is a decision recorded in `scheduling.md` section 10, not code:
/// a toast raised through `powershell` with PowerShell's own `AppUserModelID`. It is
/// deliberately unwritten until there is a Windows machine to run it on, because
/// unverified code that compiles is exactly the kind of thing this project keeps
/// finding was wrong.
///
/// # Errors
///
/// Always [`NotifyError::Unsupported`].
#[cfg(not(target_os = "macos"))]
pub fn notify(_urgency: Urgency, _body: &str) -> Result<(), NotifyError> {
    Err(NotifyError::Unsupported)
}

/// Whether the mechanism is present at all, for `doctor`'s row.
///
/// Deliberately **not** a delivery test: sending a notification to find out whether
/// notifications work is a side effect nobody asked a health check for. `doctor
/// --deep` sends one, and says so.
#[must_use]
pub fn available() -> bool {
    cfg!(target_os = "macos")
        && command("osascript", &["-e", "return 1"], Timeout::QUICK)
            .is_ok_and(|out| out.status.success())
}

#[cfg(test)]
mod tests {
    use super::Urgency;

    /// A body carrying a quote would otherwise end the AppleScript literal early and
    /// turn the remainder into script. Remote names and paths reach this text.
    #[test]
    fn the_escape_rule_is_what_applescript_actually_accepts() {
        let escape = |text: &str| text.replace('\\', r"\\").replace('"', "\\\"");
        assert_eq!(escape(r#"say "hi""#), r#"say \"hi\""#);
        assert_eq!(escape(r"C:\path"), r"C:\\path");
    }

    #[test]
    fn each_urgency_says_something_different() {
        let titles = [Urgency::Failure, Urgency::Warning, Urgency::Info];
        let mut seen: Vec<&str> = titles.iter().map(|u| u.title()).collect();
        seen.sort_unstable();
        seen.dedup();
        assert_eq!(seen.len(), 3, "a banner is read as words, not as a level");
    }
}
