//! Layer 5. Installing, removing and reporting on the agents that make Tycho run
//! without anybody typing anything.
//!
//! Layer 5 rather than 4 because it is policy over a specific scheduler: it names a
//! definition, a registry and a verb, so it belongs on the same side of the line as
//! `platform`, not above it. Which scheduler is `platform::scheduler` - launchd on
//! macOS, Task Scheduler on Windows.
//!
//! Tycho has no resident process. This is the whole of the lifecycle: the OS
//! scheduler starts a run, it does its work, and it exits.

use crate::config::{Config, Profile, Schedule};
use crate::platform::log_dir;
use crate::platform::{Agent, Job, Loaded, scheduler};
use std::path::{Path, PathBuf};

#[derive(Debug, thiserror::Error)]
pub enum ServiceError {
    #[error(transparent)]
    Scheduler(#[from] scheduler::Error),
    #[error(transparent)]
    Path(#[from] crate::primitives::path::PathError),
    #[error("{context}: {source}")]
    Io {
        context: String,
        #[source]
        source: std::io::Error,
    },
    #[error(
        "{profile} has no schedule, so there is nothing to install; add a `schedule` key to it"
    )]
    NoSchedule { profile: String },
    #[error("cannot find this binary to write into the plist: {0}")]
    NoBinary(std::io::Error),
}

fn io(context: String) -> impl FnOnce(std::io::Error) -> ServiceError {
    move |source| ServiceError::Io { context, source }
}

/// What one agent is doing, for `service status` and `doctor`.
#[derive(Clone, Debug)]
pub struct Installed {
    pub agent: Agent,
    pub loaded: Loaded,
    /// Present when the plist exists, so drift between it and the config is visible.
    pub plist: Option<PathBuf>,
    /// The schedule actually written into the plist, which is not necessarily the
    /// one in the config. Drift between them is what killed the old system.
    pub scheduled: Option<Schedule>,
}

impl Installed {
    /// Whether the installed plist still matches what the config asks for.
    ///
    /// `None` when there is nothing installed to compare.
    #[must_use]
    pub fn matches(&self, wanted: Option<Schedule>) -> Option<bool> {
        let installed = self.scheduled?;
        Some(Some(installed) == wanted)
    }
}

/// The absolute path to this binary, resolved at install time so the plist never
/// depends on a `PATH` launchd does not have.
///
/// # Errors
///
/// If the running binary cannot be located.
pub fn binary() -> Result<PathBuf, ServiceError> {
    std::env::current_exe().map_err(ServiceError::NoBinary)
}

/// Installs a profile's backup agent and the shared catch-up agent.
///
/// The log directory is created **before** the plist is written: launchd silently
/// drops output when `StandardOutPath`'s directory does not exist, so the diagnostic
/// trail this design leans on would never have been written.
///
/// # Errors
///
/// If the profile has no schedule, or launchd refuses.
pub fn install(
    profile: &Profile,
    program: &Path,
    config: Option<&Path>,
) -> Result<Vec<Agent>, ServiceError> {
    let Some(schedule) = profile.schedule else {
        return Err(ServiceError::NoSchedule {
            profile: profile.name.to_string(),
        });
    };

    // A plist that omitted this reads the default config location instead, which on a
    // machine whose config lives anywhere else means the agent fails on every firing
    // with a file-not-found - loudly in its own log, and completely invisibly to
    // anyone not reading it. Found by letting launchd actually fire an agent.
    let config: Vec<String> = config
        .map(|path| vec!["--config".to_owned(), path.display().to_string()])
        .unwrap_or_default();

    let logs = log_dir()?;
    std::fs::create_dir_all(logs.as_path())
        .map_err(io(format!("creating {}", logs.as_path().display())))?;
    let agents = scheduler::definitions_dir()?;
    std::fs::create_dir_all(agents.as_path())
        .map_err(io(format!("creating {}", agents.as_path().display())))?;

    let backup = Agent::Backup(profile.name.to_string());
    let job = Job {
        agent: &backup,
        program,
        arguments: [
            vec!["run".to_owned(), profile.name.to_string()],
            config.clone(),
        ]
        .concat(),
        log_dir: logs.as_path(),
        log_stem: profile.name.to_string(),
    };
    write_and_load(&backup, &scheduler::backup_definition(&job, schedule))?;

    // One shared agent, so installing a second profile re-bootstraps rather than
    // duplicating it.
    let catchup = Agent::Catchup;
    let job = Job {
        agent: &catchup,
        program,
        arguments: [vec!["push".to_owned(), "--all".to_owned()], config].concat(),
        log_dir: logs.as_path(),
        log_stem: "catchup".to_owned(),
    };
    write_and_load(&catchup, &scheduler::catchup_definition(&job))?;

    Ok(vec![backup, catchup])
}

