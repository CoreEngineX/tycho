//! Layer 5. The Windows lifecycle: Task Scheduler, driven through `schtasks`.
//!
//! The same shape as `launchd`, for the same reason `scheduling.md` section 1 gives:
//! Tycho has no resident process, so the OS scheduler starts a run and the run exits.
//! A definition is generated, registered, and read back - never hand-written - because
//! drift between what was installed and what the config asks for is how the system
//! this replaced failed unnoticed.
//!
//! Three things Task Scheduler does differently, each measured on a real machine
//! rather than taken from documentation:
//!
//! 1. **It will not run on battery unless told to.** `DisallowStartIfOnBatteries` and
//!    `StopIfGoingOnBatteries` default to `true`, and a definition that omits them
//!    gets them anyway - confirmed by reading a task back after registering one that
//!    named neither. On a laptop that is a backup which silently never runs, so both
//!    are written explicitly.
//! 2. **It captures no output.** launchd has `StandardOutPath`; there is no
//!    equivalent here, so the action is `cmd /c "…" >> log 2>&1`. Without it a panic
//!    before Tycho opens its own log leaves nothing at all.
//! 3. **`StartWhenAvailable` is the catch-up.** It is what makes a missed calendar
//!    run fire once the machine is awake, which is the property `scheduling.md`
//!    section 1 relies on launchd for. It round-trips through `schtasks /Query /XML`.

use crate::config::{Schedule, TimeOfDay, Weekday};
use crate::platform::{Agent, Ended, Job, Loaded};
use crate::primitives::path::{AbsPath, PathError};
use crate::sys::process::{RunError, Timeout, command};
use std::path::{Path, PathBuf};

#[derive(Debug, thiserror::Error)]
pub enum Error {
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
    #[error("schtasks {action} refused {label}: {detail}")]
    Refused {
        action: String,
        label: String,
        detail: String,
    },
}

/// Tasks live in a folder of their own, so `schtasks /Query` at the root does not
/// bury them among the hundred Windows ships with.
const FOLDER: &str = "Tycho";

/// `SCHED_S_TASK_HAS_NOT_RUN`. Task Scheduler reports it as the last result of a task
/// that has never fired, and a freshly installed agent is exactly that - so it must
/// not read as a failure.
const NEVER_RUN: i64 = 267_011;

/// The registered name, which is what every `schtasks` verb takes.
#[must_use]
pub fn task_name(agent: &Agent) -> String {
    format!("\\{FOLDER}\\{}", agent.label())
}

/// Where the generated definitions are kept.
///
/// Task Scheduler stores its own copy once a task is registered, so this is not the
/// live one. It is kept because `service status` compares the definition Tycho
/// generated against what the config now asks for, and because a definition nobody
/// can read is one nobody can check.
///
/// # Errors
///
/// If there is no home directory.
pub fn definitions_dir() -> Result<AbsPath, PathError> {
    let dir = crate::platform::data_dir()?;
    AbsPath::from_absolute(&dir.as_path().join("tasks"))
}

/// # Errors
///
/// If there is no home directory.
pub fn definition_path(agent: &Agent) -> Result<PathBuf, PathError> {
    Ok(definitions_dir()?
        .as_path()
        .join(format!("{}.xml", agent.label())))
}

/// Escapes the five characters XML gives meaning to.
///
/// A watched path may contain `&`. A definition that silently truncated at one would
/// schedule a backup of the wrong tree, at exit 0.
#[must_use]
pub fn escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

/// The `Arguments` value: the program and its arguments, with both streams appended
/// to a log.
///
/// `cmd /c "…"` and the doubled outer quotes are what survives a space in either
/// path - `cmd` strips the outer pair and treats the rest as the command line. Tested
/// against paths containing spaces rather than assumed.
fn action_arguments(job: &Job<'_>) -> String {
    use std::fmt::Write as _;

    let out = job.log_dir.join(format!("{}.out.log", job.log_stem));
    let mut line = format!("/c \"\"{}\"", job.program.display());
    for argument in &job.arguments {
        line.push(' ');
        line.push('"');
        line.push_str(argument);
        line.push('"');
    }
    let _ = write!(line, " >> \"{}\" 2>&1\"", out.display());
    line
}

