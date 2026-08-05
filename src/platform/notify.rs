//! Desktop notifications: one of the four channels behind "failure is loud", and the
//! only one whose delivery depends on an authorisation that can silently be off.
//!
//! **A CLI has no notification identity of its own**, on any platform. macOS wants an
//! installed app bundle and Windows wants a registered `AppUserModelID`; a binary in
//! `~/.cargo/bin` has neither. The pattern that works on both is the same: shell out
//! to the platform's scripting host and borrow its identity.
//!
//! On macOS that is `osascript`, so the banner is attributed to Script Editor and its
//! delivery depends on Script Editor's own notification permission. On Windows it is
//! `powershell.exe`, whose AUMID the toast is raised under, so the banner says
//! "Windows PowerShell" and Focus Assist or that entry's own notification setting can
//! suppress it. `doctor` reports the borrowed identity rather than hiding it.
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
    let script = format!(
        "display notification \"{}\" with title \"{}\"",
        applescript_literal(body),
        applescript_literal(urgency.title())
    );

    let out = command("osascript", &["-e", &script], Timeout::INTERPRETER)?;
    if out.status.success() {
        return Ok(());
    }
    Err(NotifyError::Rejected(
        "osascript".to_owned(),
        String::from_utf8_lossy(&out.stderr).trim().to_owned(),
    ))
}

/// AppleScript string literals take backslash and double-quote escapes and nothing
/// else, so a message carrying either would end the literal early and turn the rest
/// into script. Every body is Tycho's own text, but a remote name or a path reaches
/// it, and those are not.
///
/// A named function rather than a closure inside `notify` so its test can reach the
/// shipping code: the test used to redefine the same closure and assert on its own
/// copy, which could not fail however the real one changed.
#[cfg(target_os = "macos")]
fn applescript_literal(text: &str) -> String {
    text.replace('\\', r"\\").replace('"', "\\\"")
}

/// Windows PowerShell's own `AppUserModelID`, which is what the toast is attributed
/// to and whose notification setting decides whether it is shown.
///
/// `powershell.exe` and not `pwsh`: the WinRT projection these types need is in
/// Windows PowerShell 5.1, and PowerShell 7 reaches it only through a compatibility
/// shim that is not installed by default.
#[cfg(windows)]
const POWERSHELL_AUMID: &str =
    r"{1AC14E77-02E7-4E5D-B744-2EB1AE5198B7}\WindowsPowerShell\v1.0\powershell.exe";

/// Sends one notification.
///
/// # Errors
///
/// If PowerShell cannot be run or refuses. **Callers should not fail a run over
/// this**: Focus Assist suppresses delivery, which is expected.
#[cfg(windows)]
pub fn notify(urgency: Urgency, body: &str) -> Result<(), NotifyError> {
    let out = command(
        "powershell",
        &[
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            &toast_script(urgency.title(), body),
        ],
        Timeout::INTERPRETER,
    )?;
    if out.status.success() {
        return Ok(());
    }
    Err(NotifyError::Rejected(
        "powershell".to_owned(),
        String::from_utf8_lossy(&out.stderr).trim().to_owned(),
    ))
}

/// The script that raises the toast.
///
/// The text crosses two parsers on its way in, so it is escaped for both: XML,
/// because it becomes element content, and PowerShell, because the XML then sits in a
/// single-quoted literal. Escaping for one and not the other is how a remote named
/// `it's` or a path holding `&` would turn the document into a syntax error - or
/// worse, into script.
#[cfg(windows)]
fn toast_script(title: &str, body: &str) -> String {
    let toast = format!(
        "<toast><visual><binding template=\"ToastText02\"><text id=\"1\">{}</text>\
         <text id=\"2\">{}</text></binding></visual></toast>",
        xml_escape(title),
        xml_escape(body)
    );
    format!(
        "$ErrorActionPreference = 'Stop'\n\
         [Windows.UI.Notifications.ToastNotificationManager, Windows.UI.Notifications, \
         ContentType = WindowsRuntime] | Out-Null\n\
         $xml = [Windows.Data.Xml.Dom.XmlDocument, Windows.Data.Xml.Dom.XmlDocument, \
         ContentType = WindowsRuntime]::new()\n\
         $xml.LoadXml('{}')\n\
         $toast = [Windows.UI.Notifications.ToastNotification]::new($xml)\n\
         [Windows.UI.Notifications.ToastNotificationManager]::CreateToastNotifier('{}')\
         .Show($toast)\n",
        powershell_literal(&toast),
        POWERSHELL_AUMID
    )
}

/// The five characters XML gives meaning to, plus the control characters XML 1.0
/// cannot carry at all - a `\u{1}` in a remote name would otherwise make the document
/// unparsable rather than making the banner look odd.
#[cfg(windows)]
fn xml_escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for character in text.chars() {
        match character {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            c if (c as u32) < 0x20 && !matches!(c, '\t' | '\n' | '\r') => out.push(' '),
            c => out.push(c),
        }
    }
    out
}

