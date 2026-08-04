//! `tycho profile`: list, add and remove profiles in the config file.

use crate::cli::remote::{check_ownership, worse};
use crate::cli::report::{at_profile, report};
use crate::cli::{Exit, ProfileAction, ProfileAddArgs, ProfileArgs, ProfileRmArgs, render};
use crate::config::Schedule;
use crate::config_edit::{Editing, NewProfile, NewRemote};
use crate::primitives::names::{ProfileName, RemoteName};
use std::path::{Path, PathBuf};

pub fn dispatch(args: &ProfileArgs) -> Exit {
    match &args.action {
        ProfileAction::List => list(args.config.clone()),
        ProfileAction::Add(add) => add_profile(args.config.clone(), add),
        ProfileAction::Rm(rm) => remove_profile(args.config.clone(), rm),
    }
}

fn list(config: Option<PathBuf>) -> Exit {
    let editing = match open_editing(config) {
        Ok(editing) => editing,
        Err(exit) => return exit,
    };
    let names = editing.profiles();
    if names.is_empty() {
        println!("no profiles");
        return Exit::Ok;
    }
    println!("{:<16}{:<14}{:<14}schedule", "name", "watch", "remotes");
    println!("{}", "-".repeat(render::WIDTH));
    for (index, name) in names.iter().enumerate() {
        let watch = editing
            .entries(index, crate::config_edit::List::Watch)
            .len();
        let remotes = editing.remotes(index).len();
        let remotes_label = if remotes == 0 {
            "local only".to_owned()
        } else {
            render::plural(remotes, "remote")
        };
        let schedule = if editing.has_schedule(index) {
            "set"
        } else {
            "none"
        };
        println!(
            "  {:<14}{:<14}{:<14}{schedule}",
            render::clip(name, 13),
            render::plural(watch, "root"),
            remotes_label
        );
    }
    Exit::Ok
}

/// One `--remote NAME=PATH` after splitting, before the ownership check that
/// decides whether it may be written.
struct RemoteSpec {
    name: RemoteName,
    raw_name: String,
    path: String,
}

fn add_profile(config: Option<PathBuf>, args: &ProfileAddArgs) -> Exit {
    let raw_name = &args.name;
    let name = match ProfileName::parse(raw_name) {
        Ok(name) => name,
        Err(reason) => {
            return report! {
                error: "'{raw_name}' is not a valid profile name",
                note: "{reason}",
            };
        }
    };

    let has_remote = !args.remote.is_empty();
    if has_remote == args.local_only {
        let detail = if has_remote {
            "both --remote and --local-only were given"
        } else {
            "neither --remote nor --local-only was given"
        };
        return report! {
            error: "profile add needs exactly one of --remote or --local-only",
            note: "{detail}",
            help: "a profile with neither fails `config check`: it would have nowhere to \
                   send its backups and nothing marking that as deliberate",
            recovery: {
                "tycho profile add {raw_name} --remote drive=/Volumes/Drive/Backups",
                "tycho profile add {raw_name} --local-only",
            },
        };
    }

    let mut remotes: Vec<RemoteSpec> = Vec::new();
    for raw in &args.remote {
        let Some((remote_raw_name, path)) = raw.split_once('=') else {
            return report! {
                error: "'{raw}' is not NAME=PATH",
                help: "example: --remote drive=/Volumes/Drive/Backups",
            };
        };
        let remote_name = match RemoteName::parse(remote_raw_name) {
            Ok(name) => name,
            Err(reason) => {
                return report! {
                    error: "'{remote_raw_name}' is not a valid remote name",
                    note: "{reason}",
                };
            }
        };
        if remotes.iter().any(|existing| existing.name == remote_name) {
            let dup = remote_raw_name;
            return report! { error: "'{dup}' is given more than once in --remote" };
        }
        remotes.push(RemoteSpec {
            name: remote_name,
            raw_name: remote_raw_name.to_owned(),
            path: path.to_owned(),
        });
    }

    for wanted in args.optional.iter().chain(&args.trust_ownership) {
        if !remotes.iter().any(|remote| remote.raw_name == *wanted) {
            let bad = wanted;
            return report! {
                error: "'{bad}' does not match any --remote NAME=PATH given",
                help: "the name before '=' in --remote is what --optional and \
                       --trust-ownership refer to",
            };
        }
    }

    let schedule = match &args.schedule {
        Some(spec) => match spec.parse::<Schedule>() {
            Ok(schedule) => Some(schedule),
            Err(reason) => {
                return report! {
                    error: "'{spec}' is not a schedule",
                    note: "{reason}",
                    help: "accepted forms: daily:HH:MM, weekly:<weekday>:HH:MM, every:<N>h, \
                           every:<N>m",
                };
            }
        },
        None => None,
    };

    let mut editing = match open_editing(config) {
        Ok(editing) => editing,
        Err(exit) => return exit,
    };
    if editing
        .profiles()
        .iter()
        .any(|existing| existing == raw_name)
    {
        return report! {
            error: "'{raw_name}' already exists",
            at: at_profile(raw_name),
            recovery: { "tycho profile list" => "names every profile in this file" },
        };
    }

    let mut floor = Exit::Ok;
    for remote in &remotes {
        let trust = args.trust_ownership.contains(&remote.raw_name);
        match check_ownership(&remote.raw_name, Path::new(&remote.path), trust) {
            Ok(exit) => floor = worse(floor, exit),
            Err(exit) => return exit,
        }
    }

    let new_remotes: Vec<NewRemote> = remotes
        .iter()
        .map(|remote| NewRemote {
            name: remote.name.as_str().to_owned(),
            path: remote.path.clone(),
            optional: args.optional.contains(&remote.raw_name),
            trust_ownership: args.trust_ownership.contains(&remote.raw_name),
            behind_tolerance: None,
        })
        .collect();
    let new_profile = NewProfile {
        name: name.as_str().to_owned(),
        watch: args.watch.clone(),
        remotes: new_remotes,
        schedule,
        local_only: args.local_only,
    };

    if args.dry_run {
        print!("{}", crate::config_edit::preview_profile(&new_profile));
        return floor;
    }

    if let Err(error) = editing.add_profile(&new_profile) {
        return report! { error: "{error}" };
    }
    finish(&editing, "added", raw_name, floor)
}

