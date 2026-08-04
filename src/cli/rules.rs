//! `tycho watch`, `tycho ignore` and `tycho reinclude`: the same three verbs over
//! three lists, so one implementation serves all of them.

use crate::cli::report::{at_file, report};
use crate::cli::{Exit, RuleAction, RuleArgs};
use crate::config_edit::{Editing, List};

pub fn dispatch(list: List, args: &RuleArgs) -> Exit {
    let path = match crate::cli::run::config_location(args.config.clone()) {
        Ok(path) => path,
        Err(error) => return report! { error: "{error}" },
    };
    if !path.exists() {
        // The first thing anyone hits after installing. A bare `os error 2` names
        // neither what the file is for nor the one command that creates it.
        let shown = path.display();
        return report! {
            error: "no config file at {shown}",
            at: at_file(&path),
            note: "a config file lists what to watch and where to send it; every \
                   command but `config init` needs one",
            recovery: {
                "tycho config init" => "writes a starter file, with a comment on each key",
                "tycho config path" => "says where it would go",
            },
        };
    }
    let mut editing = match Editing::open(&path) {
        Ok(editing) => editing,
        Err(error) => return report! { error: "{error}", at: at_file(&path) },
    };
    let profile = match editing.which(args.profile.as_deref()) {
        Ok(index) => index,
        Err(error) => {
            return report! {
                error: "{error}",
                recovery: { "tycho profile list" => "names every profile in this file" },
            };
        }
    };

    match &args.action {
        RuleAction::List => {
            for entry in editing.entries(profile, list) {
                println!("{entry}");
            }
            Exit::Ok
        }
        RuleAction::Add { value } => {
            // Stored as written, not expanded: `~` and `$HOME` in the file are what
            // makes it portable between machines, and resolving them on the way in
            // would bake this machine's home directory into it.
            match editing.add(profile, list, value) {
                Ok(false) => {
                    println!("already there  {value}");
                    Exit::Ok
                }
                Ok(true) => finish(&editing, "added", value),
                Err(error) => report! { error: "{error}" },
            }
        }
        RuleAction::Rm { value } => match editing.remove(profile, list, value) {
            Ok(()) => finish(&editing, "removed", value),
            Err(error) => report! { error: "{error}" },
        },
    }
}

/// Writes the file, then validates what was written.
///
/// Checking afterwards rather than before is deliberate: the thing worth catching is
/// a rule that is legal TOML and wrong - a watched root that does not exist, a glob
/// that will match nothing - and only the resulting file can be checked for that.
fn finish(editing: &Editing, verb: &str, value: &str) -> Exit {
    if let Err(error) = editing.save() {
        return report! { error: "{error}" };
    }
    println!("{verb:<14} {value}");

    let Ok(parsed) = crate::config::parse(&editing.text()) else {
        return report! {
            error: "the file no longer parses, so the edit was written but cannot be read back",
            note: "the edit itself is on disk; what follows it in the file is what \
                   stopped parsing",
            recovery: { "tycho config path" => "prints the file to open" },
        };
    };
    if parsed.diagnostics.is_empty() {
        return Exit::Ok;
    }
    eprint!(
        "\n{}",
        crate::cli::render::config_check(&[], &parsed.diagnostics)
    );
    if parsed.has_errors() {
        Exit::Failure
    } else {
        Exit::Warning
    }
}
