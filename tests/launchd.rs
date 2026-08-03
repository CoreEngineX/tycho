//! The plists Tycho generates, checked by Apple's own parser rather than by reading
//! them.
//!
//! A hand-written plist that had drifted from what the script needed is a large part
//! of how the old system failed unnoticed, so these are generated - and a generator
//! nobody validated is the same problem one layer down.
//!
//! macOS only, and deliberately: the whole point is that `plutil` and not this crate
//! decides whether a plist is a plist. There is no Windows stand-in for that, so the
//! file is skipped rather than weakened into a string comparison. `tests/schtasks.rs`
//! is the equivalent for the Windows lifecycle, checked by `schtasks` itself.
#![cfg(target_os = "macos")]

use std::fs;
use std::path::Path;
use std::process::Command;
use tycho::config::{Schedule, TimeOfDay, Weekday};
use tycho::platform::launchd::{Agent, Job, backup_plist, catchup_plist, probe_plist};

fn lint(plist: &str, name: &str) {
    let dir = tempfile::tempdir().expect("temp dir");
    let path = dir.path().join(format!("{name}.plist"));
    fs::write(&path, plist).expect("write");

    let out = Command::new("plutil")
        .arg("-lint")
        .arg(&path)
        .output()
        .expect("plutil runs");
    assert!(
        out.status.success(),
        "plutil rejected {name}:\n{}\n---\n{plist}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// Reads a value back out through `plutil`, so the assertion is about what Apple
/// parses rather than about the string I happened to write.
fn value(plist: &str, keypath: &str) -> String {
    let dir = tempfile::tempdir().expect("temp dir");
    let path = dir.path().join("job.plist");
    fs::write(&path, plist).expect("write");

    let out = Command::new("plutil")
        .args(["-extract", keypath, "raw", "-o", "-"])
        .arg(&path)
        .output()
        .expect("plutil runs");
    assert!(
        out.status.success(),
        "plutil could not read {keypath}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_owned()
}

fn job<'a>(agent: &'a Agent, arguments: &[&str]) -> Job<'a> {
    Job {
        agent,
        program: Path::new("/Users/someone/.cargo/bin/tycho"),
        arguments: arguments.iter().map(|text| (*text).to_owned()).collect(),
        log_dir: Path::new("/Users/someone/Library/Logs/tycho"),
        log_stem: "demo".to_owned(),
    }
}

#[test]
fn every_generated_plist_parses() {
    let backup = Agent::Backup("demo".to_owned());
    for (name, plist) in [
        (
            "weekly",
            backup_plist(
                &job(&backup, &["run", "demo"]),
                Schedule::Weekly {
                    day: Weekday::Sunday,
                    at: TimeOfDay {
                        hour: 12,
                        minute: 0,
                    },
                },
            ),
        ),
        (
            "daily",
            backup_plist(
                &job(&backup, &["run", "demo"]),
                Schedule::Daily {
                    at: TimeOfDay {
                        hour: 18,
                        minute: 30,
                    },
                },
            ),
        ),
        (
            "interval",
            backup_plist(
                &job(&backup, &["run", "demo"]),
                Schedule::Every(std::time::Duration::from_secs(60)),
            ),
        ),
        (
            "catchup",
            catchup_plist(&job(&Agent::Catchup, &["push", "--all"])),
        ),
        (
            "probe",
            probe_plist(&job(&Agent::Probe, &["probe-access", "/Users/someone/A"])),
        ),
    ] {
        lint(&plist, name);
    }
}

/// A path with an ampersand in it is a real path somebody has, and XML gives that
/// character meaning. A plist that truncated at one would schedule a backup of the
/// wrong tree, at exit 0.
#[test]
fn a_hostile_path_survives_apples_parser_intact() {
    let hostile = "/Users/someone/R&D <2026> \"final\"";
    let agent = Agent::Backup("demo".to_owned());
    let plist = probe_plist(&job(&agent, &["probe-access", hostile]));

    lint(&plist, "hostile");
    assert_eq!(
        value(&plist, "ProgramArguments.2"),
        hostile,
        "the argument must come back out of the parser exactly as it went in"
    );
}

/// Sunday is 0, confirmed against `man launchd.plist` and now against the parser.
#[test]
fn the_weekly_schedule_reaches_launchd_as_the_documented_numbers() {
    let agent = Agent::Backup("demo".to_owned());
    let plist = backup_plist(
        &job(&agent, &["run", "demo"]),
        Schedule::Weekly {
            day: Weekday::Sunday,
            at: TimeOfDay {
                hour: 12,
                minute: 0,
            },
        },
    );
    assert_eq!(value(&plist, "StartCalendarInterval.Weekday"), "0");
    assert_eq!(value(&plist, "StartCalendarInterval.Hour"), "12");
    assert_eq!(value(&plist, "StartCalendarInterval.Minute"), "0");
    assert_eq!(value(&plist, "RunAtLoad"), "false");
}

/// Both output paths are set. launchd sends output to `/dev/null` when the path is
/// absent, which is a silent-failure surface in a tool whose thesis is that failure
/// is loud.
#[test]
fn the_output_paths_are_real_paths_the_parser_returns() {
    let agent = Agent::Backup("demo".to_owned());
    let plist = backup_plist(
        &job(&agent, &["run", "demo"]),
        Schedule::Daily {
            at: TimeOfDay { hour: 9, minute: 5 },
        },
    );
    assert_eq!(
        value(&plist, "StandardOutPath"),
        "/Users/someone/Library/Logs/tycho/demo.out.log"
    );
    assert_eq!(
        value(&plist, "StandardErrorPath"),
        "/Users/someone/Library/Logs/tycho/demo.err.log"
    );
}

/// `StartOnMount` fires on every mount - a DMG, a network share, an APFS snapshot -
/// so the throttle is what bounds the churn.
#[test]
fn the_catchup_agent_mounts_intervals_and_throttles() {
    let plist = catchup_plist(&job(&Agent::Catchup, &["push", "--all"]));
    assert_eq!(value(&plist, "StartOnMount"), "true");
    assert_eq!(value(&plist, "StartInterval"), "3600");
    assert_eq!(value(&plist, "ThrottleInterval"), "10");
    assert_eq!(value(&plist, "ProgramArguments.1"), "push");
    assert_eq!(value(&plist, "ProgramArguments.2"), "--all");
}
