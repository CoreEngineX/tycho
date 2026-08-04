//! `tycho schedule`: show, set or clear a profile's run schedule.
//!
//! The SPEC grammar's parser lives here rather than beside [`crate::config::Schedule`]:
//! `config` does no IO and turns no free text into a value on its own, and the CLI is
//! the only caller that ever does.

use crate::cli::report::report;
use crate::cli::{Exit, ScheduleAction, ScheduleArgs, render};
use crate::config::{Config, Profile, Schedule, TimeOfDay, Weekday, parse_interval};
use crate::config_edit::Editing;
use std::path::PathBuf;
use std::str::FromStr;

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

pub fn dispatch(args: &ScheduleArgs) -> Exit {
    match &args.action {
        ScheduleAction::Show => show(args),
        ScheduleAction::Set { spec } => set(args, spec),
        ScheduleAction::Off => off(args),
    }
}

fn show(args: &ScheduleArgs) -> Exit {
    let Some((parsed, _)) = crate::cli::run::load(args.config.clone()) else {
        return Exit::Failure;
    };
    let profile = match find(&parsed.config, args.profile.as_deref()) {
        Ok(profile) => profile,
        Err(exit) => return exit,
    };
    let name = clip(profile.name.as_str(), 13);
    match profile.schedule {
        Some(schedule) => println!("{name:<14}  {}", render::say(schedule)),
        None => println!("{name:<14}  no schedule, runs only when invoked by hand"),
    }
    Exit::Ok
}

/// Keeps a name inside its column, losing the end rather than the start.
fn clip(text: &str, width: usize) -> String {
    let chars: Vec<char> = text.chars().collect();
    if chars.len() <= width {
        return text.to_owned();
    }
    let kept: String = chars[..width.saturating_sub(1)].iter().collect();
    format!("{kept}~")
}

fn set(args: &ScheduleArgs, spec: &str) -> Exit {
    let schedule = match spec.parse::<Schedule>() {
        Ok(schedule) => schedule,
        Err(reason) => {
            return report! {
                error: "'{spec}' is not a schedule",
                note: "{reason}",
                help: "accepted forms: daily:HH:MM, weekly:<weekday>:HH:MM, every:<N>h, \
                       every:<N>m",
                recovery: {
                    "tycho schedule set daily:12:00" => "once a day",
                    "tycho schedule set weekly:sunday:12:00" => "once a week",
                    "tycho schedule set every:6h" => "on a fixed interval",
                },
            };
        }
    };
    let (mut editing, index) = match open(args.config.clone(), args.profile.as_deref()) {
        Ok(pair) => pair,
        Err(exit) => return exit,
    };
    if let Err(error) = editing.set_schedule(index, schedule) {
        return report! { error: "{error}" };
    }
    let said = render::say(schedule);
    finish(&editing, "scheduled", &said)
}

fn off(args: &ScheduleArgs) -> Exit {
    let (mut editing, index) = match open(args.config.clone(), args.profile.as_deref()) {
        Ok(pair) => pair,
        Err(exit) => return exit,
    };
    if let Err(error) = editing.clear_schedule(index) {
        return report! { error: "{error}" };
    }
    finish(&editing, "cleared", "schedule")
}

fn finish(editing: &Editing, verb: &str, value: &str) -> Exit {
    if let Err(error) = editing.save() {
        return report! { error: "{error}" };
    }
    println!("{verb:<14} {value}");

    let Ok(parsed) = crate::config::parse(&editing.text()) else {
        return report! {
            error: "the file no longer parses, so the edit was written but cannot be read back",
            recovery: { "tycho config path" => "prints the file to open" },
        };
    };
    if parsed.diagnostics.is_empty() {
        return Exit::Ok;
    }
    eprint!("\n{}", render::config_check(&[], &parsed.diagnostics));
    if parsed.has_errors() {
        Exit::Failure
    } else {
        Exit::Warning
    }
}

fn open(config: Option<PathBuf>, profile: Option<&str>) -> Result<(Editing, usize), Exit> {
    let path = match crate::cli::run::config_location(config) {
        Ok(path) => path,
        Err(error) => return Err(report! { error: "{error}" }),
    };
    if !path.exists() {
        let shown = path.display();
        return Err(report! {
            error: "no config file at {shown}",
            note: "a config file lists what to watch and where to send it; every command \
                   but `config init` needs one",
            recovery: {
                "tycho config init" => "writes a starter file, with a comment on each key",
            },
        });
    }
    let editing = match Editing::open(&path) {
        Ok(editing) => editing,
        Err(error) => return Err(report! { error: "{error}" }),
    };
    match editing.which(profile) {
        Ok(index) => Ok((editing, index)),
        Err(error) => Err(report! {
            error: "{error}",
            recovery: { "tycho profile list" => "names every profile in this file" },
        }),
    }
}

fn find<'a>(config: &'a Config, wanted: Option<&str>) -> Result<&'a Profile, Exit> {
    match wanted {
        Some(name) => config
            .profiles
            .iter()
            .find(|profile| profile.name.as_str() == name)
            .ok_or_else(|| {
                report! {
                    error: "no profile named '{name}'",
                    recovery: { "tycho profile list" => "names every profile in this file" },
                }
            }),
        None => match config.profiles.as_slice() {
            [only] => Ok(only),
            [] => Err(report! { error: "the config defines no profiles" }),
            many => {
                let names = many
                    .iter()
                    .map(|profile| profile.name.as_str())
                    .collect::<Vec<_>>()
                    .join(", ");
                Err(report! { error: "name a profile with -p: {names}" })
            }
        },
    }
}

#[cfg(test)]
mod tests {
    use super::Schedule;
    use crate::config::{TimeOfDay, Weekday};
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
