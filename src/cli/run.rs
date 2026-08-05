//! What `run` and `config` actually do.

use crate::capture::{Inspection, inspect};
use crate::cli::progress::Progress;
use crate::cli::report::{at_file, at_profile, at_remote, at_store, report};
use crate::cli::{ConfigAction, ConfigArgs, Exit, RunArgs, render};
use crate::config::{Config, Parsed, Profile, Schedule};
use crate::plan::Plan;
use crate::plan::PlanError;
use crate::platform;
use crate::remote::state::RemoteState;
use crate::restore::{self, resolve};
use crate::state::State;
use crate::store::run::RunError;
use crate::store::{self, Store};
use std::fs;
use std::path::{Path, PathBuf};

pub fn run(args: &RunArgs) -> Exit {
    let Some((parsed, config_text)) = load(args.config.clone()) else {
        return Exit::Failure;
    };
    if parsed.has_errors() {
        eprint!("{}", render::config_check(&[], &parsed.diagnostics));
        return report! {
            error: "the config has errors, so nothing was backed up",
            recovery: { "tycho config check" => "shows every error and warning" },
        };
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
        Err(error) => return report! { error: "{error}", at: at_file(paths.state.as_path()) },
    };

    if args.dry_run {
        let previous = state.last_entries(profile.name.as_str());
        let plan = match store::run::dry(profile, &previous, args.allow_shrink) {
            Ok(plan) => plan,
            Err(error) => return run_error(&error, profile),
        };
        let repos = if args.quick {
            Vec::new()
        } else {
            inspect_all(&plan)
        };
        print!("{}", render::dry_run(&plan, &repos, args.quick));
        for warning in plan.warnings() {
            let _ = report! { warning: "{warning}" };
        }
        return Exit::Ok;
    }

    let store = match Store::open_or_init(&paths.store) {
        Ok(store) => store,
        Err(error) => return report! { error: "{error}", at: at_store(paths.store.as_path()) },
    };
    let run_paths = store::run::Paths {
        lock: paths.lock.as_path(),
        state: paths.state.as_path(),
        config_text: Some(config_text),
    };
    let mut progress = Progress::new();
    let name = profile.name.as_str();
    let done = match store::run::execute(
        profile,
        &store,
        &run_paths,
        &mut state,
        args.allow_shrink,
        &mut |step| progress.report(name, &step),
    ) {
        Ok(done) => {
            progress.clear();
            done
        }
        Err(error) => {
            progress.clear();
            return run_error(&error, profile);
        }
    };

    print!("{}", render::run_result(profile.name.as_str(), &done));
    for warning in &done.warnings {
        let _ = report! { warning: "{warning}" };
    }

    // A quiet green run over an incomplete backup is the exact failure this project
    // exists to correct, so anything the run did not manage to take is said out loud.
    let mut incomplete = Exit::Ok;
    let missed = done
        .summary
        .repos_found
        .saturating_sub(done.summary.repos_captured);
    if missed > 0 {
        let found = done.summary.repos_found;
        let _ = report! { warning: "{missed} of {found} repositories were not captured" };
        incomplete = Exit::Warning;
    }
    let outcome = worse(report(&done.remotes), incomplete);
    announce(profile, outcome, &done.remotes);
    outcome
}

/// A run's failure, structured. `RootShrank` and `InProgress` are worth a specific
/// recovery; everything else already says what happened in its own message.
fn run_error(error: &RunError, profile: &Profile) -> Exit {
    let name = profile.name.as_str();
    match error {
        RunError::InProgress(_) => report! {
            error: "{error}",
            at: at_profile(name),
            note: "another `tycho run` for this profile, or its scheduled agent, holds the lock",
        },
        RunError::Plan(PlanError::RootShrank { .. }) => report! {
            error: "{error}",
            at: at_profile(name),
            recovery: {
                "tycho run --profile {name} --allow-shrink" => "back it up anyway",
            },
        },
        _ => report! { error: "{error}", at: at_profile(name) },
    }
}

