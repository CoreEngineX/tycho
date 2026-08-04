//! Layer 3. The TOML schema, its validation, and the rule tree. Pure: no IO, so
//! rule resolution is testable with plain values.

pub mod check;
pub mod raw;
pub mod rules;

use crate::primitives::names::{ProfileName, RemoteName, RootAlias};
use crate::primitives::path::AbsPath;
use jiff::Zoned;
use std::fmt;
use std::path::Path;
use std::str::FromStr;
use std::time::Duration;

pub use check::{Diagnostic, DiagnosticKind, Severity};

/// A watched root, and the name it gets inside the store. A sum type rather than a
/// struct with an optional name, so "named" and "defaulted" cannot both be true.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WatchEntry {
    Bare(AbsPath),
    Named { path: AbsPath, name: RootAlias },
}

impl WatchEntry {
    #[must_use]
    pub fn path(&self) -> &AbsPath {
        match self {
            Self::Bare(path) | Self::Named { path, .. } => path,
        }
    }

    /// The alias this root's content is stored under: the explicit name, or the
    /// root's last path component.
    ///
    /// # Errors
    ///
    /// If the last component is not usable as an alias - a leading dot, a `.lock`
    /// suffix, or the reserved `.tycho`.
    pub fn alias(&self) -> Result<RootAlias, crate::primitives::names::AliasError> {
        match self {
            Self::Named { name, .. } => Ok(name.clone()),
            Self::Bare(path) => match path.as_path().file_name() {
                Some(component) => RootAlias::from_component(component),
                None => Err(crate::primitives::names::AliasError::Empty),
            },
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct TimeOfDay {
    pub hour: u8,
    pub minute: u8,
}

impl TimeOfDay {
    /// # Errors
    ///
    /// If the text is not `HH:MM` inside 00:00 to 23:59.
    pub fn parse(text: &str) -> Result<Self, String> {
        let Some((hour, minute)) = text.split_once(':') else {
            return Err(format!("'{text}' is not HH:MM"));
        };
        let hour: u8 = hour.parse().map_err(|_| format!("'{text}' is not HH:MM"))?;
        let minute: u8 = minute
            .parse()
            .map_err(|_| format!("'{text}' is not HH:MM"))?;
        if hour > 23 || minute > 59 {
            return Err(format!("'{text}' is not a time of day"));
        }
        Ok(Self { hour, minute })
    }

    /// This time of day on a given date.
    #[must_use]
    pub const fn on(self, date: jiff::civil::Date) -> jiff::civil::DateTime {
        date.at(self.hour as i8, self.minute as i8, 0, 0)
    }
}

impl fmt::Display for TimeOfDay {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:02}:{:02}", self.hour, self.minute)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Weekday {
    Sunday,
    Monday,
    Tuesday,
    Wednesday,
    Thursday,
    Friday,
    Saturday,
}

impl Weekday {
    /// # Errors
    ///
    /// If the text is not an English weekday name.
    pub fn parse(text: &str) -> Result<Self, String> {
        match text.to_ascii_lowercase().as_str() {
            "sunday" => Ok(Self::Sunday),
            "monday" => Ok(Self::Monday),
            "tuesday" => Ok(Self::Tuesday),
            "wednesday" => Ok(Self::Wednesday),
            "thursday" => Ok(Self::Thursday),
            "friday" => Ok(Self::Friday),
            "saturday" => Ok(Self::Saturday),
            _ => Err(format!("'{text}' is not a day of the week")),
        }
    }

    #[must_use]
    pub const fn civil(self) -> jiff::civil::Weekday {
        match self {
            Self::Sunday => jiff::civil::Weekday::Sunday,
            Self::Monday => jiff::civil::Weekday::Monday,
            Self::Tuesday => jiff::civil::Weekday::Tuesday,
            Self::Wednesday => jiff::civil::Weekday::Wednesday,
            Self::Thursday => jiff::civil::Weekday::Thursday,
            Self::Friday => jiff::civil::Weekday::Friday,
            Self::Saturday => jiff::civil::Weekday::Saturday,
        }
    }

    /// launchd's `Weekday` key, where Sunday is 0.
    #[must_use]
    pub const fn launchd(self) -> u8 {
        match self {
            Self::Sunday => 0,
            Self::Monday => 1,
            Self::Tuesday => 2,
            Self::Wednesday => 3,
            Self::Thursday => 4,
            Self::Friday => 5,
            Self::Saturday => 6,
        }
    }
}

/// Exactly one, enforced by the type. `Daily` and `Weekly` become
/// `StartCalendarInterval`; `Every` becomes `StartInterval`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Schedule {
    Daily { at: TimeOfDay },
    Weekly { day: Weekday, at: TimeOfDay },
    Every(Duration),
}

/// How far past its schedule a profile may drift before it is overdue.
///
/// A run does not start at exactly the scheduled second - launchd coalesces, the
/// machine wakes late, a previous run holds the lock - so a grace period is what
/// separates "ran a bit late" from "did not run". Six hours is comfortably longer than
/// any run this design produces and far shorter than the weekly interval it guards.
pub const GRACE: Duration = Duration::from_secs(6 * 3_600);

impl Schedule {
    /// How long this schedule leaves between runs.
    ///
    /// Nominal, not exact: a weekly schedule's real interval is 167 or 169 hours
    /// across a DST boundary. That imprecision is irrelevant here because it is only
    /// ever compared against a value plus [`GRACE`], and it is what lets the overdue
    /// check be pure arithmetic rather than a second calendar walk.
    #[must_use]
    pub const fn interval(self) -> Duration {
        match self {
            Self::Daily { .. } => Duration::from_secs(86_400),
            Self::Weekly { .. } => Duration::from_secs(7 * 86_400),
            Self::Every(every) => every,
        }
    }

