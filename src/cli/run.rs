//! What `run` and `config` actually do.

use crate::capture::{Inspection, inspect};
use crate::cli::{ConfigAction, ConfigArgs, Exit, RunArgs, render};
use crate::config::{Config, Parsed, Profile};
use crate::plan::Plan;
use crate::platform;
use crate::state::State;
use crate::store::{self, Store};
use std::fs;
use std::path::PathBuf;

pub fn run(args: &RunArgs) -> Exit {
    let Some((parsed, config_text)) = load(args.config.clone()) else {
        return Exit::Failure;
    };
    if parsed.has_errors() {
        eprint!("{}", render::config_check(&[], &parsed.diagnostics));
        eprintln!("tycho: the config has errors, so nothing was backed up");
        return Exit::Failure;
    }
    let Some(profile) = select(&parsed.config, args.profile.as_deref()) else {
        return Exit::Failure;
    };

    let Some(paths) = locations(profile) else {
        return Exit::Failure;
    };
    let mut state = match State::load(paths.state.as_path()) {
        Ok(state) => state,
        Err(error) => {
            eprintln!("tycho: {error}");
            return Exit::Failure;
        }
    };

    if args.dry_run {
        let previous = state.last_entries(profile.name.as_str());
        let plan = match store::run::dry(profile, &previous, args.allow_shrink) {
            Ok(plan) => plan,
            Err(error) => {
                eprintln!("tycho: {error}");
                return Exit::Failure;
            }
        };
        let repos = if args.quick {
            Vec::new()
        } else {
            inspect_all(&plan)
        };
        print!("{}", render::dry_run(&plan, &repos, args.quick));
        for warning in plan.warnings() {
            eprintln!("warn  {warning:?}");
        }
        return Exit::Ok;
    }

    let store = match Store::open_or_init(&paths.store) {
        Ok(store) => store,
        Err(error) => {
            eprintln!("tycho: {error}");
            return Exit::Failure;
        }
    };
    let run_paths = store::run::Paths {
        lock: paths.lock.as_path(),
        state: paths.state.as_path(),
        config_text: Some(&config_text),
    };
    let done = match store::run::execute(profile, &store, &run_paths, &mut state, args.allow_shrink)
    {
        Ok(done) => done,
        Err(error) => {
            eprintln!("tycho: {error}");
            return Exit::Failure;
        }
    };

    print!("{}", render::run_result(profile.name.as_str(), &done));
    for warning in &done.warnings {
        eprintln!("warn  {warning}");
    }

    // A quiet green run over an incomplete backup is the exact failure this project
    // exists to correct, so anything the run did not manage to take is said out loud.
    let mut incomplete = Exit::Ok;
    let missed = done
        .summary
        .repos_found
        .saturating_sub(done.summary.repos_captured);
    if missed > 0 {
        eprintln!(
            "warn  {missed} of {} repositories were not captured",
            done.summary.repos_found
        );
        incomplete = Exit::Warning;
    }
    if !profile.remotes.is_empty() {
        eprintln!("warn  nothing was pushed; remotes are not built yet");
        incomplete = Exit::Warning;
    }
    incomplete
}

/// Where a profile's three files live.
struct Locations {
    store: crate::primitives::path::AbsPath,
    state: crate::primitives::path::AbsPath,
    lock: crate::primitives::path::AbsPath,
}

fn locations(profile: &Profile) -> Option<Locations> {
    let store = platform::store_path(profile.name.as_str(), profile.store_path.as_ref());
    let state = platform::state_path();
    let lock = platform::data_dir();
    match (store, state, lock) {
        (Ok(store), Ok(state), Ok(dir)) => {
            // First run on a machine: nothing has made the data directory yet, and
            // the lock file cannot be created inside one that is not there.
            if let Err(error) = std::fs::create_dir_all(dir.as_path()) {
                eprintln!("tycho: creating {dir}: {error}");
                return None;
            }
            let lock = crate::primitives::path::AbsPath::from_absolute(
                &dir.as_path().join(format!("{}.lock", profile.name)),
            );
            match lock {
                Ok(lock) => Some(Locations { store, state, lock }),
                Err(error) => {
                    eprintln!("tycho: {error}");
                    None
                }
            }
        }
        (Err(error), ..) | (_, Err(error), _) | (_, _, Err(error)) => {
            eprintln!("tycho: {error}");
            None
        }
    }
}