/// Failure is loud on four channels, and this is the third of them: the exit code and
/// the state-file record carry the contract, `status` carries the red line, and a
/// banner carries the interruption.
///
/// A notification that could not be delivered is **never** a failed run. A machine in
/// a Focus mode suppresses delivery, which is expected - which is exactly why the
/// notification is the convenience and the other three are the contract.
fn announce(profile: &Profile, outcome: Exit, remotes: &[store::run::RemoteResult]) {
    use crate::platform::notify::{Urgency, notify};

    let failed: Vec<&str> = remotes
        .iter()
        .filter(|remote| remote.state.is_red())
        .map(|remote| remote.name.as_str())
        .collect();

    let (urgency, body) = match outcome {
        Exit::Ok => return,
        Exit::Failure if failed.is_empty() => (
            Urgency::Failure,
            format!("{} did not complete", profile.name),
        ),
        Exit::Failure => (
            Urgency::Failure,
            format!("{} could not reach {}", profile.name, failed.join(", ")),
        ),
        Exit::Warning => (
            Urgency::Warning,
            format!("{} finished with warnings", profile.name),
        ),
    };
    let _ = notify(urgency, &body);
}

/// The check that makes the no-daemon design safe rather than merely cheap.
///
/// `man launchd.plist` promises catch-up across sleep and says nothing about
/// power-off, so a Mac shut down over a weekend would silently skip its weekly backup
/// and nothing would ever notice. Every invocation of every agent runs this.
fn overdue(profile: &Profile, state: &State) -> Option<std::time::Duration> {
    let schedule = profile.schedule?;
    let last = state
        .profiles
        .get(profile.name.as_str())
        .into_iter()
        .flatten()
        .find(|run| run.outcome != crate::state::Outcome::Failed)
        .and_then(|run| run.when.parse::<jiff::Timestamp>().ok())
        .map(|stamp| stamp.to_zoned(jiff::tz::TimeZone::system()));
    let now = jiff::Timestamp::now().to_zoned(jiff::tz::TimeZone::system());
    schedule.overdue_by(last.as_ref(), &now)
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
                let _ = report! { error: "{error}", at: at_file(dir.as_path()) };
                return None;
            }
            let lock = crate::primitives::path::AbsPath::from_absolute(
                &dir.as_path().join(format!("{}.lock", profile.name)),
            );
            match lock {
                Ok(lock) => Some(Locations { store, state, lock }),
                Err(error) => {
                    let _ = report! { error: "{error}" };
                    None
                }
            }
        }
        (Err(error), ..) | (_, Err(error), _) | (_, _, Err(error)) => {
            let _ = report! { error: "{error}" };
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
        Err(error) => return report! { error: "{error}", at: at_store(paths.store.as_path()) },
    };
    if let Some(path) = &args.path {
        return path_history(&store, path, args.count);
    }
    match store.history(args.count) {
        Ok(backups) => {
            print!("{}", render::history(&backups));
            Exit::Ok
        }
        Err(error) => report! { error: "{error}", at: at_store(paths.store.as_path()) },
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
        Err(error) => return report! { error: "{error}", at: at_file(path) },
    };
    match resolve::history(store, &resolved, count) {
        Ok(commits) => {
            print!("{}", render::path_history(path, &resolved, &commits));
            Exit::Ok
        }
        Err(error) => report! { error: "{error}", at: at_file(path) },
    }
}

