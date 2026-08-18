//! `tycho watch`, `tycho ignore` and `tycho reinclude`: the same three verbs over
//! three lists, so one implementation serves all of them.

use crate::cli::render::Change;
use crate::cli::report::{at_file, at_file_line, report};
use crate::cli::{Exit, RuleAction, RuleArgs, RulesAction, RulesArgs};
use crate::config::Profile;
use crate::config::rules::{RuleTree, Source, Verdict};
use crate::config::{local, rules};
use crate::config_edit::{Editing, List, LocalEditing};
use crate::primitives::path::AbsPath;
use std::io::IsTerminal;
use std::path::Path;

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
                recovery: { "tycho profile list" => "names every profile this config defines" },
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
        RuleAction::Add { value, local } => {
            if *local {
                return local_edit(args, list, value, Change::Gained);
            }
            // Stored as written, not expanded: `~` and `$HOME` in the file are what
            // makes it portable between machines, and resolving them on the way in
            // would bake this machine's home directory into it.
            match editing.add(profile, list, value) {
                Ok(false) => {
                    println!("already there  {value}");
                    Exit::Ok
                }
                Ok(true) => {
                    if list == List::Watch && !overlap_accepted(&editing, profile, value) {
                        println!("left unchanged  {value}");
                        return Exit::Ok;
                    }
                    finish(&editing, Change::Gained, "added", value)
                }
                Err(error) => report! { error: "{error}" },
            }
        }
        RuleAction::Rm { value, local } => {
            if *local {
                return local_edit(args, list, value, Change::Lost);
            }
            match editing.remove(profile, list, value) {
                Ok(()) => finish(&editing, Change::Lost, "removed", value),
                Err(error) => report! { error: "{error}" },
            }
        }
    }
}

/// Whether an overlap with another profile's root is fine. Overlap across
/// profiles is double coverage into two stores - legitimate, but worth a pause
/// when someone is at the keyboard to answer for it.
fn overlap_accepted(editing: &Editing, profile: usize, value: &str) -> bool {
    let Ok(added) = AbsPath::parse(value) else {
        // Not a path this host can expand; validation says so elsewhere.
        return true;
    };
    let Ok(parsed) = crate::config::parse(&editing.text()) else {
        return true;
    };
    let profiles = &parsed.config.profiles;
    let Some(ours) = profiles.get(profile) else {
        return true;
    };
    for other in profiles.iter().filter(|other| other.name != ours.name) {
        for root in &other.watch {
            let theirs = root.path();
            if theirs.contains(&added) || added.contains(theirs) {
                let name = other.name.as_str();
                eprintln!(
                    "warning: {value} overlaps {theirs}, watched by profile '{name}'\n         \
                     both profiles will capture it, each into its own store"
                );
                if !std::io::stdin().is_terminal() {
                    return true;
                }
                eprint!("add it anyway? [y/N] ");
                let mut answer = String::new();
                if std::io::stdin().read_line(&mut answer).is_err() {
                    return false;
                }
                return matches!(answer.trim().to_lowercase().as_str(), "y" | "yes");
            }
        }
    }
    true
}