/// Every setting that decides whether a run happens at all.
///
/// The battery pair is the load-bearing part: both default to `true`, so a laptop
/// running on battery at the scheduled moment simply does not back up, and nothing
/// reports it. `ExecutionTimeLimit` is generous rather than absent because a task with
/// no limit that hangs holds `MultipleInstancesPolicy` against every later run.
fn settings() -> String {
    "  <Settings>\n\
     \x20   <DisallowStartIfOnBatteries>false</DisallowStartIfOnBatteries>\n\
     \x20   <StopIfGoingOnBatteries>false</StopIfGoingOnBatteries>\n\
     \x20   <StartWhenAvailable>true</StartWhenAvailable>\n\
     \x20   <MultipleInstancesPolicy>IgnoreNew</MultipleInstancesPolicy>\n\
     \x20   <ExecutionTimeLimit>PT2H</ExecutionTimeLimit>\n\
     \x20   <AllowHardTerminate>true</AllowHardTerminate>\n\
     \x20   <Enabled>true</Enabled>\n\
     \x20   <Hidden>false</Hidden>\n\
     \x20   <Priority>7</Priority>\n\
     \x20 </Settings>\n"
        .to_owned()
}

/// `schtasks /Create /XML` insists the declaration say UTF-16 - a document declaring
/// UTF-8 is refused outright with `unable to switch the encoding`, pointing at column
/// 40 of line 1. So the declaration says UTF-16 and [`encode`] makes that true, rather
/// than the file claiming one encoding and carrying another.
fn document(description: &str, triggers: &str, job: &Job<'_>) -> String {
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-16\"?>\n\
         <Task version=\"1.2\" xmlns=\"http://schemas.microsoft.com/windows/2004/02/mit/task\">\n\
         \x20 <RegistrationInfo>\n\
         \x20   <Description>{}</Description>\n\
         \x20 </RegistrationInfo>\n\
         \x20 <Triggers>\n{}\x20 </Triggers>\n\
         \x20 <Principals>\n\
         \x20   <Principal id=\"Author\">\n\
         \x20     <LogonType>InteractiveToken</LogonType>\n\
         \x20     <RunLevel>LeastPrivilege</RunLevel>\n\
         \x20   </Principal>\n\
         \x20 </Principals>\n{}\
         \x20 <Actions Context=\"Author\">\n\
         \x20   <Exec>\n\
         \x20     <Command>cmd.exe</Command>\n\
         \x20     <Arguments>{}</Arguments>\n\
         \x20   </Exec>\n\
         \x20 </Actions>\n\
         </Task>\n",
        escape(description),
        triggers,
        settings(),
        escape(&action_arguments(job))
    )
}

const fn day_element(day: Weekday) -> &'static str {
    match day {
        Weekday::Sunday => "Sunday",
        Weekday::Monday => "Monday",
        Weekday::Tuesday => "Tuesday",
        Weekday::Wednesday => "Wednesday",
        Weekday::Thursday => "Thursday",
        Weekday::Friday => "Friday",
        Weekday::Saturday => "Saturday",
    }
}

/// A fixed date in the past, so the trigger's own recurrence decides when it fires
/// rather than the boundary. Only the time of day is read from it.
fn boundary(at: TimeOfDay) -> String {
    format!("2020-01-01T{:02}:{:02}:00", at.hour, at.minute)
}