/// The newest backup, read once.
fn newest(store: &Store) -> Option<resolve::Backup> {
    let commit = match store.at(None) {
        Ok(Some(commit)) => commit,
        Ok(None) => {
            let _ = report! {
                error: "this store holds no backups yet",
                recovery: { "tycho run" => "creates the first one" },
            };
            return None;
        }
        Err(error) => {
            let _ = report! { error: "{error}" };
            return None;
        }
    };
    match resolve::Backup::read(store, commit) {
        Ok(backup) => Some(backup),
        Err(error) => {
            let _ = report! { error: "{error}" };
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
        let name = profile.name.as_str();
        let _ = report! {
            warning: "{name} has no remotes to push to",
            recovery: { "tycho remote add <name> <path> -p {name}" => "gives it somewhere to push" },
        };
        return Exit::Ok;
    }
    let Some(paths) = locations(profile) else {
        return Exit::Failure;
    };
    let mut state = match State::load(paths.state.as_path()) {
        Ok(state) => state,
        Err(error) => return report! { error: "{error}", at: at_file(paths.state.as_path()) },
    };
    let store = match Store::open_or_init(&paths.store) {
        Ok(store) => store,
        Err(error) => return report! { error: "{error}", at: at_store(paths.store.as_path()) },
    };
    let run_paths = store::run::Paths {
        lock: paths.lock.as_path(),
        state: paths.state.as_path(),
        config_text: Some(config_text),
    };

    let mut progress = Progress::new();
    let name = profile.name.as_str();
    let outcome = store::run::catch_up(profile, &store, &run_paths, &mut state, &mut |step| {
        progress.report(name, &step)
    });
    progress.clear();
    match outcome {
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
        Err(error) => run_error(&error, profile),
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
        let name = result.name.as_str();
        match &result.state {
            RemoteState::Failed { reason, .. } => {
                let _ = report! { error: "{reason}", at: at_remote(name) };
                exit = Exit::Failure;
            }
            RemoteState::Behind { runs, .. } => {
                let tolerance = result.tolerance;
                let _ = report! {
                    warning: "behind {runs} of {tolerance}",
                    at: at_remote(name),
                };
            }
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
            let _ = report! {
                warning: "{error}",
                note: "showing status with no run history because the state file could not be read",
            };
            State::default()
        }
        Err(error) => {
            let _ = report! { warning: "{error}" };
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
        let Some(name) = wanted else {
            return report! { error: "the config defines no profiles" };
        };
        return report! {
            error: "no profile named '{name}'",
            at: at_profile(name),
            recovery: { "tycho profile list" => "names every profile in this file" },
        };
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
        // Overdue is red on its own, whatever the remotes say: a backup that did not
        // happen is worse than one that happened and could not be pushed.
        red |= overdue(profile, &state).is_some();
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
        overdue: overdue(profile, state),
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
        severity: if current.is_red() {
            crate::doctor::Verdict::Fail
        } else if current.is_yellow() {
            crate::doctor::Verdict::Warn
        } else {
            crate::doctor::Verdict::Ok
        },
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
                Err(error) => return report! { error: "{error}" },
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
            return report! {
                error: "this store holds no backups yet",
                recovery: { "tycho run" => "creates the first one" },
            };
        }
        Err(error) => return report! { error: "{error}" },
    }

    let wanted = restore::Wanted {
        paths: args.paths.clone(),
        bundle: args.bundle,
    };
    match restore::execute(&store, &args.into, at.as_ref(), &wanted, args.force) {
        Ok(done) => {
            print!("{}", render::restored(&args.into, &done));
            if !done.missing.is_empty() {
                // A path the backup holds and the disk does not is a restore that did
                // not happen, whatever the extraction's own exit status said. Exit 1
                // rather than 3: `cli.md` reserves 3 for a refused overlay file, which
                // is still in the staging tree, and this is not.
                Exit::Failure
            } else if done.conflicts() > 0 {
                // A refused overlay file is not a failed restore - everything else
                // landed, and the file it declined to write is still in the staging
                // tree.
                Exit::Warning
            } else {
                Exit::Ok
            }
        }
        Err(error) => report! { error: "{error}" },
    }
}

/// `--store` reads **no config file at all**, which is the whole disaster case: on a
/// replacement machine there is no config, no state file and no local store.
fn open_source(args: &crate::cli::RestoreArgs) -> Option<Store> {
    let path = match args.source() {
        crate::cli::Source::Store(path) => match std::path::absolute(&path) {
            Ok(path) => crate::primitives::path::AbsPath::from_absolute(&path),
            Err(error) => {
                let _ = report! { error: "{error}", at: at_file(&path) };
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
            let _ = report! { error: "{error}" };
            return None;
        }
    };

    match Store::open_to_read(&path) {
        Ok(store) => Some(store),
        Err(error) => {
            let _ = report! { error: "{error}", at: at_store(path.as_path()) };
            None
        }
    }
}

pub fn config(args: &ConfigArgs) -> Exit {
    let path = match config_location(args.config.clone()) {
        Ok(path) => path,
        Err(error) => return report! { error: "{error}" },
    };

    if matches!(args.action, ConfigAction::Path) {
        println!("{}", path.display());
        return Exit::Ok;
    }
    if let ConfigAction::Init { force } = args.action {
        return init(&path, force);
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

/// `config init`: a starter file, refusing to overwrite one that is already there.
fn init(path: &Path, force: bool) -> Exit {
    if path.exists() {
        if !force {
            return report! {
                error: "a config file already exists",
                at: at_file(path),
                note: "`--force` overwrites it after writing a `.bak` of the original",
                recovery: { "tycho config init --force" },
            };
        }
        // A config is a file somebody wrote by hand, so --force keeps a copy rather
        // than trusting that they meant it.
        let backup = path.with_extension("toml.bak");
        if let Err(error) = std::fs::copy(path, &backup) {
            return report! { error: "{error}", at: at_file(&backup) };
        }
        println!("kept           {}", backup.display());
    }

    if let Some(parent) = path.parent()
        && let Err(error) = std::fs::create_dir_all(parent)
    {
        return report! { error: "{error}", at: at_file(parent) };
    }

    let text = crate::config_edit::starter();
    if let Err(error) = crate::sys::fs::write_atomic(path, text.as_bytes()) {
        return report! { error: "{error}", at: at_file(path) };
    }

    println!("wrote          {}", path.display());
    println!("\nedit it, then `tycho config check`");
    Exit::Ok
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

pub(crate) fn config_location(override_path: Option<PathBuf>) -> Result<PathBuf, String> {
    match override_path {
        Some(path) => Ok(path),
        None => platform::config_path()
            .map(|path| path.as_path().to_path_buf())
            .map_err(|error| error.to_string()),
    }
}

pub(crate) fn load(override_path: Option<PathBuf>) -> Option<(Parsed, String)> {
    let path = match config_location(override_path) {
        Ok(path) => path,
        Err(error) => {
            let _ = report! { error: "{error}" };
            return None;
        }
    };
    let text = match fs::read_to_string(&path) {
        Ok(text) => text,
        // The first thing anyone hits after installing, and a bare `os error 2` names
        // neither the file's purpose nor the one command that creates it.
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let shown = path.display();
            let _ = report! {
                error: "no config file at {shown}",
                at: at_file(&path),
                note: "a config file lists what to watch and where to send it; every \
                       command but `config init` needs one",
                recovery: {
                    "tycho config init" => "writes a starter file, with a comment on each key",
                    "tycho config path" => "says where it would go",
                },
            };
            return None;
        }
        Err(error) => {
            let _ = report! { error: "{error}", at: at_file(&path) };
            return None;
        }
    };
    match crate::config::parse(&text) {
        Ok(parsed) => {
            // The composition root for `safe.directory`, and the only place that can
            // be: it is process-wide and takes one file, so it has to see every
            // profile at once. Here rather than on the push path because `status`,
            // `doctor` and `verify` all reach a remote too, and installing it only
            // where the push happens meant a trusted remote worked under `run` and
            // read as broken under every command that reports on it.
            if let Ok(dir) = platform::data_dir() {
                let _ = crate::remote::trust::install(&parsed.config, dir.as_path());
            }
            Some((parsed, text))
        }
        Err(error) => {
            let _ = report! { error: "{error}", at: at_file(&path) };
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
            let _ = report! {
                error: "the config defines no profiles",
                recovery: { "tycho profile add <name> --local-only" => "or with --remote NAME=PATH" },
            };
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
                let _ = report! {
                    error: "no profile named '{name}'",
                    at: at_profile(name),
                    recovery: { "tycho profile list" => "names every profile in this file" },
                };
            }
            found
        }
        None => match config.profiles.as_slice() {
            [only] => Some(only),
            [] => {
                let _ = report! {
                    error: "the config defines no profiles",
                    recovery: { "tycho profile add <name> --local-only" => "or with --remote NAME=PATH" },
                };
                None
            }
            many => {
                let names = many
                    .iter()
                    .map(|p| p.name.as_str())
                    .collect::<Vec<_>>()
                    .join(", ");
                let _ = report! {
                    error: "more than one profile matches; say which one",
                    note: "configured profiles: {names}",
                    help: "pass `--profile <name>` to say which one",
                };
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
                Err(error) => {
                    let key = &repo.key;
                    let _ = report! { warning: "{key}: {error}" };
                }
            }
        }
    }
    out
}

/// `tycho log`: tail the log file without needing to know where it lives.
pub fn log(args: &crate::cli::LogArgs) -> Exit {
    let Some((parsed, _)) = load(args.config.clone()) else {
        return Exit::Failure;
    };
    let Some(profile) = select(&parsed.config, args.profile.as_deref()) else {
        return Exit::Failure;
    };
    let path = match crate::platform::log::path_for(profile.name.as_str()) {
        Ok(path) => path,
        Err(error) => return report! { error: "{error}" },
    };
    if !path.exists() {
        let name = profile.name.as_str();
        let _ = report! { warning: "nothing logged yet for {name}", at: at_file(&path) };
        return Exit::Ok;
    }

    // `tail` rather than a reimplementation: `-f` means following across a rotation
    // and a truncation, and getting that subtly wrong in a diagnostic tool is worse
    // than shelling out to the thing everyone already trusts to do it.
    let count = args.count.to_string();
    let path = path.display().to_string();
    let mut arguments = vec!["-n", &count];
    if args.follow {
        arguments.push("-f");
    }
    arguments.push(&path);

    match crate::sys::process::stream_to_terminal("tail", &arguments) {
        Ok(code) => {
            if code == 0 {
                Exit::Ok
            } else {
                Exit::Failure
            }
        }
        Err(error) => report! { error: "{error}" },
    }
}
