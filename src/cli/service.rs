//! What `tycho service` does.

use crate::cli::report::{Diagnostic, at_profile, report};
use crate::cli::{Exit, ServiceAction, ServiceArgs, render};
use crate::config::Profile;
use crate::platform::Agent;
use crate::service::{self, ServiceError};

pub fn dispatch(args: &ServiceArgs) -> Exit {
    let Some((parsed, _)) = crate::cli::run::load(args.config.clone()) else {
        return Exit::Failure;
    };
    if parsed.has_errors() && !matches!(args.action, ServiceAction::Status) {
        eprint!("{}", render::config_check(&[], &parsed.diagnostics));
        return report! {
            error: "the config has errors, so no agent was touched",
            recovery: { "tycho config check" => "shows every error and warning" },
        };
    }

    // No profile named means every profile: installing one at a time is the special
    // case, not the general one.
    let wanted: Vec<&Profile> = if let Some(name) = args.profile.as_deref() {
        let Some(profile) = parsed
            .config
            .profiles
            .iter()
            .find(|profile| profile.name.as_str() == name)
        else {
            return report! {
                error: "no profile named '{name}'",
                at: at_profile(name),
                recovery: { "tycho profile list" => "names every profile this config defines" },
            };
        };
        vec![profile]
    } else {
        parsed.config.profiles.iter().collect()
    };
    if wanted.is_empty() {
        return report! {
            error: "the config has no profiles",
            recovery: { "tycho profile add <name> --local-only" => "or with --remote NAME=PATH" },
        };
    }

    match args.action {
        ServiceAction::Install => install(&wanted, args.config.as_deref()),
        ServiceAction::Restart => {
            // bootout then bootstrap, which `write_and_load` already does on every
            // install, so a restart is an install against the current config.
            install(&wanted, args.config.as_deref())
        }
        ServiceAction::Uninstall => uninstall(&wanted, &parsed.config),
        ServiceAction::Status => status(&parsed.config),
    }
}

fn install(profiles: &[&Profile], config: Option<&std::path::Path>) -> Exit {
    let program = match service::binary() {
        Ok(path) => path,
        Err(error) => return report! { error: "{error}" },
    };
    // Absolute, because launchd runs the agent from `/` and a relative `--config`
    // would resolve against a directory nobody chose.
    let config = match config.map(std::path::absolute).transpose() {
        Ok(config) => config,
        Err(error) => return report! { error: "{error}" },
    };

    let mut installed = Vec::new();
    for profile in profiles {
        match service::install(profile, &program, config.as_deref()) {
            Ok(agents) => installed.extend(agents),
            Err(error @ ServiceError::NoSchedule { .. }) => {
                let name = profile.name.as_str();
                return report! {
                    error: "{error}",
                    at: at_profile(name),
                    recovery: {
                        "tycho schedule set daily:09:00 -p {name}" => "any of the accepted forms works",
                    },
                };
            }
            Err(error) => {
                return report! { error: "{error}", at: at_profile(profile.name.as_str()) };
            }
        }
    }
    installed.sort_by_key(Agent::label);
    installed.dedup();

    print!("{}", render::service_installed(&installed, &program));
    Exit::Ok
}

fn uninstall(profiles: &[&Profile], config: &crate::config::Config) -> Exit {
    for profile in profiles {
        // The catch-up agent is shared, so it goes only when the last profile does.
        let remaining = config
            .profiles
            .iter()
            .filter(|other| {
                other.name != profile.name && !profiles.iter().any(|going| going.name == other.name)
            })
            .count();
        match service::uninstall(profile, remaining) {
            Ok(removed) => {
                for agent in removed {
                    println!("removed   {}", agent.label());
                }
            }
            Err(error) => {
                return report! { error: "{error}", at: at_profile(profile.name.as_str()) };
            }
        }
    }
    println!("\nthe store and every backup are untouched");
    Exit::Ok
}

fn status(config: &crate::config::Config) -> Exit {
    let mut rows = Vec::new();
    for agent in service::agents_for(config) {
        let wanted = match &agent {
            Agent::Backup(name) => Some(
                config
                    .profiles
                    .iter()
                    .find(|profile| profile.name.as_str() == name)
                    .and_then(|profile| profile.schedule),
            ),
            // The shared agents carry a fixed schedule of their own, so there is no
            // config value to drift from and nothing to compare.
            Agent::Catchup | Agent::Probe => None,
        };
        let location = match &agent {
            Agent::Backup(name) => Some(at_profile(name)),
            Agent::Catchup | Agent::Probe => None,
        };
        match service::inspect(agent) {
            Ok(installed) => rows.push((installed, wanted)),
            Err(error) => {
                let mut diagnostic = Diagnostic::error(error.to_string());
                if let Some(location) = location {
                    diagnostic = diagnostic.location(location);
                }
                return diagnostic.emit();
            }
        }
    }

    print!("{}", render::service_status(&rows));
    // A non-zero last exit is exactly the evidence that revealed a year of silent
    // failure in the old system, and nobody had reason to look. This looks.
    let broken = rows.iter().any(|(installed, wanted)| {
        matches!(
            installed.loaded,
            crate::platform::Loaded::Yes { last, .. } if !last.is_clean()
        ) || wanted.is_some_and(|wanted| installed.matches(wanted) == Some(false))
    });
    if broken { Exit::Failure } else { Exit::Ok }
}