    /// How long past due this profile is, or `None` if it is not.
    ///
    /// **This is what makes the no-daemon design safe rather than merely cheap.**
    /// `man launchd.plist` promises catch-up across *sleep* and says nothing about
    /// power-off, so a Mac shut down over a weekend would silently skip its weekly
    /// backup and nothing would ever notice - the exact shape of the failure this
    /// project exists to correct. Every invocation of every agent runs this.
    ///
    /// A profile that has never run successfully is overdue from the moment it is
    /// configured, which is right: "no backup has ever worked" is the loudest thing
    /// this tool can have to say.
    #[must_use]
    pub fn overdue_by(self, last_success: Option<&Zoned>, now: &Zoned) -> Option<Duration> {
        let Some(last) = last_success else {
            return Some(self.interval());
        };
        let elapsed = now.timestamp().as_second() - last.timestamp().as_second();
        let elapsed = Duration::from_secs(u64::try_from(elapsed).unwrap_or(0));
        elapsed
            .checked_sub(self.interval() + GRACE)
            .filter(|over| !over.is_zero())
    }

    /// When this schedule next fires, strictly after `from`.
    ///
    /// Zoned rather than added seconds, because "Sunday 12:00" is a wall-clock
    /// promise: across the two nights a year the offset changes, the interval to the
    /// next run is 23 or 25 hours, not 24. `Every` is the opposite by definition - an
    /// elapsed duration, which is what launchd's `StartInterval` counts.
    ///
    /// # Errors
    ///
    /// If the arithmetic leaves the range `jiff` can represent.
    pub fn next_after(self, from: &Zoned) -> Result<Zoned, jiff::Error> {
        let tz = from.time_zone().clone();
        match self {
            Self::Every(interval) => from.checked_add(
                jiff::Span::new().seconds(i64::try_from(interval.as_secs()).unwrap_or(i64::MAX)),
            ),
            Self::Daily { at } => {
                let today = at.on(from.date()).to_zoned(tz.clone())?;
                if &today > from {
                    return Ok(today);
                }
                at.on(from.date().tomorrow()?).to_zoned(tz)
            }
            Self::Weekly { day, at } => {
                let mut date = from.date();
                // Eight, not seven: today may match the weekday and have already
                // passed, in which case the answer is a week from today.
                for _ in 0..8 {
                    if date.weekday() == day.civil() {
                        let candidate = at.on(date).to_zoned(tz.clone())?;
                        if &candidate > from {
                            return Ok(candidate);
                        }
                    }
                    date = date.tomorrow()?;
                }
                at.on(date).to_zoned(tz)
            }
        }
    }
}

impl FromStr for Schedule {
    type Err = String;