fn remove_profile(config: Option<PathBuf>, rm: &ProfileRmArgs) -> Exit {
    let mut editing = match open_editing(config) {
        Ok(editing) => editing,
        Err(exit) => return exit,
    };
    let raw_name = &rm.name;
    let index = match editing.which(Some(raw_name)) {
        Ok(index) => index,
        Err(error) => {
            return report! {
                error: "{error}",
                recovery: { "tycho profile list" => "names every profile in this file" },
            };
        }
    };

    if editing.profiles().len() <= 1 {
        return report! {
            error: "'{raw_name}' is the only profile in this file",
            at: at_profile(raw_name),
            note: "a config with no profiles fails `config check`, and this tool refuses to \
                   leave one behind",
            recovery: {
                "tycho profile add <name> --local-only" => "add a replacement first, if you want one",
            },
        };
    }

    let store = crate::platform::store_path(raw_name, None);
    let existing_store = match &store {
        Ok(path) if path.as_path().exists() => Some(path.clone()),
        _ => None,
    };
    if let Some(path) = &existing_store {
        if !rm.keep_store && !rm.delete_store {
            let shown = path.to_string();
            return report! {
                error: "'{raw_name}' has a local store at {shown}",
                at: at_profile(raw_name),
                note: "removing the profile never touches a remote's own copy - every remote \
                       keeps its own '{raw_name}.git', only this machine's local store is in \
                       question",
                recovery: {
                    "tycho profile rm {raw_name} --keep-store" => "leave the local store on disk",
                    "tycho profile rm {raw_name} --delete-store" => "delete it, permanently",
                },
            };
        }
        if rm.delete_store {
            if let Err(error) = std::fs::remove_dir_all(path.as_path()) {
                let shown = path;
                return report! { error: "removing the store at {shown}: {error}" };
            }
            println!("deleted        {path}");
        }
    }

    if let Err(error) = editing.remove_profile(index) {
        return report! { error: "{error}" };
    }
    finish(&editing, "removed", raw_name, Exit::Ok)
}

fn finish(editing: &Editing, verb: &str, value: &str, floor: Exit) -> Exit {
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
        return floor;
    }
    eprint!("\n{}", render::config_check(&[], &parsed.diagnostics));
    worse(
        floor,
        if parsed.has_errors() {
            Exit::Failure
        } else {
            Exit::Warning
        },
    )
}

fn open_editing(config: Option<PathBuf>) -> Result<Editing, Exit> {
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
    Editing::open(&path).map_err(|error| report! { error: "{error}" })
}

#[cfg(test)]
mod tests {
    use super::{add_profile, remove_profile};
    use crate::cli::{Exit, ProfileAddArgs, ProfileRmArgs};

