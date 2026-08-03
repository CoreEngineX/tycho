//! Layer 5. Agent labels, the plists Tycho generates, and the `launchctl` lifecycle.
//!
//! The plist is generated, never hand-written. A hand-written plist that had drifted
//! from what the script needed is a large part of how the old system failed unnoticed.

use crate::config::{Schedule, Weekday};
use crate::primitives::path::{AbsPath, PathError};
use crate::sys::process::{RunError, Timeout, command};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

#[derive(Debug, thiserror::Error)]
pub enum LaunchdError {
    #[error(transparent)]
    Run(#[from] RunError),
    #[error(transparent)]
    Path(#[from] PathError),
    #[error("{context}: {source}")]
    Io {
        context: String,
        #[source]
        source: std::io::Error,
    },
    #[error("launchctl {action} refused {label}: {detail}")]
    Refused {
        action: String,
        label: String,
        detail: String,
    },
}

/// The publisher's namespace, not the person running it. `com.apple.*` and
/// `com.google.keystone` run on machines belonging to people who work at neither, so
/// a branded label is correct precisely because strangers will use it. A neutral
/// namespace would claim a name nobody owns and let two unrelated tools collide.
const PREFIX: &str = "com.coreenginex.tycho";

/// Which agent a label names.
///
/// A sum type rather than a string, because the `profile.` infix is the whole of the
/// collision safety: deriving `com.coreenginex.tycho.<profile>` straight from a
/// user-controlled name means a profile called `catchup` produces exactly the
/// catch-up agent's label, and `Label` uniquely identifies a job - so one silently
/// displaces the other.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Agent {
    /// One per profile, on the profile's own schedule.
    Backup(String),
    /// One shared, pushing whatever is behind. Never captures.
    Catchup,
    /// Transient, bootstrapped by `doctor --deep` to measure the agent's own grant.
    Probe,
}

impl Agent {
    #[must_use]
    pub fn label(&self) -> String {
        match self {
            Self::Backup(profile) => format!("{PREFIX}.profile.{profile}"),
            Self::Catchup => format!("{PREFIX}.catchup"),
            Self::Probe => format!("{PREFIX}.probe"),
        }
    }

    /// `~/Library/LaunchAgents/<label>.plist`.
    ///
    /// # Errors
    ///
    /// If there is no home directory.
    pub fn plist_path(&self) -> Result<PathBuf, PathError> {
        Ok(agents_dir()?
            .as_path()
            .join(format!("{}.plist", self.label())))
    }
}

/// # Errors
///
/// If there is no home directory.
pub fn agents_dir() -> Result<AbsPath, PathError> {
    AbsPath::parse("~/Library/LaunchAgents")
}

/// The account whose GUI domain the agents live in.
///
/// Read from the home directory's owner rather than `libc::getuid`, which
/// `unsafe_code = "forbid"` rules out, and rather than `$UID`, which is a shell
/// variable launchd does not set. This cannot disagree with the account whose
/// `~/Library/LaunchAgents` is being written to, because it *is* that directory's
/// owner.
///
/// # Errors
///
/// If there is no home directory, or it cannot be read.
pub fn uid() -> Result<u32, LaunchdError> {
    use std::os::unix::fs::MetadataExt as _;
    let home = std::env::home_dir().ok_or(PathError::NoHome)?;
    let meta = std::fs::metadata(&home).map_err(|source| LaunchdError::Io {
        context: format!("reading {}", home.display()),
        source,
    })?;
    Ok(meta.uid())
}

/// Escapes the five characters XML gives meaning to.
///
/// A watched path may contain `&` or `<`. A plist that silently truncated at one
/// would schedule a backup of the wrong tree, at exit 0.
#[must_use]
pub fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for character in text.chars() {
        match character {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            other => out.push(other),
        }
    }
    out
}

/// Everything a plist needs that is not the schedule.
#[derive(Clone, Debug)]
pub struct Job<'a> {
    pub agent: &'a Agent,
    /// Resolved and absolute, written at install time, so the plist never depends on
    /// a `PATH` launchd does not have.
    pub program: &'a Path,
    pub arguments: Vec<String>,
    pub log_dir: &'a Path,
    /// The stem of the two log files, so a profile's agent and the shared catch-up
    /// agent do not write to the same pair.
    pub log_stem: String,
}

