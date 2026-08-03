//! What `run` and `config` actually do.

use crate::capture::{Inspection, inspect};
use crate::cli::{ConfigAction, ConfigArgs, Exit, RunArgs, render};
use crate::config::{Config, Parsed, Profile, Schedule};
use crate::plan::Plan;
use crate::platform;
use crate::remote::state::RemoteState;
use crate::restore::{self, resolve};
use crate::state::State;
use crate::store::{self, Store};
use std::fs;
use std::path::{Path, PathBuf};

pub fn run(args: &RunArgs) -> Exit {
    let Some((parsed, config_text)) = load(args.config.clone()) else {
        return Exit::Failure;
    };
    if parsed.has_errors() {
        eprint!("{}", render::config_check(&[], &parsed.diagnostics));
        eprintln!("tycho: the config has errors, so nothing was backed up");
        return Exit::Failure;
    }
    let Some(profiles) = selected(&parsed.config, args.profile.as_deref(), args.all) else {
        return Exit::Failure;
    };

    // One profile's failure must not stop the others: the whole point of separate
    // profiles is that they are independent backups.
    let mut worst = Exit::Ok;
    for profile in profiles {
        worst = worse(worst, one_run(args, profile, &config_text));
    }
    worst
}

fn one_run(args: &RunArgs, profile: &Profile, config_text: &str) -> Exit {
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
        config_text: Some(config_text),
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
    match (report(&done.remotes), incomplete) {
        (Exit::Failure, _) => Exit::Failure,
        (Exit::Warning, _) | (Exit::Ok, Exit::Warning) => Exit::Warning,
        (Exit::Ok, other) => other,
    }
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
    if let Some(path) = &args.path {
        return path_history(&store, path, args.count);
    }
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

/// `history --path`, which has two answer shapes and says which it is showing.
///
/// A plain file or an overlay entry answers from the store's own commits - backup
/// runs. A tracked file answers from its repository's commits, which is what you
/// want: "the version before I broke it" is a commit in your repo, not a Sunday.
fn path_history(store: &Store, path: &Path, count: usize) -> Exit {
    let Some(backup) = newest(store) else {
        return Exit::Failure;
    };
    let resolved = match resolve::resolve(store, &backup, path) {
        Ok(resolved) => resolved,
        Err(error) => {
            eprintln!("tycho: {error}");
            return Exit::Failure;
        }
    };
    match resolve::history(store, &resolved, count) {
        Ok(commits) => {
            print!("{}", render::path_history(path, &resolved, &commits));
            Exit::Ok
        }
        Err(error) => {
            eprintln!("tycho: {error}");
            Exit::Failure
        }
    }
}

/// The newest backup, read once.
fn newest(store: &Store) -> Option<resolve::Backup> {
    let commit = match store.at(None) {
        Ok(Some(commit)) => commit,
        Ok(None) => {
            eprintln!("tycho: this store holds no backups yet");
            return None;
        }
        Err(error) => {
            eprintln!("tycho: {error}");
            return None;
        }
    };
    match resolve::Backup::read(store, commit) {
        Ok(backup) => Some(backup),
        Err(error) => {
            eprintln!("tycho: {error}");
            None
        }
    }
}

/// `tycho push`: the catch-up in `remotes.md` section 4b. No capture at all, so what
/// is in a backup never depends on when a drive was plugged in.
pub fn push(args: &crate::cli::PushArgs) -> Exit {
    let Some((parsed, config_text)) = load(args.config.clone()) else {
        return Exit::Failure;
    };
    let Some(profiles) = selected(&parsed.config, args.profile.as_deref(), args.all) else {
        return Exit::Failure;
    };
    let mut worst = Exit::Ok;
    for profile in profiles {
        worst = worse(worst, one_push(profile, &config_text));
    }
    worst
}

fn one_push(profile: &Profile, config_text: &str) -> Exit {
    if profile.remotes.is_empty() {
        eprintln!("tycho: {} has no remotes to push to", profile.name);
        return Exit::Ok;
    }
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
        config_text: Some(config_text),
    };

    match store::run::catch_up(profile, &store, &run_paths, &mut state) {
        // A run in progress is about to push anyway, so this is a success, not a
        // contention error to retry.
        Ok(None) => Exit::Ok,
        Ok(Some(results)) => {
            let span = store.span().ok().flatten();
            print!(
                "{}",
                render::status(&[snapshot(profile, &state, span.as_ref())], None)
            );
            report(&results)
        }
        Err(error) => {
            eprintln!("tycho: {error}");
            Exit::Failure
        }
    }
}