fn trigger(schedule: Schedule) -> String {
    match schedule {
        Schedule::Every(interval) => format!(
            "\x20   <TimeTrigger>\n\
             \x20     <StartBoundary>2020-01-01T00:00:00</StartBoundary>\n\
             \x20     <Enabled>true</Enabled>\n\
             \x20     <Repetition>\n\
             \x20       <Interval>PT{}M</Interval>\n\
             \x20       <StopAtDurationEnd>false</StopAtDurationEnd>\n\
             \x20     </Repetition>\n\
             \x20   </TimeTrigger>\n",
            (interval.as_secs() / 60).max(1)
        ),
        Schedule::Daily { at } => format!(
            "\x20   <CalendarTrigger>\n\
             \x20     <StartBoundary>{}</StartBoundary>\n\
             \x20     <Enabled>true</Enabled>\n\
             \x20     <ScheduleByDay><DaysInterval>1</DaysInterval></ScheduleByDay>\n\
             \x20   </CalendarTrigger>\n",
            boundary(at)
        ),
        Schedule::Weekly { day, at } => format!(
            "\x20   <CalendarTrigger>\n\
             \x20     <StartBoundary>{}</StartBoundary>\n\
             \x20     <Enabled>true</Enabled>\n\
             \x20     <ScheduleByWeek>\n\
             \x20       <DaysOfWeek><{} /></DaysOfWeek>\n\
             \x20       <WeeksInterval>1</WeeksInterval>\n\
             \x20     </ScheduleByWeek>\n\
             \x20   </CalendarTrigger>\n",
            boundary(at),
            day_element(day)
        ),
    }
}

/// A profile's backup task.
#[must_use]
pub fn backup_definition(job: &Job<'_>, schedule: Schedule) -> String {
    document("Tycho backup", &trigger(schedule), job)
}

/// The shared catch-up task: hourly, and nothing else.
///
/// launchd's `StartOnMount` has no Task Scheduler equivalent - there is no "a volume
/// appeared" trigger - so the interval is what notices a drive that came back, and
/// `Schedule::overdue_by` in `config.rs` is what makes the difference not matter.
///
/// **No logon trigger**, for two reasons that agree. A `<LogonTrigger>` naming no
/// `<UserId>` means every user, which Task Scheduler refuses to register without
/// elevation - measured, as `Access is denied` on an ordinary account. And
/// `catchup_plist` leaves `RunAtLoad` out deliberately: at login the cloud File
/// Providers may not have mounted, so every remote would be probed as unreachable on
/// every boot.
#[must_use]
pub fn catchup_definition(job: &Job<'_>) -> String {
    document(
        "Tycho catch-up push",
        &trigger(Schedule::Every(std::time::Duration::from_secs(3_600))),
        job,
    )
}

/// `doctor --deep`'s transient task, which exists to measure what a scheduled run can
/// actually reach.
#[must_use]
pub fn probe_definition(job: &Job<'_>) -> String {
    document(
        "Tycho access probe",
        &trigger(Schedule::Every(std::time::Duration::from_secs(86_400))),
        job,
    )
}

/// The bytes a definition is stored as: UTF-16LE with a byte-order mark, which is
/// what the declaration promises and what `schtasks` reads.
#[must_use]
pub fn encode(definition: &str) -> Vec<u8> {
    let mut out = vec![0xff, 0xfe];
    for unit in definition.encode_utf16() {
        out.extend_from_slice(&unit.to_le_bytes());
    }
    out
}

/// Reads back what [`encode`] wrote, so `service status` can compare the installed
/// definition against the config.
#[must_use]
pub fn decode(bytes: &[u8]) -> String {
    let body = bytes.strip_prefix(&[0xff, 0xfe]).unwrap_or(bytes);
    let units: Vec<u16> = body
        .chunks_exact(2)
        .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
        .collect();
    String::from_utf16_lossy(&units)
}

/// Registers a definition, replacing any task of the same name.
///
/// `/F` is what makes `install` idempotent and `restart` a one-liner, the same role
/// `bootout` before `bootstrap` plays on macOS.
///
/// # Errors
///
/// If `schtasks` refuses.
pub fn register(agent: &Agent, definition: &Path) -> Result<(), Error> {
    let name = task_name(agent);
    let path = definition.display().to_string();
    let out = command(
        "schtasks",
        &["/Create", "/TN", &name, "/XML", &path, "/F"],
        Timeout::WORK,
    )?;
    if out.status.success() {
        return Ok(());
    }
    Err(Error::Refused {
        action: "/Create".to_owned(),
        label: name,
        detail: message(&out),
    })
}