/// Writes a plist and loads it, booting out any previous copy first.
///
/// `bootstrap` on an already-loaded label fails, so this is what makes `install`
/// idempotent and `restart` a one-liner.
///
/// # Errors
///
/// If the plist cannot be written or launchd refuses.
pub fn write_and_load(agent: &Agent, definition: &str) -> Result<(), ServiceError> {
    let path = scheduler::definition_path(agent)?;
    let _ = scheduler::deregister(agent);
    crate::sys::fs::write_atomic(&path, &scheduler::encode(definition))
        .map_err(io(format!("writing {}", path.display())))?;
    scheduler::register(agent, &path)?;
    Ok(())
}

/// Unloads a profile's agent and removes its plist. **Never touches the store or any
/// backup.**
///
/// The catch-up agent is removed only when no profile's agent is left, since it is
/// shared.
///
/// # Errors
///
/// If a plist cannot be removed.
pub fn uninstall(profile: &Profile, remaining: usize) -> Result<Vec<Agent>, ServiceError> {
    let mut removed = vec![Agent::Backup(profile.name.to_string())];
    if remaining == 0 {
        removed.push(Agent::Catchup);
    }

    for agent in &removed {
        let _ = scheduler::deregister(agent);
        let path = scheduler::definition_path(agent)?;
        match std::fs::remove_file(&path) {
            Ok(()) => {}
            // Already gone is the answer `uninstall` wants: after a crash, or after a
            // hand-deleted plist, it must still leave nothing behind.
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(source) => return Err(io(format!("removing {}", path.display()))(source)),
        }
    }
    Ok(removed)
}

/// Reads what is installed, without changing anything.
///
/// # Errors
///
/// If `launchctl` cannot be run.
pub fn inspect(agent: Agent) -> Result<Installed, ServiceError> {
    let loaded = scheduler::state(&agent).map_err(scheduler::Error::from)?;
    let path = scheduler::definition_path(&agent)?;
    let text = std::fs::read(&path)
        .ok()
        .map(|bytes| scheduler::decode(&bytes));
    Ok(Installed {
        agent,
        loaded,
        plist: text.as_ref().map(|_| path),
        scheduled: text.as_deref().and_then(scheduler::scheduled_in),
    })
}

/// Every agent that could exist for a config, whether installed or not.
#[must_use]
pub fn agents_for(config: &Config) -> Vec<Agent> {
    let mut agents: Vec<Agent> = config
        .profiles
        .iter()
        .map(|profile| Agent::Backup(profile.name.to_string()))
        .collect();
    agents.push(Agent::Catchup);
    agents
}

#[cfg(test)]
mod tests {
    use crate::config::{Schedule, TimeOfDay, Weekday};
    use crate::platform::{Agent, Job, scheduler};
    use scheduler::scheduled_in;
    use std::path::Path;

    fn definition(schedule: Schedule) -> String {
        let agent = Agent::Backup("demo".to_owned());
        let job = Job {
            agent: &agent,
            program: Path::new("/bin/tycho"),
            arguments: vec!["run".to_owned(), "demo".to_owned()],
            log_dir: Path::new("/logs"),
            log_stem: "demo".to_owned(),
        };
        scheduler::backup_definition(&job, schedule)
    }

    /// Writer and reader pinned to each other. `doctor` compares the installed plist
    /// against the config, and drift between what the config says and what is
    /// actually installed is precisely what killed the old system - so a reader that
    /// silently failed to parse would report a match that is not there.
    #[test]
    fn every_schedule_shape_reads_back_out_of_its_own_plist() {
        for schedule in [
            Schedule::Weekly {
                day: Weekday::Sunday,
                at: TimeOfDay {
                    hour: 12,
                    minute: 0,
                },
            },
            Schedule::Weekly {
                day: Weekday::Saturday,
                at: TimeOfDay {
                    hour: 23,
                    minute: 59,
                },
            },
            Schedule::Daily {
                at: TimeOfDay { hour: 9, minute: 5 },
            },
            Schedule::Every(std::time::Duration::from_secs(3600)),
        ] {
            assert_eq!(
                scheduled_in(&definition(schedule)),
                Some(schedule),
                "{schedule:?} did not survive the round trip"
            );
        }
    }

    /// A daily plist has no `Weekday`, and reading one as weekly would report drift
    /// that is not there - or worse, agreement that is not there.
    #[test]
    fn daily_is_not_read_as_a_weekly_on_sunday() {
        let daily = definition(Schedule::Daily {
            at: TimeOfDay {
                hour: 12,
                minute: 0,
            },
        });
        assert!(matches!(scheduled_in(&daily), Some(Schedule::Daily { .. })));
    }

    #[test]
    fn a_document_that_is_not_ours_reads_as_no_schedule() {
        assert_eq!(scheduled_in("<plist><dict></dict></plist>"), None);
        assert_eq!(scheduled_in("<Task><Triggers /></Task>"), None);
    }
}
