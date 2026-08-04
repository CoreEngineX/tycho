//! What `tycho doctor` and the hidden `probe-access` do.

use crate::cli::{DoctorArgs, Exit, ProbeArgs, render};
use crate::doctor::{self, Scope, Verdict};
use crate::platform::log_dir;
use crate::platform::{Agent, Job, scheduler};
use crate::state::State;
use std::path::PathBuf;

pub fn dispatch(args: &DoctorArgs) -> Exit {
    let Some((parsed, _)) = crate::cli::run::load(args.config.clone()) else {
        return Exit::Failure;
    };

    let mut config = parsed.config;
    if let Some(wanted) = args.profile.as_deref() {
        config
            .profiles
            .retain(|profile| profile.name.as_str() == wanted);
        if config.profiles.is_empty() {
            eprintln!("tycho: no profile named '{wanted}'");
            return Exit::Failure;
        }
    }

    let state = crate::platform::state_path()
        .ok()
        .and_then(|path| State::load(path.as_path()).ok())
        .unwrap_or_default();

    let mut report = doctor::run(
        &config,
        &state,
        &Scope {
            deep: args.deep,
            remote: args.remote.clone(),
        },
    );

    // The config's own diagnostics belong in the report rather than beside it: one
    // command that aggregates health should not need a second one run alongside.
    if !parsed.diagnostics.is_empty()
        && let Some(environment) = report.sections.first_mut()
    {
        for check in &mut environment.checks {
            if check.name == "config" {
                let errors = parsed
                    .diagnostics
                    .iter()
                    .filter(|item| item.severity == crate::config::Severity::Error)
                    .count();
                check.verdict = if errors > 0 {
                    Verdict::Fail
                } else {
                    Verdict::Warn
                };
                check.evidence = format!(
                    "{}, {}",
                    render::plural(errors, "error"),
                    render::plural(parsed.diagnostics.len() - errors, "warning")
                );
            }
        }
    }

    if args.deep
        && let Some(access) = probe_through_scheduler(&config)
        && let Some(environment) = report.sections.first_mut()
    {
        for check in &mut environment.checks {
            if check.name == "full disk access" {
                *check = access.clone();
            }
        }
    }

    print!("{}", render::doctor(&report));

    // The same policy as everything else, so the one command that aggregates health
    // cannot report a failure while exiting 0.
    let (failures, warnings) = report.counts();
    if failures > 0 {
        Exit::Failure
    } else if warnings > 0 {
        Exit::Warning
    } else {
        Exit::Ok
    }
}

/// Measures the **agent's** Full Disk Access grant, which is the only way to measure
/// it at all.
///
/// An interactive `doctor` runs with the terminal's TCC grant, a different grant from
/// the agent's, so reading a watched root here proves nothing about what the agent can
/// do. The only mechanism that measures the real thing is to run the probe through
/// launchd - the same lifecycle `service install` already implements.
fn probe_through_scheduler(config: &crate::config::Config) -> Option<doctor::Check> {
    let roots: Vec<String> = config
        .profiles
        .iter()
        .flat_map(|profile| &profile.watch)
        .map(|entry| entry.path().as_path().display().to_string())
        .collect();
    if roots.is_empty() {
        return None;
    }

    let program = crate::service::binary().ok()?;
    let logs = log_dir().ok()?;
    std::fs::create_dir_all(logs.as_path()).ok()?;
    let out = logs.as_path().join("probe-access.json");
    let _ = std::fs::remove_file(&out);

    let agent = Agent::Probe;
    let mut arguments = vec![
        "probe-access".to_owned(),
        "--out".to_owned(),
        out.display().to_string(),
    ];
    arguments.extend(roots.iter().cloned());
    let job = Job {
        agent: &agent,
        program: &program,
        arguments,
        log_dir: logs.as_path(),
        log_stem: "probe".to_owned(),
    };

    // Transient by design: bootstrapped, observed, booted out. Leaving it loaded would
    // put a job on the machine that exists only to answer one question.
    if crate::service::write_and_load(&agent, &scheduler::probe_definition(&job)).is_err() {
        return None;
    }
    let found = wait_for(&out);
    let _ = scheduler::deregister(&agent);
    let _ = std::fs::remove_file(&out);
    // The definition goes too, not just the loaded job. Deregistering leaves the file,
    // and a health check that litters the agent directory with a job nobody installed
    // is one people learn to distrust. Asked of `scheduler` rather than of `Agent`,
    // whose `plist_path` is launchd's answer on every platform - `write_and_load` wrote
    // to `scheduler::definition_path`, so on Windows this removed a file that was never
    // there and orphaned the one that was.
    if let Ok(definition) = crate::platform::scheduler::definition_path(&agent) {
        let _ = std::fs::remove_file(definition);
    }

    Some(match found {
        Some(text) => {
            let denied: Vec<&str> = text
                .lines()
                .filter_map(|line| line.strip_prefix("denied "))
                .collect();
            if denied.is_empty() {
                doctor::Check {
                    name: "full disk access".to_owned(),
                    verdict: Verdict::Ok,
                    evidence: format!("the agent read all {}", render::plural(roots.len(), "root")),
                }
            } else {
                doctor::Check {
                    name: "full disk access".to_owned(),
                    verdict: Verdict::Fail,
                    evidence: format!(
                        "the agent cannot read {}; add tycho in System Settings",
                        denied[0]
                    ),
                }
            }
        }
        None => doctor::Check {
            name: "full disk access".to_owned(),
            verdict: Verdict::Warn,
            evidence: "the probe did not report back in time".to_owned(),
        },
    })
}

/// launchd starts the job when it is ready, not when we ask, so this polls rather
/// than assuming.
fn wait_for(path: &std::path::Path) -> Option<String> {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    while std::time::Instant::now() < deadline {
        if let Ok(text) = std::fs::read_to_string(path) {
            return Some(text);
        }
        std::thread::sleep(std::time::Duration::from_millis(250));
    }
    None
}

/// The hidden `probe-access`, run **by launchd** so that what it measures is the
/// agent's grant rather than the terminal's.
///
/// A TCC denial is not a loud failure on its own - it is a read error on a directory.
/// What makes it loud is the sanity gate in `capture.md`: a root that yields zero
/// capturable entries fails the run outright, so a TCC-denied root cannot produce a
/// green empty backup. This exists to say so before that happens.
pub fn probe_access(args: &ProbeArgs) -> Exit {
    let mut report = String::new();
    for root in &args.roots {
        let line = match std::fs::read_dir(root) {
            Ok(mut entries) => {
                // Opening a directory can succeed where reading an entry is denied,
                // so this takes one entry rather than trusting the open.
                match entries.next() {
                    Some(Err(error)) => format!("denied {} ({error})\n", root.display()),
                    _ => format!("read {}\n", root.display()),
                }
            }
            Err(error) => format!("denied {} ({error})\n", root.display()),
        };
        report.push_str(&line);
    }

    let out: PathBuf = args.out.clone();
    match crate::sys::fs::write_atomic(&out, report.as_bytes()) {
        Ok(()) => Exit::Ok,
        Err(error) => {
            eprintln!("tycho: writing {}: {error}", out.display());
            Exit::Failure
        }
    }
}