pub fn history(args: &crate::cli::HistoryArgs) -> Exit {
    let Some((parsed, _)) = load(args.config.clone()) else {
        return Exit::Failure;
    };
    let Some(profile) = select(&parsed.config, args.profile.as_deref()) else {
        return Exit::Failure;
    };
    let Some(paths) = locations(profile) else {
        return Exit::Failure;
    };
    let store = match Store::open_or_init(&paths.store) {
        Ok(store) => store,
        Err(error) => {
            eprintln!("tycho: {error}");
            return Exit::Failure;
        }
    };
    match store.history(args.count) {
        Ok(backups) => {
            print!("{}", render::history(&backups));
            Exit::Ok
        }
        Err(error) => {
            eprintln!("tycho: {error}");
            Exit::Failure
        }
    }
}

pub fn config(args: &ConfigArgs) -> Exit {
    let path = match resolve(args.config.clone()) {
        Ok(path) => path,
        Err(error) => {
            eprintln!("tycho: {error}");
            return Exit::Failure;
        }
    };

    if matches!(args.action, ConfigAction::Path) {
        println!("{}", path.display());
        return Exit::Ok;
    }

    let Some((parsed, _)) = load(args.config.clone()) else {
        return Exit::Failure;
    };
    let summaries: Vec<String> = parsed.config.profiles.iter().map(summarise).collect();
    print!("{}", render::config_check(&summaries, &parsed.diagnostics));

    if parsed.has_errors() {
        Exit::Failure
    } else if parsed.diagnostics.is_empty() {
        Exit::Ok
    } else {
        Exit::Warning
    }
}

/// The echo `cli.md` section 9 prints: reading your own config back in summarised
/// form is how a remote attached to the wrong profile becomes visible.
fn summarise(profile: &Profile) -> String {
    let plural =
        |count: usize, word: &str| format!("{count} {word}{}", if count == 1 { "" } else { "s" });
    let schedule = profile.schedule.map_or_else(
        || "no schedule".to_owned(),
        |schedule| match schedule {
            crate::config::Schedule::Daily { at } => format!("daily {at}"),
            crate::config::Schedule::Weekly { day, at } => format!("{day:?} {at}").to_lowercase(),
            crate::config::Schedule::Every(every) => format!("every {}s", every.as_secs()),
        },
    );
    format!(
        "{:<18}{}, {}, {}, {}, {schedule}",
        profile.name,
        plural(profile.watch.len(), "root"),
        plural(
            profile.ignore_paths.len() + profile.ignore_globs.len(),
            "ignore"
        ),
        plural(profile.reinclude.len(), "reinclude"),
        plural(profile.remotes.len(), "remote"),
    )
}

fn resolve(override_path: Option<PathBuf>) -> Result<PathBuf, String> {
    match override_path {
        Some(path) => Ok(path),
        None => platform::config_path()
            .map(|path| path.as_path().to_path_buf())
            .map_err(|error| error.to_string()),
    }
}

fn load(override_path: Option<PathBuf>) -> Option<(Parsed, String)> {
    let path = match resolve(override_path) {
        Ok(path) => path,
        Err(error) => {
            eprintln!("tycho: {error}");
            return None;
        }
    };
    let text = match fs::read_to_string(&path) {
        Ok(text) => text,
        Err(error) => {
            eprintln!("tycho: cannot read {}: {error}", path.display());
            return None;
        }
    };
    match crate::config::parse(&text) {
        Ok(parsed) => Some((parsed, text)),
        Err(error) => {
            eprintln!("tycho: {error}");
            None
        }
    }
}

fn select<'a>(config: &'a Config, wanted: Option<&str>) -> Option<&'a Profile> {
    match wanted {
        Some(name) => {
            let found = config
                .profiles
                .iter()
                .find(|profile| profile.name.as_str() == name);
            if found.is_none() {
                eprintln!("tycho: no profile named '{name}'");
            }
            found
        }
        None => match config.profiles.as_slice() {
            [only] => Some(only),
            [] => {
                eprintln!("tycho: the config defines no profiles");
                None
            }
            many => {
                let names: Vec<&str> = many.iter().map(|p| p.name.as_str()).collect();
                eprintln!("tycho: name a profile: {}", names.join(", "));
                None
            }
        },
    }
}

fn inspect_all(plan: &Plan) -> Vec<(String, Inspection)> {
    let mut out = Vec::new();
    for root in &plan.roots {
        for repo in root.repos() {
            match inspect(repo.source.as_path()) {
                Ok(inspection) => out.push((repo.key.clone(), inspection)),
                Err(error) => eprintln!("warn  {}: {error}", repo.key),
            }
        }
    }
    out
}