/// The worse of two outcomes, so a fan-out over profiles reports the worst thing
/// that happened rather than the last thing.
const fn worse(a: Exit, b: Exit) -> Exit {
    match (a, b) {
        (Exit::Failure, _) | (_, Exit::Failure) => Exit::Failure,
        (Exit::Warning, _) | (_, Exit::Warning) => Exit::Warning,
        (Exit::Ok, Exit::Ok) => Exit::Ok,
    }
}

/// What the remotes did, as an exit code.
///
/// Yellow is said out loud and still exits 0, which is what makes `run` and
/// `status --check` agree: an optional remote merely unplugged is yellow in both. A
/// monitor that cries wolf every time a drive is out is a monitor people turn off.
/// Code 3 belongs to `status --check` and `doctor`, not here.
fn report(results: &[store::run::RemoteResult]) -> Exit {
    let mut exit = Exit::Ok;
    for result in results {
        match &result.state {
            RemoteState::Failed { reason, .. } => {
                eprintln!("error {}: {reason}", result.name);
                exit = Exit::Failure;
            }
            RemoteState::Behind { runs, .. } => eprintln!(
                "warn  {} is behind {runs} of {}",
                result.name, result.tolerance
            ),
            RemoteState::Synced { .. } | RemoteState::Unseen => {}
        }
    }
    exit
}

/// `tycho status`, which must render when everything else is broken.
pub fn status(args: &crate::cli::StatusArgs) -> Exit {
    let Some((parsed, _)) = load(args.config.clone()) else {
        return Exit::Failure;
    };
    let banner = if parsed.has_errors() {
        Some("config unreadable, showing the last run instead")
    } else {
        None
    };

    let state = match platform::state_path().map(|path| State::load(path.as_path())) {
        Ok(Ok(state)) => state,
        Ok(Err(error)) => {
            eprintln!("tycho: {error}");
            State::default()
        }
        Err(error) => {
            eprintln!("tycho: {error}");
            State::default()
        }
    };

    let wanted = args.profile.as_deref();
    let profiles: Vec<&Profile> = parsed
        .config
        .profiles
        .iter()
        .filter(|profile| wanted.is_none_or(|name| profile.name.as_str() == name))
        .collect();
    if profiles.is_empty() {
        eprintln!("tycho: no profile named '{}'", wanted.unwrap_or(""));
        return Exit::Failure;
    }

    let reports: Vec<render::ProfileStatus> = profiles
        .iter()
        .map(|profile| {
            // A store that will not open still has a state file, so the profile keeps
            // its line rather than the whole report failing.
            let span = locations(profile)
                .and_then(|paths| Store::open_or_init(&paths.store).ok())
                .and_then(|store| store.span().ok())
                .flatten();
            snapshot(profile, &state, span.as_ref())
        })
        .collect();
    print!("{}", render::status(&reports, banner));

    if !args.check {
        return Exit::Ok;
    }
    let mut red = false;
    let mut yellow = false;
    for profile in &profiles {
        for remote in &profile.remotes {
            let current = state.remote(profile.name.as_str(), remote.name.as_str());
            red |= current.is_red();
            yellow |= current.is_yellow();
        }
    }
    if red {
        return Exit::Failure;
    }
    if yellow && args.strict {
        return Exit::Warning;
    }
    Exit::Ok
}

/// One profile's status line, its subtitle, and one row per remote.
fn snapshot(
    profile: &Profile,
    state: &State,
    span: Option<&crate::store::Span>,
) -> render::ProfileStatus {
    let last = state.last(profile.name.as_str());
    render::ProfileStatus {
        name: profile.name.to_string(),
        next_run: profile.schedule.and_then(|schedule| {
            schedule
                .next_after(&jiff::Timestamp::now().to_zoned(jiff::tz::TimeZone::system()))
                .ok()
                .map(|next| render::upcoming(&next))
        }),
        backups: span.map_or(0, |span| span.backups),
        since: span.map(|span| render::day(&span.oldest)),
        newest: span.map(|span| render::moment(&span.newest)),
        store_bytes: last.map_or(0, |run| run.store_bytes),
        remotes: profile
            .remotes
            .iter()
            .map(|remote| row(remote, profile, state))
            .collect(),
    }
}

