//! `tycho schedule`: show, set or clear a profile's run schedule.

use crate::cli::report::report;
use crate::cli::{Exit, ScheduleAction, ScheduleArgs, render};
use crate::config::{Config, Profile, Schedule};
use crate::config_edit::Editing;
use std::path::PathBuf;

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
    let name = render::clip(profile.name.as_str(), 13);
    match profile.schedule {
        Some(schedule) => println!("{name:<14}  {}", render::say(schedule)),
        None => println!("{name:<14}  no schedule, runs only when invoked by hand"),
    }
    Exit::Ok
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