/// `--local`: the rule goes in a `.tycho/rules.toml` inside the tree, not in the
/// config. The file sits at the deepest still-captured ancestor of the target -
/// for an ignore that is simply the parent, and for a re-include under an
/// ignored directory it is the nearest ancestor above the ignore, the only
/// placement at which the rule would ever be read.
fn local_edit(args: &RuleArgs, list: List, value: &str, change: Change) -> Exit {
    if list == List::Watch {
        return report! {
            error: "a local file cannot declare `watch`",
            note: "it scopes a watch, it cannot create one",
        };
    }
    let Some((parsed, _)) = crate::cli::run::load(args.config.clone()) else {
        return Exit::Failure;
    };
    let target = match expand(value) {
        Ok(target) => target,
        Err(reason) => return report! { error: "'{value}' is not a path: {reason}" },
    };
    let profile = match owning_profile(&parsed.config.profiles, &target, args.profile.as_deref()) {
        Ok(profile) => profile,
        Err(exit) => return exit,
    };
    let (_tree, anchor) = match load_chain(profile, &target) {
        Ok(loaded) => loaded,
        Err(exit) => return exit,
    };
    let Ok(relative) = target.as_path().strip_prefix(anchor.as_path()) else {
        return report! { error: "{target} is not under {anchor}" };
    };
    let entry: Vec<String> = relative
        .components()
        .map(|component| component.as_os_str().to_string_lossy().into_owned())
        .collect();
    let entry = entry.join("/");
    if entry.is_empty() {
        return report! {
            error: "{target} is the nearest captured directory itself",
            note: "a local rule names something beneath its own directory; to act \
                   on this path, write the rule one level up or in the config",
        };
    }
    let file = anchor
        .as_path()
        .join(local::DIR_NAME)
        .join(local::FILE_NAME);
    let mut editing = match LocalEditing::open_or_start(&file) {
        Ok(editing) => editing,
        Err(error) => return report! { error: "{error}", at: at_file(&file) },
    };
    let (verb, outcome) = match change {
        Change::Gained => ("added", editing.add(list, &entry).map(|changed| !changed)),
        Change::Lost => ("removed", editing.remove(list, &entry).map(|()| false)),
    };
    match outcome {
        Ok(true) => {
            println!("already there  {entry}");
            Exit::Ok
        }
        Ok(false) => {
            if let Err(error) = editing.save() {
                return report! { error: "{error}", at: at_file(&file) };
            }
            println!("{}", crate::cli::render::echo(change, verb, &entry));
            println!("               in {}", file.display());
            Exit::Ok
        }
        Err(error) => report! { error: "{error}", at: at_file(&file) },
    }
}

/// `tycho rules explain PATH`.
pub fn explain_dispatch(args: &RulesArgs) -> Exit {
    let RulesAction::Explain { path } = &args.action;
    let Some((parsed, _)) = crate::cli::run::load(args.config.clone()) else {
        return Exit::Failure;
    };
    let target = match expand(path) {
        Ok(target) => target,
        Err(reason) => return report! { error: "'{path}' is not a path: {reason}" },
    };
    let profile = match owning_profile(&parsed.config.profiles, &target, args.profile.as_deref()) {
        Ok(profile) => profile,
        Err(exit) => return exit,
    };
    let (tree, _) = match load_chain(profile, &target) {
        Ok(loaded) => loaded,
        Err(exit) => return exit,
    };
    let candidates = tree.explain(target.as_path());
    let Some((winner, beaten)) = candidates.split_first() else {
        return report! {
            error: "no rule matches {target}",
            note: "only paths under a watched root are captured, and this one is \
                   under none",
        };
    };
    print_decision(&tree, winner, None);
    for decision in beaten {
        print_decision(&tree, decision, Some("beaten"));
    }
    Exit::Ok
}

fn print_decision(tree: &RuleTree, decision: &crate::config::rules::Decision, label: Option<&str>) {
    let verdict = match decision.verdict {
        Verdict::Capture => "capture",
        Verdict::Skip => "skip",
    };
    let Some(id) = decision.rule else { return };
    match label {
        None => println!("{verdict:<9}{}", tree.rule_text(id)),
        Some(label) => println!("{label:<9}{verdict:<8}{}", tree.rule_text(id)),
    }
    let source = match tree.rule_source(id) {
        Source::Junk => "the built-in junk list".to_owned(),
        Source::Global => "the profile config".to_owned(),
        Source::Local { file, line } => format!("{file}:{line}"),
    };
    let tier = match decision.tier {
        rules::Tier::ExplicitPath => "explicit-path",
        rules::Tier::Glob => "glob",
        rules::Tier::Junk => "junk",
    };
    let origin = match decision.origin {
        rules::Origin::Global => "global".to_owned(),
        rules::Origin::Local(_) => "local".to_owned(),
        rules::Origin::Junk => "junk".to_owned(),
    };
    println!("         {source}");
    println!(
        "         depth {}, tier {tier}, origin {origin}",
        decision.depth
    );
}

/// The target as an absolute path: expanded like a config entry, with a bare
/// relative argument resolved against the working directory.
fn expand(value: &str) -> Result<AbsPath, String> {
    if let Ok(path) = AbsPath::parse(value) {
        return Ok(path);
    }
    let cwd = std::env::current_dir().map_err(|error| error.to_string())?;
    AbsPath::from_absolute(&cwd.join(value)).map_err(|error| error.to_string())
}

