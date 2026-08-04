//! `tycho remote`: list, add and remove a profile's destinations.

use crate::cli::report::{at_profile, at_remote, report};
use crate::cli::{Exit, RemoteAction, RemoteAddArgs, RemoteArgs, RemoteRmArgs, render};
use crate::config_edit::{Editing, NewRemote};
use crate::primitives::names::RemoteName;
use std::path::{Path, PathBuf};

pub fn dispatch(args: &RemoteArgs) -> Exit {
    match &args.action {
        RemoteAction::List => list(args),
        RemoteAction::Add(add) => add_remote(args, add),
        RemoteAction::Rm(rm) => remove_remote(args, rm),
    }
}

fn list(args: &RemoteArgs) -> Exit {
    let (editing, index) = match open(args.config.clone(), args.profile.as_deref()) {
        Ok(pair) => pair,
        Err(exit) => return exit,
    };
    let remotes = editing.remotes(index);
    if remotes.is_empty() {
        println!("no remotes, local only");
        return Exit::Ok;
    }
    println!("{:<12}{:<32}{:<18}tolerance", "name", "path", "flags");
    println!("{}", "-".repeat(render::WIDTH));
    for remote in &remotes {
        let mut flags = Vec::new();
        if remote.optional {
            flags.push("optional");
        }
        if remote.trust_ownership {
            flags.push("trust-ownership");
        }
        let flags = if flags.is_empty() {
            "-".to_owned()
        } else {
            flags.join(",")
        };
        let tolerance = remote
            .behind_tolerance
            .map_or_else(|| "4 (default)".to_owned(), |n| n.to_string());
        println!(
            "  {:<10}{:<32}{:<18}{tolerance}",
            render::clip(&remote.name, 9),
            render::fit(&remote.path, 31),
            flags
        );
    }
    Exit::Ok
}

fn add_remote(args: &RemoteArgs, add: &RemoteAddArgs) -> Exit {
    let (mut editing, index) = match open(args.config.clone(), args.profile.as_deref()) {
        Ok(pair) => pair,
        Err(exit) => return exit,
    };

    let raw_name = &add.name;
    let name = match RemoteName::parse(raw_name) {
        Ok(name) => name,
        Err(reason) => {
            return report! {
                error: "'{raw_name}' is not a valid remote name",
                note: "{reason}",
            };
        }
    };
    if editing
        .remotes(index)
        .iter()
        .any(|existing| existing.name == name.as_str())
    {
        return report! {
            error: "'{raw_name}' is already a remote on this profile",
            at: at_remote(raw_name),
            recovery: {
                "tycho remote rm {raw_name}" => "remove it first, if you meant to replace it",
            },
        };
    }

    let path = Path::new(&add.path);
    let floor = match check_ownership(raw_name, path, add.trust_ownership) {
        Ok(exit) => exit,
        Err(exit) => return exit,
    };

    let new = NewRemote {
        name: name.as_str().to_owned(),
        path: add.path.clone(),
        optional: add.optional,
        trust_ownership: add.trust_ownership,
        behind_tolerance: add.behind_tolerance,
    };
    if let Err(error) = editing.add_remote(index, &new) {
        return report! { error: "{error}" };
    }
    finish(&editing, "added", raw_name, floor)
}

fn remove_remote(args: &RemoteArgs, rm: &RemoteRmArgs) -> Exit {
    let (mut editing, index) = match open(args.config.clone(), args.profile.as_deref()) {
        Ok(pair) => pair,
        Err(exit) => return exit,
    };
    let raw_name = &rm.name;
    let remotes = editing.remotes(index);
    let profile_name = editing.profiles()[index].clone();

    if !remotes.iter().any(|remote| remote.name == *raw_name) {
        return report! {
            error: "'{raw_name}' is not a remote on '{profile_name}'",
            at: at_profile(&profile_name),
            recovery: {
                "tycho remote list -p {profile_name}" => "names every remote on this profile",
            },
        };
    }
    if remotes.len() == 1 && !rm.local_only {
        return report! {
            error: "'{raw_name}' is the only remote on '{profile_name}'",
            at: at_profile(&profile_name),
            note: "removing it would leave this profile's backups on this machine alone",
            recovery: {
                "tycho remote add <name> <path>" => "point it at the replacement first",
                "tycho remote rm {raw_name} --local-only" => "or accept local-only backups",
            },
        };
    }

    if let Err(error) = editing.remove_remote(index, raw_name) {
        return report! { error: "{error}" };
    }
    if rm.local_only
        && remotes.len() == 1
        && let Err(error) = editing.set_local_only(index, true)
    {
        return report! { error: "{error}" };
    }
    finish(&editing, "removed", raw_name, Exit::Ok)
}

/// Fails fast, at add time: refuses a remote whose volume git will refuse to trust,
/// before anything is written.
///
/// # Errors
///
/// The [`Exit`] to propagate immediately - nothing is written - when the volume
/// records no ownership and `trust_ownership` was not given.
pub(crate) fn check_ownership(
    name: &str,
    path: &Path,
    trust_ownership: bool,
) -> Result<Exit, Exit> {
    // Saying so was the decision. There is nothing left to verify, and warning anyway
    // told people to pass the flag they had just passed.
    if trust_ownership {
        return Ok(Exit::Ok);
    }

    // The folder itself will not exist yet on first contact - Tycho creates it on the
    // first run - but ownership is a property of the **volume**, so the nearest
    // ancestor that does exist answers the same question. Only a path with no existing
    // ancestor at all is a drive that is not plugged in.
    let Some(probe) = nearest_existing(path) else {
        let shown = path.display();
        return Ok(report! {
            warning: "'{name}' cannot be checked for ownership yet",
            at: at_remote(name),
            note: "nothing along {shown} is present, so the volume cannot be inspected \
                   - the drive is most likely not plugged in",
            help: "the first backup run will fail if that volume turns out to record none",
        });
    };

    match crate::sys::volume::records_ownership(probe) {
        Ok(true) => Ok(Exit::Ok),
        Ok(false) => {
            let shown = path.display();
            Err(report! {
                error: "'{name}' is on a volume that records no ownership",
                at: at_remote(name),
                note: "git refuses a repository there unless it is told to trust it, and \
                       {shown} is exactly that kind of volume - exFAT, FAT32, or a drive \
                       mounted with ownership ignored",
                recovery: {
                    "tycho remote add {name} {shown} --trust-ownership",
                },
            })
        }
        Err(error) => Err(report! {
            error: "could not check ownership for '{name}': {error}",
            at: at_remote(name),
        }),
    }
}

