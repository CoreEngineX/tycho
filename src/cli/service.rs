//! What `tycho service` does.

use crate::cli::{Exit, ServiceAction, ServiceArgs, render};
use crate::config::Profile;
use crate::platform::Agent;
use crate::service;

pub fn dispatch(args: &ServiceArgs) -> Exit {
    let Some((parsed, _)) = crate::cli::run::load(args.config.clone()) else {
        return Exit::Failure;
    };
    if parsed.has_errors() && !matches!(args.action, ServiceAction::Status) {
        eprint!("{}", render::config_check(&[], &parsed.diagnostics));
        eprintln!("tycho: the config has errors, so no agent was touched");
        return Exit::Failure;
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
            eprintln!("tycho: no profile named '{name}'");
            return Exit::Failure;
        };
        vec![profile]
    } else {
        parsed.config.profiles.iter().collect()
    };
    if wanted.is_empty() {
        eprintln!("tycho: the config has no profiles");
        return Exit::Failure;
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
        Err(error) => {
            eprintln!("tycho: {error}");
            return Exit::Failure;
        }
    };
    // Absolute, because launchd runs the agent from `/` and a relative `--config`
    // would resolve against a directory nobody chose.
    let config = match config.map(std::path::absolute).transpose() {
        Ok(config) => config,
        Err(error) => {
            eprintln!("tycho: {error}");
            return Exit::Failure;
        }
    };

    let mut installed = Vec::new();
    for profile in profiles {
        match service::install(profile, &program, config.as_deref()) {
            Ok(agents) => installed.extend(agents),
            Err(error) => {
                eprintln!("tycho: {error}");
                return Exit::Failure;
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
                eprintln!("tycho: {error}");
                return Exit::Failure;
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
        match service::inspect(agent) {
            Ok(installed) => rows.push((installed, wanted)),
            Err(error) => {
                eprintln!("tycho: {error}");
                return Exit::Failure;
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