    /// `daily:HH:MM`, `weekly:<weekday>:HH:MM`, `every:<N>h` or `every:<N>m`.
    fn from_str(text: &str) -> Result<Self, Self::Err> {
        let bad = || {
            format!("'{text}' is not daily:HH:MM, weekly:<weekday>:HH:MM, every:<N>h or every:<N>m")
        };
        let Some((kind, rest)) = text.split_once(':') else {
            return Err(bad());
        };
        match kind {
            "daily" => Ok(Self::Daily {
                at: TimeOfDay::parse(rest)?,
            }),
            "weekly" => {
                let Some((day, at)) = rest.split_once(':') else {
                    return Err(bad());
                };
                Ok(Self::Weekly {
                    day: Weekday::parse(day)?,
                    at: TimeOfDay::parse(at)?,
                })
            }
            "every" => {
                if !matches!(rest.chars().last(), Some('h' | 'm')) {
                    return Err(bad());
                }
                parse_interval(rest).map(Self::Every)
            }
            _ => Err(bad()),
        }
    }
}

/// Parses `6h`-style intervals: an integer and one of `s`, `m`, `h`, `d`.
///
/// # Errors
///
/// If the text is not that shape, or the count is zero.
pub fn parse_interval(text: &str) -> Result<Duration, String> {
    let trimmed = text.trim();
    let split = trimmed.len().saturating_sub(1);
    let (count, unit) = trimmed.split_at(split);
    let seconds = match unit {
        "s" => 1,
        "m" => 60,
        "h" => 3_600,
        "d" => 86_400,
        _ => return Err(format!("'{text}' must end in s, m, h or d")),
    };
    let count: u64 = count
        .parse()
        .map_err(|_| format!("'{text}' must start with a whole number"))?;
    if count == 0 {
        return Err(format!("'{text}' is not an interval"));
    }
    Ok(Duration::from_secs(count * seconds))
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum LogLevel {
    Error,
    Warn,
    #[default]
    Info,
    Debug,
}

impl LogLevel {
    /// # Errors
    ///
    /// If the text is not one of the four levels.
    pub fn parse(text: &str) -> Result<Self, String> {
        match text {
            "error" => Ok(Self::Error),
            "warn" => Ok(Self::Warn),
            "info" => Ok(Self::Info),
            "debug" => Ok(Self::Debug),
            _ => Err(format!("'{text}' is not a log level")),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Remote {
    /// A label, not a location - `path` is what finds the drive, and this never
    /// touches the filesystem. It names the remote in `status`, in `doctor`'s rows,
    /// and in the state file.
    ///
    /// **Renaming one starts its history over.** The state file keys a remote's
    /// last-seen time, head and behind-count by this name, so a rename reads as a
    /// remote that has never been seen: `unseen` on the next run, then a full
    /// re-verify. Harmless before there is history worth keeping, which is the
    /// argument for settling on the name early rather than tidying it later.
    pub name: RemoteName,
    pub path: AbsPath,
    pub optional: bool,
    pub behind_tolerance: u32,
    /// Opt in to git operating on this remote despite the filesystem recording no
    /// ownership - exFAT and FAT32, which is what an external drive usually is.
    ///
    /// Off by default and never inferred. `safe.directory` exists because a
    /// repository on removable media can be attacker-controlled, hooks included, so
    /// the decision is the user's to write down per remote rather than Tycho's to
    /// make for every path in a config file.
    pub trust_ownership: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Profile {
    pub name: ProfileName,
    pub watch: Vec<WatchEntry>,
    pub ignore_paths: Vec<AbsPath>,
    pub ignore_globs: Vec<String>,
    pub reinclude: Vec<AbsPath>,
    pub remotes: Vec<Remote>,
    pub schedule: Option<Schedule>,
    pub use_default_ignores: bool,
    pub store_path: Option<AbsPath>,
    pub local_only: bool,
}

impl Profile {
    /// The rule inputs for this profile, ready for `RuleTree::build`.
    #[must_use]
    pub fn rule_set(&self) -> rules::RuleSet {
        rules::RuleSet {
            watch: self
                .watch
                .iter()
                .map(|entry| entry.path().clone())
                .collect(),
            ignore_paths: self.ignore_paths.clone(),
            reinclude: self.reinclude.clone(),
            ignore_globs: self.ignore_globs.clone(),
            junk: if self.use_default_ignores {
                rules::DEFAULT_JUNK
                    .iter()
                    .map(|pattern| (*pattern).to_owned())
                    .collect()
            } else {
                Vec::new()
            },
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Config {
    pub log_level: LogLevel,
    pub profiles: Vec<Profile>,
}

/// What a parse produced, and everything wrong with it. A config with errors still
/// yields whatever profiles were readable, so `status` can render in a degraded mode
/// rather than showing nothing.
#[derive(Debug)]
pub struct Parsed {
    pub config: Config,
    pub diagnostics: Vec<Diagnostic>,
}

impl Parsed {
    #[must_use]
    pub fn has_errors(&self) -> bool {
        self.diagnostics
            .iter()
            .any(|diagnostic| diagnostic.severity == Severity::Error)
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("the config file is not valid TOML: {0}")]
    Toml(String),
    #[error(
        "this config was written by a newer tycho: it declares version {found}, and this build understands {understood}"
    )]
    Version { found: u32, understood: u32 },
}

/// Parses and validates against the real environment.
///
/// # Errors
///
/// If the text is not TOML, or declares a version this build does not understand.
pub fn parse(text: &str) -> Result<Parsed, ConfigError> {
    parse_with(text, std::env::home_dir().as_deref(), |name| {
        std::env::var(name).ok()
    })
}

/// The same, against a supplied environment - which is how `doctor` re-resolves
/// every root under the launchd agent's environment and compares.
///
/// # Errors
///
/// As [`parse`].
pub fn parse_with(
    text: &str,
    home: Option<&Path>,
    var: impl Fn(&str) -> Option<String>,
) -> Result<Parsed, ConfigError> {
    let probe: raw::VersionProbe =
        toml::from_str(text).map_err(|error| ConfigError::Toml(error.to_string()))?;
    if let Some(found) = probe.version
        && found > raw::VERSION
    {
        return Err(ConfigError::Version {
            found,
            understood: raw::VERSION,
        });
    }

    let raw: raw::RawConfig =
        toml::from_str(text).map_err(|error| ConfigError::Toml(error.to_string()))?;
    Ok(check::validate(&raw, home, &var))
}

#[cfg(test)]
mod schedule_tests {
    use super::{Schedule, TimeOfDay, Weekday, parse_interval};
    use jiff::{Zoned, tz::TimeZone};

    fn at(text: &str) -> Zoned {
        let tz = TimeZone::get("America/Toronto").expect("a zone with DST");
        text.parse::<jiff::civil::DateTime>()
            .expect("a civil datetime")
            .to_zoned(tz)
            .expect("no gap")
    }

    fn noon() -> TimeOfDay {
        TimeOfDay {
            hour: 12,
            minute: 0,
        }
    }

    #[test]
    fn daily_takes_today_if_it_has_not_passed_and_tomorrow_otherwise() {
        let daily = Schedule::Daily { at: noon() };
        let next = daily.next_after(&at("2026-08-03T09:00:00")).expect("next");
        assert_eq!(next.strftime("%F %H:%M").to_string(), "2026-08-03 12:00");

        let next = daily.next_after(&at("2026-08-03T12:00:00")).expect("next");
        assert_eq!(
            next.strftime("%F %H:%M").to_string(),
            "2026-08-04 12:00",
            "the boundary is strictly after, or a run would re-fire on itself"
        );
    }

    #[test]
    fn weekly_finds_the_named_day() {
        let weekly = Schedule::Weekly {
            day: Weekday::Sunday,
            at: noon(),
        };
        let next = weekly.next_after(&at("2026-08-03T09:00:00")).expect("next");
        assert_eq!(
            next.strftime("%a %F %H:%M").to_string(),
            "Sun 2026-08-09 12:00"
        );
    }

    /// Today is the named day and the time has gone, so the answer is a week out
    /// rather than a few hours ago.
    #[test]
    fn weekly_on_its_own_day_after_the_hour_goes_a_full_week_out() {
        let weekly = Schedule::Weekly {
            day: Weekday::Sunday,
            at: noon(),
        };
        let next = weekly.next_after(&at("2026-08-09T14:00:00")).expect("next");
        assert_eq!(next.strftime("%F %H:%M").to_string(), "2026-08-16 12:00");
    }

    /// The whole reason this is zoned arithmetic. Toronto springs forward at 02:00 on
    /// 2027-03-14, so the gap between two consecutive 12:00 runs is 23 hours - and a
    /// schedule that added 86,400 seconds would drift the run to 13:00 and stay there.
    #[test]
    fn a_wall_clock_schedule_holds_its_hour_across_a_dst_boundary() {
        let daily = Schedule::Daily { at: noon() };
        let before = at("2027-03-13T12:00:01");
        let next = daily.next_after(&before).expect("next");

        assert_eq!(next.strftime("%F %H:%M").to_string(), "2027-03-14 12:00");
        let elapsed = &next - &before;
        assert_eq!(elapsed.get_hours(), 22, "23 hours, less the one second");
    }

    /// `Every` is the opposite promise: an elapsed duration, which does not hold its
    /// wall-clock hour and is not meant to.
    #[test]
    fn an_interval_counts_elapsed_time_not_wall_clock() {
        let every = Schedule::Every(parse_interval("24h").expect("24h"));
        let before = at("2027-03-13T12:00:00");
        let next = every.next_after(&before).expect("next");

        assert_eq!(next.strftime("%F %H:%M").to_string(), "2027-03-14 13:00");
    }
}

#[cfg(test)]
mod parse_tests {
    use super::{Schedule, TimeOfDay, Weekday};
    use std::time::Duration;

    #[test]
    fn daily_parses_the_time_of_day() {
        assert_eq!(
            "daily:12:30".parse::<Schedule>().expect("parses"),
            Schedule::Daily {
                at: TimeOfDay {
                    hour: 12,
                    minute: 30
                }
            }
        );
    }

    #[test]
    fn weekly_parses_the_day_and_time() {
        assert_eq!(
            "weekly:sunday:09:00".parse::<Schedule>().expect("parses"),
            Schedule::Weekly {
                day: Weekday::Sunday,
                at: TimeOfDay { hour: 9, minute: 0 }
            }
        );
    }

    #[test]
    fn every_accepts_hours_and_minutes_only() {
        assert_eq!(
            "every:6h".parse::<Schedule>().expect("parses"),
            Schedule::Every(Duration::from_secs(6 * 3_600))
        );
        assert_eq!(
            "every:30m".parse::<Schedule>().expect("parses"),
            Schedule::Every(Duration::from_secs(30 * 60))
        );
    }

    #[test]
    fn every_rejects_units_outside_the_documented_grammar() {
        assert!("every:6s".parse::<Schedule>().is_err());
        assert!("every:2d".parse::<Schedule>().is_err());
    }

    #[test]
    fn an_unrecognised_shape_names_every_accepted_form() {
        let error = "hourly:12:00".parse::<Schedule>().expect_err("rejected");
        assert!(error.contains("daily:HH:MM"), "{error}");
        assert!(error.contains("every:<N>m"), "{error}");

        assert!("nonsense".parse::<Schedule>().is_err());
        assert!("weekly:sunday".parse::<Schedule>().is_err());
    }
}

#[cfg(test)]
mod overdue_tests {
    use super::{GRACE, Schedule, TimeOfDay, Weekday};
    use jiff::{Span, Zoned, tz::TimeZone};

    fn now() -> Zoned {
        "2026-11-08T14:30:00"
            .parse::<jiff::civil::DateTime>()
            .expect("a civil datetime")
            .to_zoned(TimeZone::get("America/Toronto").expect("a zone"))
            .expect("no gap")
    }

    fn ago(span: Span) -> Zoned {
        now().checked_sub(span).expect("in range")
    }

    fn weekly() -> Schedule {
        Schedule::Weekly {
            day: Weekday::Sunday,
            at: TimeOfDay {
                hour: 12,
                minute: 0,
            },
        }
    }

    #[test]
    fn a_run_inside_the_interval_is_not_overdue() {
        let last = ago(Span::new().days(3));
        assert_eq!(weekly().overdue_by(Some(&last), &now()), None);
    }

    /// A run does not start at exactly the scheduled second: launchd coalesces, the
    /// machine wakes late, a previous run holds the lock. Grace is what separates
    /// "ran a bit late" from "did not run".
    #[test]
    fn a_run_inside_the_grace_period_is_not_overdue() {
        let last = ago(Span::new().days(7).hours(5));
        assert_eq!(weekly().overdue_by(Some(&last), &now()), None);
    }

    /// The case the whole check exists for: a Mac shut down over a weekend, which
    /// `man launchd.plist` promises nothing about.
    #[test]
    fn a_run_past_the_grace_period_is_overdue_and_says_by_how_much() {
        let last = ago(Span::new().days(9));
        let over = weekly()
            .overdue_by(Some(&last), &now())
            .expect("two days past the grace period");
        assert!(over.as_secs() > 86_400, "{over:?}");
    }

    /// "No backup has ever worked" is the loudest thing this tool can have to say, so
    /// it is overdue from the moment it is configured.
    #[test]
    fn a_profile_that_has_never_run_is_overdue_immediately() {
        assert_eq!(weekly().overdue_by(None, &now()), Some(weekly().interval()));
    }

    #[test]
    fn each_schedule_shape_carries_its_own_interval() {
        assert_eq!(weekly().interval().as_secs(), 7 * 86_400);
        assert_eq!(
            Schedule::Daily {
                at: TimeOfDay { hour: 1, minute: 0 }
            }
            .interval()
            .as_secs(),
            86_400
        );
        assert_eq!(
            Schedule::Every(std::time::Duration::from_secs(900))
                .interval()
                .as_secs(),
            900
        );
    }

    /// An hourly catch-up must not be called overdue after ninety minutes, or the
    /// notification fires several times a day and stops meaning anything.
    #[test]
    fn a_short_interval_still_gets_the_full_grace_period() {
        let hourly = Schedule::Every(std::time::Duration::from_secs(3_600));
        let last = ago(Span::new().hours(2));
        assert_eq!(hourly.overdue_by(Some(&last), &now()), None);
        assert_eq!(GRACE.as_secs(), 6 * 3_600);
    }
}