/// Renders a backup agent's plist.
///
/// `RunAtLoad` is **false** deliberately: a backup on every login is not what a
/// weekly schedule means, and the wake-up catch-up plus the overdue check cover a
/// missed run.
#[must_use]
pub fn backup_plist(job: &Job<'_>, schedule: Schedule) -> String {
    let mut body = String::new();
    match schedule {
        Schedule::Daily { at } => {
            let _ = write!(
                body,
                "  <key>StartCalendarInterval</key>\n  <dict>\n\
                 \x20   <key>Hour</key><integer>{}</integer>\n\
                 \x20   <key>Minute</key><integer>{}</integer>\n  </dict>\n",
                at.hour, at.minute
            );
        }
        Schedule::Weekly { day, at } => {
            let _ = write!(
                body,
                "  <key>StartCalendarInterval</key>\n  <dict>\n\
                 \x20   <key>Weekday</key><integer>{}</integer>\n\
                 \x20   <key>Hour</key><integer>{}</integer>\n\
                 \x20   <key>Minute</key><integer>{}</integer>\n  </dict>\n",
                Weekday::launchd(day),
                at.hour,
                at.minute
            );
        }
        Schedule::Every(interval) => {
            let _ = writeln!(
                body,
                "  <key>StartInterval</key><integer>{}</integer>",
                interval.as_secs()
            );
        }
    }
    body.push_str("  <key>RunAtLoad</key><false/>\n");
    render(job, &body)
}

/// Renders the shared catch-up agent's plist.
///
/// **`RunAtLoad` is deliberately absent**, though an earlier draft had it. At login,
/// cloud File Providers may not have mounted, so remote paths do not exist yet and
/// every remote would be probed as unreachable on every boot. `StartOnMount` already
/// fires when those domains mount, which is strictly better timing.
#[must_use]
pub fn catchup_plist(job: &Job<'_>) -> String {
    // StartOnMount fires on every mount - a DMG, a network share, an APFS local
    // snapshot - so ThrottleInterval bounds the churn. The job exits before touching
    // the filesystem when nothing is behind, so the common case is a process spawn.
    render(
        job,
        "  <key>StartOnMount</key><true/>\n\
         \x20 <key>StartInterval</key><integer>3600</integer>\n\
         \x20 <key>ThrottleInterval</key><integer>10</integer>\n",
    )
}

/// Renders the transient probe agent's plist. It runs once, on load, and that is the
/// whole point: it exists to be bootstrapped, observed, and booted out.
#[must_use]
pub fn probe_plist(job: &Job<'_>) -> String {
    render(job, "  <key>RunAtLoad</key><true/>\n")
}

fn render(job: &Job<'_>, extra: &str) -> String {
    let mut out = String::from(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
         <!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \
         \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n\
         <plist version=\"1.0\">\n<dict>\n",
    );
    let _ = writeln!(
        out,
        "  <key>Label</key>\n  <string>{}</string>",
        escape(&job.agent.label())
    );

    out.push_str("  <key>ProgramArguments</key>\n  <array>\n");
    let _ = writeln!(
        out,
        "    <string>{}</string>",
        escape(&job.program.display().to_string())
    );
    for argument in &job.arguments {
        let _ = writeln!(out, "    <string>{}</string>", escape(argument));
    }
    out.push_str("  </array>\n");

    // Both are set. launchd sends output to /dev/null when the path is absent, which
    // is a silent-failure surface in a tool whose whole thesis is that failure is
    // loud.
    for (key, suffix) in [("StandardOutPath", "out"), ("StandardErrorPath", "err")] {
        let path = job.log_dir.join(format!("{}.{suffix}.log", job.log_stem));
        let _ = writeln!(
            out,
            "  <key>{key}</key>\n  <string>{}</string>",
            escape(&path.display().to_string())
        );
    }

    out.push_str(extra);
    out.push_str("</dict>\n</plist>\n");
    out
}