/// The profile whose watch tree holds the path - named with `-p`, or found by
/// looking, in which case exactly one must match.
fn owning_profile<'a>(
    profiles: &'a [Profile],
    target: &AbsPath,
    wanted: Option<&str>,
) -> Result<&'a Profile, Exit> {
    let owners: Vec<&Profile> = profiles
        .iter()
        .filter(|profile| {
            profile
                .watch
                .iter()
                .any(|entry| entry.path().contains(target))
        })
        .collect();
    if let Some(name) = wanted {
        return owners
            .iter()
            .find(|profile| profile.name.as_str() == name)
            .copied()
            .ok_or_else(|| {
                report! {
                    error: "profile '{name}' does not watch {target}",
                    recovery: { "tycho profile list" => "names every profile" },
                }
            });
    }
    match owners.as_slice() {
        [] => Err(report! {
            error: "no watched root contains {target}",
            note: "rules only apply beneath a watched root",
        }),
        [one] => Ok(one),
        many => {
            let names: Vec<&str> = many.iter().map(|profile| profile.name.as_str()).collect();
            let shown = names.join(", ");
            Err(report! {
                error: "more than one profile watches {target}: {shown}",
                recovery: { "tycho rules explain <path> -p <profile>" },
            })
        }
    }
}

/// Builds the profile's tree and folds in the target's ancestor chain of
/// `.tycho/rules.toml` files, under the walk's own read condition. Containment
/// means nothing outside this chain can affect the target, so this is the whole
/// tree as far as the target is concerned. Also returns the deepest ancestor
/// whose verdict is Capture - where a `--local` rule for the target belongs.
fn load_chain(profile: &Profile, target: &AbsPath) -> Result<(RuleTree, AbsPath), Exit> {
    let mut tree = match RuleTree::build(&profile.rule_set()) {
        Ok(tree) => tree,
        Err(error) => return Err(report! { error: "{error}" }),
    };
    let root = profile
        .watch
        .iter()
        .map(crate::config::WatchEntry::path)
        .find(|root| root.contains(target));
    let Some(root) = root else {
        return Err(report! { error: "no watched root contains {target}" });
    };
    let chain: Vec<&Path> = target
        .as_path()
        .ancestors()
        .skip(1)
        .take_while(|ancestor| ancestor.starts_with(root.as_path()))
        .collect();
    let mut anchor: Option<AbsPath> = None;
    for dir in chain.into_iter().rev() {
        if !tree.captures(dir) {
            continue;
        }
        let Ok(base) = AbsPath::from_absolute(dir) else {
            continue;
        };
        anchor = Some(base.clone());
        let file = dir.join(local::DIR_NAME).join(local::FILE_NAME);
        let Ok(text) = std::fs::read_to_string(&file) else {
            continue;
        };
        let rules = match local::parse(&text, &base) {
            Ok(rules) => rules,
            Err(error) => {
                let line = match error {
                    local::LocalRuleError::WatchKey { line }
                    | local::LocalRuleError::Escapes { line, .. } => Some(line),
                    _ => None,
                };
                return Err(if let Some(line) = line {
                    report! { error: "{error}", at: at_file_line(&file, line) }
                } else {
                    report! { error: "{error}", at: at_file(&file) }
                });
            }
        };
        let Ok(file_path) = AbsPath::from_absolute(&file) else {
            continue;
        };
        if let Err(error) = tree.add_local(&base, &file_path, &rules) {
            return Err(report! { error: "{error}", at: at_file(&file) });
        }
    }
    let Some(anchor) = anchor else {
        return Err(report! {
            error: "no captured directory sits between the watched root and {target}",
        });
    };
    Ok((tree, anchor))
}

/// Writes the file, then validates what was written.
///
/// Checking afterwards rather than before is deliberate: the thing worth catching is
/// a rule that is legal TOML and wrong - a watched root that does not exist, a glob
/// that will match nothing - and only the resulting file can be checked for that.
fn finish(editing: &Editing, change: Change, verb: &str, value: &str) -> Exit {
    if let Err(error) = editing.save() {
        return report! { error: "{error}" };
    }
    println!("{}", crate::cli::render::echo(change, verb, value));

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