/// A single-quoted PowerShell string ends at the next `'`, and doubling is the only
/// escape it has.
#[cfg(windows)]
fn powershell_literal(text: &str) -> String {
    text.replace('\'', "''")
}

/// # Errors
///
/// Always [`NotifyError::Unsupported`].
#[cfg(not(any(target_os = "macos", windows)))]
pub fn notify(_urgency: Urgency, _body: &str) -> Result<(), NotifyError> {
    Err(NotifyError::Unsupported)
}

/// Whether the mechanism is present at all, for `doctor`'s row.
///
/// Deliberately **not** a delivery test: sending a notification to find out whether
/// notifications work is a side effect nobody asked a health check for. `doctor
/// --deep` sends one, and says so.
#[cfg(target_os = "macos")]
#[must_use]
pub fn available() -> bool {
    command("osascript", &["-e", "return 1"], Timeout::INTERPRETER)
        .is_ok_and(|out| out.status.success())
}

/// As the macOS arm. Asks only that PowerShell runs - whether a toast is *shown*
/// depends on Focus Assist and on the notification setting for PowerShell's own
/// AppUserModelID, which is the same borrowed-identity caveat `doctor` reports there.
#[cfg(windows)]
#[must_use]
pub fn available() -> bool {
    command(
        "powershell",
        &["-NoProfile", "-NonInteractive", "-Command", "exit 0"],
        Timeout::INTERPRETER,
    )
    .is_ok_and(|out| out.status.success())
}

#[cfg(not(any(target_os = "macos", windows)))]
#[must_use]
pub fn available() -> bool {
    false
}

#[cfg(all(test, windows))]
mod windows_tests {
    use super::{Urgency, notify, toast_script, xml_escape};

    /// Both parsers, in the order the text meets them.
    #[test]
    fn a_hostile_body_survives_xml_and_then_powershell() {
        let script = toast_script("Tycho: backup failed", "remote 't7' & <drive> failed");
        // XML first: nothing that could close an element is left as itself.
        assert!(script.contains("&amp;"), "{script}");
        assert!(script.contains("&lt;drive&gt;"), "{script}");
        // Then PowerShell: the apostrophes XML turned into `&apos;` are gone, and the
        // literal's own quotes are the only bare ones left.
        assert!(!script.contains("'t7'"), "{script}");
        assert_eq!(
            script.matches("&apos;").count(),
            2,
            "both quotes became entities: {script}"
        );
    }

    #[test]
    fn a_control_character_cannot_reach_the_document() {
        assert_eq!(xml_escape("a\u{1}b"), "a b");
        assert_eq!(
            xml_escape("keeps\tthe\nallowed\rones"),
            "keeps\tthe\nallowed\rones"
        );
    }

    /// The one that had never been run. Raising it is the whole point of the arm, and
    /// a toast that compiles and is never fired is the mistake this project keeps
    /// finding it made.
    ///
    /// Delivery is not asserted: Focus Assist may suppress it, which is expected and
    /// is why the notification is the convenience rather than the contract. What is
    /// asserted is that Windows accepted it - and `toast_script` is proved to fail
    /// loudly by the malformed case below, so acceptance is not vacuous.
    #[test]
    fn a_toast_is_accepted_by_windows() {
        notify(
            Urgency::Info,
            "tycho self-test: this banner came from the test suite",
        )
        .expect("Windows accepted the toast");
    }

    /// So that the test above means something: a document Windows will not parse
    /// comes back as an error rather than as silence.
    #[test]
    fn a_document_windows_rejects_is_reported() {
        let broken = "$ErrorActionPreference = 'Stop'\n\
             [Windows.UI.Notifications.ToastNotificationManager, Windows.UI.Notifications, \
             ContentType = WindowsRuntime] | Out-Null\n\
             $xml = [Windows.Data.Xml.Dom.XmlDocument, Windows.Data.Xml.Dom.XmlDocument, \
             ContentType = WindowsRuntime]::new()\n\
             $xml.LoadXml('<toast><visual>')\n";
        let out = crate::sys::process::command(
            "powershell",
            &["-NoProfile", "-NonInteractive", "-Command", broken],
            crate::sys::process::Timeout::INTERPRETER,
        )
        .expect("powershell runs");
        assert!(
            !out.status.success(),
            "a malformed toast must not pass for a sent one"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::Urgency;

    /// A body carrying a quote would otherwise end the AppleScript literal early and
    /// turn the remainder into script. Remote names and paths reach this text.
    #[cfg(target_os = "macos")]
    #[test]
    fn the_escape_rule_is_what_applescript_actually_accepts() {
        use super::applescript_literal as escape;

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