/// What `launchctl list` says about one job.
///
/// `last_exit` is why this exists: a non-zero exit shown by `launchctl list` was the
/// evidence that revealed a year of silent failure, and nobody had reason to look.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Loaded {
    /// Not bootstrapped into the domain at all.
    No,
    /// Loaded, with how the last invocation ended.
    Yes { pid: Option<u32>, last: Ended },
}

/// How a job's last invocation finished.
///
/// A sum type because `launchctl list` reports a **raw wait status**, not an exit
/// code: `LastExitStatus = 256` means exit 1, and `= 9` means killed by SIGKILL.
/// Printing the raw number would have shown `exit 256` for an ordinary failure, which
/// is a number no `echo $?` ever produced and nobody could look up.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ended {
    Exit(i32),
    Signal(i32),
}

impl Ended {
    /// Decodes a `wait(2)` status the way the shell does.
    #[must_use]
    pub const fn from_wait_status(raw: i32) -> Self {
        let signal = raw & 0x7f;
        if signal == 0 {
            Self::Exit((raw >> 8) & 0xff)
        } else {
            Self::Signal(signal)
        }
    }

    #[must_use]
    pub const fn is_clean(self) -> bool {
        matches!(self, Self::Exit(0))
    }
}

impl std::fmt::Display for Ended {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Exit(0) => f.pad("loaded"),
            Self::Exit(code) => f.pad(&format!("exit {code}")),
            Self::Signal(signal) => f.pad(&format!("killed {signal}")),
        }
    }
}

/// Asks launchd about one label.
///
/// # Errors
///
/// If `launchctl` cannot be run at all. A label that is not loaded is
/// [`Loaded::No`], not an error.
pub fn state(agent: &Agent) -> Result<Loaded, RunError> {
    let label = agent.label();
    let out = command("launchctl", &["list", &label], Timeout::QUICK)?;
    if !out.status.success() {
        return Ok(Loaded::No);
    }

    // `launchctl list <label>` prints a plist-ish dict, one key per line.
    let text = String::from_utf8_lossy(&out.stdout);
    let field = |name: &str| -> Option<i64> {
        text.lines()
            .find_map(|line| {
                let (key, value) = line.split_once('=')?;
                key.trim().trim_matches('"').eq(name).then_some(value)
            })
            .and_then(|value| value.trim().trim_end_matches(';').trim().parse().ok())
    };

    Ok(Loaded::Yes {
        pid: field("PID").and_then(|pid| u32::try_from(pid).ok()),
        last: Ended::from_wait_status(
            field("LastExitStatus")
                .and_then(|raw| i32::try_from(raw).ok())
                .unwrap_or_default(),
        ),
    })
}

/// Loads a plist into the user's GUI domain.
///
/// # Errors
///
/// If `launchctl` fails.
pub fn bootstrap(agent: &Agent, plist: &Path) -> Result<(), LaunchdError> {
    let domain = format!("gui/{}", uid()?);
    let path = plist.display().to_string();
    let out = command("launchctl", &["bootstrap", &domain, &path], Timeout::WORK)?;
    if out.status.success() {
        return Ok(());
    }
    Err(LaunchdError::Refused {
        action: "bootstrap".to_owned(),
        label: agent.label(),
        detail: String::from_utf8_lossy(&out.stderr).trim().to_owned(),
    })
}

