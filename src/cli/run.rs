//! What `run` and `config` actually do.

use crate::capture::{Inspection, inspect};
use crate::cli::{ConfigAction, ConfigArgs, Exit, RunArgs, render};
use crate::config::rules::RuleTree;
use crate::config::{Config, Parsed, Profile};
use crate::plan::{self, Plan};
use crate::platform;
use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;

pub fn run(args: &RunArgs) -> Exit {
    let Some(parsed) = load(args.config.clone()) else {
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

    if !args.dry_run {
        eprintln!("tycho: only --dry-run is implemented yet");
        return Exit::Failure;
    }

    let tree = match RuleTree::build(&profile.rule_set()) {
        Ok(tree) => tree,
        Err(error) => {
            eprintln!("tycho: {error}");
            return Exit::Failure;
        }
    };

    // No state file yet, so there is nothing to compare a shrink against.
    let previous = BTreeMap::new();
    let plan = match plan::build(profile, &tree, &previous, args.allow_shrink) {
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
    Exit::Ok
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

    let Some(parsed) = load(args.config.clone()) else {
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

fn load(override_path: Option<PathBuf>) -> Option<Parsed> {
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
        Ok(parsed) => Some(parsed),
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