/// Removes a task. One that is not registered is not an error - `uninstall` after a
/// crash must still leave nothing behind.
///
/// # Errors
///
/// If `schtasks` fails for any reason other than the task being absent.
pub fn deregister(agent: &Agent) -> Result<(), Error> {
    let name = task_name(agent);
    let out = command("schtasks", &["/Delete", "/TN", &name, "/F"], Timeout::QUICK)?;
    if out.status.success() || state(agent)? == Loaded::No {
        return Ok(());
    }
    Err(Error::Refused {
        action: "/Delete".to_owned(),
        label: name,
        detail: message(&out),
    })
}

/// Asks Task Scheduler about one task.
///
/// # Errors
///
/// If `schtasks` cannot be run at all. A task that is not registered is
/// [`Loaded::No`], not an error.
pub fn state(agent: &Agent) -> Result<Loaded, RunError> {
    let name = task_name(agent);
    let out = command(
        "schtasks",
        &["/Query", "/TN", &name, "/FO", "LIST", "/V"],
        Timeout::QUICK,
    )?;
    if !out.status.success() {
        return Ok(Loaded::No);
    }

    let text = String::from_utf8_lossy(&out.stdout);
    let field = |name: &str| -> Option<String> {
        text.lines().find_map(|line| {
            let (key, value) = line.split_once(':')?;
            key.trim()
                .eq_ignore_ascii_case(name)
                .then(|| value.trim().to_owned())
        })
    };

    let last = match field("Last Result").and_then(|value| value.trim().parse::<i64>().ok()) {
        None | Some(NEVER_RUN) => Ended::NeverRun,
        Some(code) => Ended::Exit(i32::try_from(code).unwrap_or(-1)),
    };
    // Task Scheduler reports no pid. `Running` is the only thing it will say about a
    // task that has one, so that is what is reported rather than a number invented
    // to fill the field.
    Ok(Loaded::Yes { pid: None, last })
}

/// Reads the schedule back out of a generated definition.
///
/// Parsed from the shape this module writes rather than through an XML library: this
/// is the reader for text this crate wrote, and `tests/schtasks.rs` pins the two
/// together through Task Scheduler's own parser.
#[must_use]
pub fn scheduled_in(xml: &str) -> Option<Schedule> {
    if let Some(minutes) = between(xml, "<Interval>PT", "M</Interval>")
        && let Ok(minutes) = minutes.parse::<u64>()
        && !xml.contains("<LogonTrigger>")
    {
        return Some(Schedule::Every(std::time::Duration::from_secs(
            minutes * 60,
        )));
    }

    let boundary = between(xml, "<StartBoundary>", "</StartBoundary>")?;
    let time = boundary.split('T').nth(1)?;
    let mut parts = time.split(':');
    let at = TimeOfDay {
        hour: parts.next()?.parse().ok()?,
        minute: parts.next()?.parse().ok()?,
    };

    if xml.contains("<ScheduleByWeek>") {
        let day = [
            Weekday::Sunday,
            Weekday::Monday,
            Weekday::Tuesday,
            Weekday::Wednesday,
            Weekday::Thursday,
            Weekday::Friday,
            Weekday::Saturday,
        ]
        .into_iter()
        .find(|day| xml.contains(&format!("<{} />", day_element(*day))))?;
        return Some(Schedule::Weekly { day, at });
    }
    xml.contains("<ScheduleByDay>")
        .then_some(Schedule::Daily { at })
}

fn between<'a>(text: &'a str, open: &str, close: &str) -> Option<&'a str> {
    let start = text.find(open)? + open.len();
    let rest = &text[start..];
    Some(&rest[..rest.find(close)?])
}

/// `schtasks` reports refusals on stdout as often as on stderr, so both are offered.
fn message(out: &std::process::Output) -> String {
    let stderr = String::from_utf8_lossy(&out.stderr).trim().to_owned();
    if stderr.is_empty() {
        return String::from_utf8_lossy(&out.stdout).trim().to_owned();
    }
    stderr
}