/// Unloads a label. A label that was not loaded is not an error - `uninstall` after a
/// crash must still work.
///
/// # Errors
///
/// If `launchctl` cannot be run at all.
pub fn bootout(agent: &Agent) -> Result<(), LaunchdError> {
    let target = format!("gui/{}/{}", uid()?, agent.label());
    let _ = command("launchctl", &["bootout", &target], Timeout::WORK)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{Agent, Job, backup_plist, catchup_plist, escape};
    use crate::config::{Schedule, TimeOfDay, Weekday};
    use std::path::Path;

    fn job<'a>(agent: &'a Agent, arguments: &[&str]) -> Job<'a> {
        Job {
            agent,
            program: Path::new("/Users/someone/.cargo/bin/tycho"),
            arguments: arguments.iter().map(|text| (*text).to_owned()).collect(),
            log_dir: Path::new("/Users/someone/Library/Logs/tycho"),
            log_stem: "demo".to_owned(),
        }
    }

    fn sunday_noon() -> Schedule {
        Schedule::Weekly {
            day: Weekday::Sunday,
            at: TimeOfDay {
                hour: 12,
                minute: 0,
            },
        }
    }

    /// The infix is the whole of the collision safety. `ProfileName` reserves
    /// `catchup`; this proves the label construction does not undo that for a name it
    /// does allow.
    #[test]
    fn the_profile_infix_keeps_the_two_families_disjoint() {
        let profile = Agent::Backup("catchup-work".to_owned()).label();
        assert_eq!(profile, "com.coreenginex.tycho.profile.catchup-work");
        assert_ne!(profile, Agent::Catchup.label());
        assert!(!profile.starts_with(&Agent::Catchup.label()));
    }

    /// Sunday is 0, confirmed against `man launchd.plist`.
    #[test]
    fn sunday_is_weekday_zero() {
        let agent = Agent::Backup("demo".to_owned());
        let plist = backup_plist(&job(&agent, &["run", "demo"]), sunday_noon());
        assert!(
            plist.contains("<key>Weekday</key><integer>0</integer>"),
            "{plist}"
        );
        assert_eq!(Weekday::launchd(Weekday::Sunday), 0);
    }

    /// Both were deliberate, and both would otherwise be silently "fixed" by a later
    /// edit: a backup on every login is not what a weekly schedule means, and a
    /// catch-up at login probes File Providers that have not mounted yet.
    #[test]
    fn run_at_load_is_false_on_backup_and_absent_on_catchup() {
        let backup = Agent::Backup("demo".to_owned());
        let plist = backup_plist(&job(&backup, &["run", "demo"]), sunday_noon());
        assert!(plist.contains("<key>RunAtLoad</key><false/>"), "{plist}");

        let catchup = Agent::Catchup;
        let plist = catchup_plist(&job(&catchup, &["push", "--all"]));
        assert!(!plist.contains("RunAtLoad"), "{plist}");
        assert!(plist.contains("<key>StartOnMount</key><true/>"), "{plist}");
    }

    /// launchd sends output to `/dev/null` when the path is absent, which is a
    /// silent-failure surface in a tool whose thesis is that failure is loud.
    #[test]
    fn both_output_paths_are_set() {
        let agent = Agent::Backup("demo".to_owned());
        let plist = backup_plist(&job(&agent, &["run", "demo"]), sunday_noon());
        assert!(
            plist.contains("/Library/Logs/tycho/demo.out.log"),
            "{plist}"
        );
        assert!(
            plist.contains("/Library/Logs/tycho/demo.err.log"),
            "{plist}"
        );
    }

    #[test]
    fn an_interval_schedule_becomes_start_interval() {
        let agent = Agent::Backup("demo".to_owned());
        let plist = backup_plist(
            &job(&agent, &["run", "demo"]),
            Schedule::Every(std::time::Duration::from_secs(3600)),
        );
        assert!(
            plist.contains("<key>StartInterval</key><integer>3600</integer>"),
            "{plist}"
        );
        assert!(!plist.contains("StartCalendarInterval"), "{plist}");
    }

    #[test]
    fn xml_metacharacters_are_escaped() {
        assert_eq!(
            escape("a & b < c > d \" e ' f"),
            "a &amp; b &lt; c &gt; d &quot; e &apos; f"
        );
    }

    /// A path with an ampersand in it is a real path somebody has. A plist that
    /// truncated at one would schedule a backup of the wrong tree at exit 0.
    #[test]
    fn a_hostile_argument_survives_into_the_plist() {
        let agent = Agent::Backup("demo".to_owned());
        let plist = backup_plist(&job(&agent, &["run", "R&D <archive>"]), sunday_noon());
        assert!(
            plist.contains("<string>R&amp;D &lt;archive&gt;</string>"),
            "{plist}"
        );
        assert!(!plist.contains("R&D"), "the raw ampersand must not survive");
    }
}