fn row(remote: &crate::config::Remote, profile: &Profile, state: &State) -> render::RemoteRow {
    let current = state.remote(profile.name.as_str(), remote.name.as_str());
    let tolerance = store::run::tolerance(remote);
    let (detail, note, hint) = match &current {
        RemoteState::Unseen => (
            "never pushed".to_owned(),
            if remote.optional {
                "optional".to_owned()
            } else {
                String::new()
            },
            None,
        ),
        // `verified` appears only when the full ref-set comparison actually passed,
        // never as decoration - it is what distinguishes a verified push from a push
        // command that merely returned success.
        RemoteState::Synced { at, .. } => (
            format!("pushed {}", render::moment(at)),
            "verified".to_owned(),
            None,
        ),
        RemoteState::Behind { last_seen, .. } => (
            if last_seen.is_empty() {
                "never seen".to_owned()
            } else {
                format!("last seen {}", render::day(last_seen))
            },
            if remote.optional {
                "optional, on next mount".to_owned()
            } else {
                "required".to_owned()
            },
            None,
        ),
        RemoteState::Failed { at, reason } => (
            format!("{reason}"),
            render::moment(at),
            Some(format!("tycho log {}", profile.name)),
        ),
    };
    render::RemoteRow {
        name: remote.name.to_string(),
        word: current.word(tolerance),
        detail,
        note,
        hint,
    }
}

/// `tycho restore`. Never writes into your live tree.
pub fn restore(args: &crate::cli::RestoreArgs) -> Exit {
    let store = match open_source(args) {
        Some(store) => store,
        None => return Exit::Failure,
    };

    let at = match args.at.as_deref() {
        Some(text) => {
            let now = jiff::Timestamp::now().to_zoned(jiff::tz::TimeZone::system());
            match restore::when::parse(text, &now) {
                Ok(stamp) => Some(stamp),
                Err(error) => {
                    eprintln!("tycho: {error}");
                    return Exit::Failure;
                }
            }
        }
        None => None,
    };

    match store.span() {
        Ok(Some(span)) => println!(
            "reading   {}, {} to {}",
            render::plural(span.backups, "backup"),
            render::day(&span.oldest),
            render::day(&span.newest)
        ),
        Ok(None) => {
            eprintln!("tycho: this store holds no backups yet");
            return Exit::Failure;
        }
        Err(error) => {
            eprintln!("tycho: {error}");
            return Exit::Failure;
        }
    }

    let wanted = restore::Wanted {
        paths: args.paths.clone(),
        bundle: args.bundle,
    };
    match restore::execute(&store, &args.into, at.as_ref(), &wanted, args.force) {
        Ok(done) => {
            print!("{}", render::restored(&args.into, &done));
            // A refused overlay file is not a failed restore - everything else landed,
            // and the file it declined to write is still in the staging tree.
            if done.conflicts() > 0 {
                Exit::Warning
            } else {
                Exit::Ok
            }
        }
        Err(error) => {
            eprintln!("tycho: {error}");
            Exit::Failure
        }
    }
}

/// `--store` reads **no config file at all**, which is the whole disaster case: on a
/// replacement machine there is no config, no state file and no local store.
fn open_source(args: &crate::cli::RestoreArgs) -> Option<Store> {
    let path = match args.source() {
        crate::cli::Source::Store(path) => match std::path::absolute(&path) {
            Ok(path) => crate::primitives::path::AbsPath::from_absolute(&path),
            Err(error) => {
                eprintln!("tycho: {}: {error}", path.display());
                return None;
            }
        },
        crate::cli::Source::Profile(wanted) => {
            let (parsed, _) = load(args.config.clone())?;
            let profile = select(&parsed.config, wanted.as_deref())?;
            let paths = locations(profile)?;
            Ok(paths.store)
        }
    };
    let path = match path {
        Ok(path) => path,
        Err(error) => {
            eprintln!("tycho: {error}");
            return None;
        }
    };

    match Store::open_to_read(&path) {
        Ok(store) => Some(store),
        Err(error) => {
            eprintln!("tycho: {error}");
            None
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
            Schedule::Daily { at } => format!("daily {at}"),
            Schedule::Weekly { day, at } => format!("{day:?} {at}").to_lowercase(),
            Schedule::Every(every) => format!("every {}s", every.as_secs()),
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

pub(crate) fn load(override_path: Option<PathBuf>) -> Option<(Parsed, String)> {
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

/// Which profiles a command was aimed at.
///
/// `--all` is the manual and scripted form, and the shared catch-up agent's own form;
/// the installed backup agents run one profile each, per `scheduling.md`. Naming a
/// profile and passing `--all` cannot both happen - clap refuses the combination -
/// which is why this takes a bool rather than an `Option` that means two things.
fn selected<'a>(config: &'a Config, wanted: Option<&str>, all: bool) -> Option<Vec<&'a Profile>> {
    if all {
        if config.profiles.is_empty() {
            eprintln!("tycho: the config defines no profiles");
            return None;
        }
        return Some(config.profiles.iter().collect());
    }
    select(config, wanted).map(|profile| vec![profile])
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