    fn add_args(name: &str) -> ProfileAddArgs {
        ProfileAddArgs {
            name: name.to_owned(),
            watch: Vec::new(),
            remote: Vec::new(),
            optional: Vec::new(),
            trust_ownership: Vec::new(),
            schedule: None,
            local_only: false,
            dry_run: true,
        }
    }

    /// Both validation checks below run before any file is touched, so they need no
    /// config file at all - `config: None` is never reached.
    #[test]
    fn add_needs_exactly_one_of_remote_or_local_only() {
        let mut neither = add_args("demo");
        assert_eq!(add_profile(None, &neither), Exit::Failure);

        neither.local_only = true;
        neither.remote = vec!["drive=/Volumes/Drive".to_owned()];
        assert_eq!(add_profile(None, &neither), Exit::Failure, "both given");
    }

    /// Pointed at a temp config, never at the machine's own.
    ///
    /// The second half needs to get *past* validation, so it opens a config file -
    /// and `None` resolves to the user's real one, which exists on a developer's
    /// machine and not on a clean runner. That is a test asserting the environment.
    #[test]
    fn an_optional_name_must_match_a_declared_remote() {
        let (dir, path) = config_with_one_profile();
        let mut args = add_args("fresh");
        args.remote = vec![format!("drive={}", dir.path().join("nowhere").display())];
        args.optional = vec!["ghost".to_owned()];
        assert_eq!(add_profile(Some(path.clone()), &args), Exit::Failure);

        args.optional = vec!["drive".to_owned()];
        // The remote path does not exist, so ownership cannot be checked: that is the
        // warning, not a refusal.
        assert_ne!(add_profile(Some(path), &args), Exit::Failure);
    }

    #[test]
    fn a_trust_ownership_name_must_match_a_declared_remote_too() {
        let mut args = add_args("demo");
        args.remote = vec!["drive=/Volumes/Drive".to_owned()];
        args.trust_ownership = vec!["nope".to_owned()];
        assert_eq!(add_profile(None, &args), Exit::Failure);
    }

    /// A real directory on this machine, quoted as a TOML **literal** string.
    ///
    /// Two things a hardcoded `/tmp` hid: it is not a path on Windows, so the
    /// re-validation every write runs called it an error; and the watched root's own
    /// name becomes a git refname, which `tempfile`'s `.tmpXXXX` cannot be. Hence a
    /// named subdirectory, and literal quoting so a `C:\...` path needs no escaping.
    fn config_with_one_profile() -> (tempfile::TempDir, std::path::PathBuf) {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("tycho.toml");
        std::fs::write(&path, profile_toml("demo", &watched(&dir))).expect("write");
        (dir, path)
    }

    fn watched(dir: &tempfile::TempDir) -> std::path::PathBuf {
        let root = dir.path().join("watched");
        std::fs::create_dir_all(&root).expect("mkdir");
        root
    }

    fn profile_toml(name: &str, root: &std::path::Path) -> String {
        format!(
            "version = 1\n\n[[profile]]\nname = \"{name}\"\nwatch = ['{}']\n\
             local_only = true\nschedule = {{ every = \"6h\" }}\n",
            root.display()
        )
    }

    /// Requirement 4(a): the last profile in the file can never be removed, since a
    /// config with none fails `config check`.
    #[test]
    fn removing_the_last_profile_is_refused() {
        let (_dir, path) = config_with_one_profile();
        let outcome = remove_profile(
            Some(path),
            &ProfileRmArgs {
                name: "demo".to_owned(),
                keep_store: false,
                delete_store: false,
            },
        );
        assert_eq!(outcome, Exit::Failure);
    }

    #[test]
    fn removing_one_of_several_profiles_succeeds() {
        let (dir, path) = config_with_one_profile();
        let mut text = std::fs::read_to_string(&path).expect("read");
        let second = profile_toml("second", &watched(&dir));
        text.push_str(second.split_once("\n\n").expect("a profile table").1);
        std::fs::write(&path, text).expect("write");

        let outcome = remove_profile(
            Some(path.clone()),
            &ProfileRmArgs {
                name: "demo".to_owned(),
                keep_store: false,
                delete_store: false,
            },
        );
        assert_eq!(outcome, Exit::Ok);
        let text = std::fs::read_to_string(&path).expect("read back");
        assert!(!text.contains("name = \"demo\""), "{text}");
        assert!(text.contains("name = \"second\""), "{text}");
    }
}