/// The closest ancestor of `path` that is on disk, which is on the same volume.
fn nearest_existing(path: &Path) -> Option<&Path> {
    std::iter::successors(Some(path), |here| here.parent()).find(|here| here.exists())
}

/// Writes the file, then re-validates it, folding in whatever [`Exit`] the caller had
/// already earned - the ownership check's warning must not be lost under a clean
/// re-validation.
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
    eprint!(
        "\n{}",
        crate::cli::render::config_check(&[], &parsed.diagnostics)
    );
    worse(
        floor,
        if parsed.has_errors() {
            Exit::Failure
        } else {
            Exit::Warning
        },
    )
}

pub(crate) const fn worse(a: Exit, b: Exit) -> Exit {
    match (a, b) {
        (Exit::Failure, _) | (_, Exit::Failure) => Exit::Failure,
        (Exit::Warning, _) | (_, Exit::Warning) => Exit::Warning,
        (Exit::Ok, Exit::Ok) => Exit::Ok,
    }
}

/// Opens the config file and resolves which profile a command means.
fn open(config: Option<PathBuf>, profile: Option<&str>) -> Result<(Editing, usize), Exit> {
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
    let editing = match Editing::open(&path) {
        Ok(editing) => editing,
        Err(error) => return Err(report! { error: "{error}" }),
    };
    match editing.which(profile) {
        Ok(index) => Ok((editing, index)),
        Err(error) => Err(report! {
            error: "{error}",
            recovery: { "tycho profile list" => "names every profile in this file" },
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::{RemoteAddArgs, RemoteArgs, RemoteRmArgs, add_remote, remove_remote};
    use crate::cli::fixture::{self, Fixture};
    use crate::cli::{Exit, RemoteAction};

    /// The remote's path is a real directory, so the ownership check reaches a
    /// filesystem it can actually interrogate rather than the not-present warning.
    fn config_with_one_remote() -> Fixture {
        let fixture = Fixture::new();
        let drive = fixture.dir("drive");
        let profile = fixture.profile(
            "demo",
            &format!(
                "remotes = [\n  {{ name = \"drive\", path = {} }},\n]\n\
                 schedule = {{ every = \"6h\" }}\n",
                fixture::literal(&drive)
            ),
        );
        fixture.append(&profile);
        fixture
    }

    fn args(path: &std::path::Path, action: RemoteAction) -> RemoteArgs {
        RemoteArgs {
            action,
            profile: None,
            config: Some(path.to_path_buf()),
        }
    }

    /// The guard requirement 5 asks for: removing a profile's only remote is refused
    /// unless `--local-only` says the profile should become local-only.
    #[test]
    fn removing_the_last_remote_is_refused_without_local_only() {
        let fixture = config_with_one_remote();
        let path = fixture.path.clone();
        let outcome = remove_remote(
            &args(&path, RemoteAction::List),
            &RemoteRmArgs {
                name: "drive".to_owned(),
                local_only: false,
            },
        );
        assert_eq!(outcome, Exit::Failure);
    }

    #[test]
    fn removing_the_last_remote_with_local_only_marks_the_profile_local_only() {
        let fixture = config_with_one_remote();
        let path = fixture.path.clone();
        let outcome = remove_remote(
            &args(&path, RemoteAction::List),
            &RemoteRmArgs {
                name: "drive".to_owned(),
                local_only: true,
            },
        );
        assert_eq!(outcome, Exit::Ok);
        let text = std::fs::read_to_string(&path).expect("read back");
        assert!(text.contains("local_only = true"), "{text}");
        assert!(!text.contains("name = \"drive\""), "{text}");
    }

    #[test]
    fn removing_a_remote_that_is_not_the_last_one_needs_no_flag() {
        let fixture = config_with_one_remote();
        let path = fixture.path.clone();
        // A second remote first, so removing "drive" is no longer the last-remote case.
        let added = add_remote(
            &args(&path, RemoteAction::List),
            &RemoteAddArgs {
                name: "t7".to_owned(),
                path: fixture.dir("t7").display().to_string(),
                optional: true,
                trust_ownership: false,
                behind_tolerance: None,
            },
        );
        assert_ne!(added, Exit::Failure);

        let outcome = remove_remote(
            &args(&path, RemoteAction::List),
            &RemoteRmArgs {
                name: "drive".to_owned(),
                local_only: false,
            },
        );
        assert_eq!(outcome, Exit::Ok);
    }

    #[test]
    fn removing_an_unconfigured_remote_is_refused() {
        let fixture = config_with_one_remote();
        let path = fixture.path.clone();
        let outcome = remove_remote(
            &args(&path, RemoteAction::List),
            &RemoteRmArgs {
                name: "ghost".to_owned(),
                local_only: false,
            },
        );
        assert_eq!(outcome, Exit::Failure);
    }
}
