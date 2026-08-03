//! The walk, against real directory trees.

use std::collections::BTreeMap;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use tempfile::TempDir;
use tycho::config::Profile;
use tycho::config::rules::RuleTree;
use tycho::plan::{Entry, ExcludeReason, PlanError, Warning, build};
use tycho::primitives::encode::FileMode;

/// Builds a config against a temp directory, so `~/A` in the docs becomes a real
/// tree the walk has to descend.
fn profile(dir: &Path, extra: &str) -> Profile {
    let root = dir.display().to_string();
    let text = format!(
        "version = 1\n[[profile]]\nname = \"work\"\nwatch = [\"{root}/A\"]\nlocal_only = true\n{extra}"
    );
    let parsed = tycho::config::parse_with(&text, Some(Path::new("/nowhere")), |_| None)
        .expect("valid TOML");
    assert!(
        !parsed.has_errors(),
        "fixture config has errors: {:#?}",
        parsed.diagnostics
    );
    parsed
        .config
        .profiles
        .into_iter()
        .next()
        .expect("a profile")
}

fn write(path: &Path, text: &str) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).expect("mkdir");
    }
    fs::write(path, text).expect("write");
}

fn run(dir: &TempDir, extra: &str) -> Result<tycho::plan::Plan, PlanError> {
    let profile = profile(dir.path(), extra);
    let tree = RuleTree::build(&profile.rule_set()).expect("rules compile");
    build(&profile, &tree, &BTreeMap::new(), false)
}

fn stored(plan: &tycho::plan::Plan) -> Vec<String> {
    plan.roots
        .iter()
        .flat_map(|root| &root.entries)
        .filter_map(|entry| match entry {
            Entry::Plain(file) => Some(file.stored.to_string()),
            Entry::Repo(_) => None,
        })
        .collect()
}

fn repo_keys(plan: &tycho::plan::Plan) -> Vec<String> {
    plan.roots
        .iter()
        .flat_map(tycho::plan::RootPlan::repos)
        .map(|repo| repo.key.clone())
        .collect()
}

/// Marks a directory as a repository. `bare_file` makes the submodule form, where
/// `.git` is a file holding a gitdir pointer rather than a directory.
fn make_repo(dir: &Path, as_file: bool) {
    fs::create_dir_all(dir).expect("mkdir");
    if as_file {
        fs::write(dir.join(".git"), "gitdir: ../../.git/modules/x\n").expect("write");
    } else {
        fs::create_dir_all(dir.join(".git")).expect("mkdir");
        fs::write(dir.join(".git").join("HEAD"), "ref: refs/heads/main\n").expect("write");
    }
}

#[test]
fn a_watched_root_yields_its_files_under_its_alias() {
    let dir = tempfile::tempdir().expect("temp dir");
    write(&dir.path().join("A/x.md"), "x");
    write(&dir.path().join("A/sub/y.md"), "y");

    let plan = run(&dir, "").expect("the walk succeeds");
    let mut names = stored(&plan);
    names.sort();
    assert_eq!(names, vec!["A/sub/y.md", "A/x.md"]);
    assert_eq!(plan.files(), 2);
    assert_eq!(plan.bytes(), 2);
}

/// The one that fails green. Pruning descent wherever the verdict is Skip would
/// never reach the re-included subtree.
#[test]
fn a_reincluded_subtree_survives_an_ignored_ancestor() {
    let dir = tempfile::tempdir().expect("temp dir");
    let root = dir.path().display().to_string();
    write(&dir.path().join("A/s/dropped.bin"), "no");
    write(&dir.path().join("A/s/keep/k.pem"), "yes");

    let plan = run(
        &dir,
        &format!("ignore = [\"{root}/A/s\"]\nreinclude = [\"{root}/A/s/keep\"]\n"),
    )
    .expect("the walk succeeds");

    let names = stored(&plan);
    assert!(
        names.contains(&"A/s/keep/k.pem".to_owned()),
        "the re-included file was never reached: {names:?}"
    );
    assert!(
        !names.contains(&"A/s/dropped.bin".to_owned()),
        "the ignored sibling came through: {names:?}"
    );
}

#[test]
fn junk_directories_are_excluded_without_being_descended() {
    let dir = tempfile::tempdir().expect("temp dir");
    write(&dir.path().join("A/keep.md"), "k");
    write(&dir.path().join("A/node_modules/pkg/index.js"), "j");
    write(&dir.path().join("A/target/debug/build.o"), "o");

    let plan = run(&dir, "").expect("the walk succeeds");
    assert_eq!(stored(&plan), vec!["A/keep.md"]);
}

/// Content stops at a repository boundary; discovery continues through it. Both
/// forms of `.git` count, because every platform repository on this machine is a
/// submodule whose `.git` is a file.
#[test]
fn repositories_are_found_recursively_and_their_files_are_not_plain() {
    let dir = tempfile::tempdir().expect("temp dir");
    write(&dir.path().join("A/loose.md"), "l");
    make_repo(&dir.path().join("A/org"), false);
    write(&dir.path().join("A/org/tracked.md"), "t");
    make_repo(&dir.path().join("A/org/handbook"), true);
    write(&dir.path().join("A/org/handbook/deep.md"), "d");

    let plan = run(&dir, "").expect("the walk succeeds");

    let mut keys = repo_keys(&plan);
    keys.sort();
    assert_eq!(
        keys,
        vec!["A/org", "A/org/handbook"],
        "the nested submodule must be captured in its own right, not as a gitlink"
    );
    assert_eq!(
        stored(&plan),
        vec!["A/loose.md"],
        "a repository's files are history, not loose blobs"
    );
    for name in stored(&plan) {
        assert!(!name.contains(".git"), "{name} reached the store tree");
    }
}

#[test]
fn a_repository_key_percent_encodes_each_component() {
    let dir = tempfile::tempdir().expect("temp dir");
    write(&dir.path().join("A/keep.md"), "k");
    make_repo(&dir.path().join("A/my repo"), false);

    let plan = run(&dir, "").expect("the walk succeeds");
    assert_eq!(repo_keys(&plan), vec!["A/my%20repo"]);
}

#[test]
fn symlinks_are_stored_as_links_and_never_followed() {
    let dir = tempfile::tempdir().expect("temp dir");
    write(&dir.path().join("A/target.md"), "t");
    fs::create_dir_all(dir.path().join("A/elsewhere")).expect("mkdir");
    write(&dir.path().join("A/elsewhere/hidden.md"), "h");
    std::os::unix::fs::symlink("elsewhere", dir.path().join("A/link")).expect("symlink");
    std::os::unix::fs::symlink("nowhere", dir.path().join("A/dangling")).expect("symlink");

    let plan = run(&dir, "").expect("the walk succeeds");
    let links: Vec<&tycho::plan::PlainFile> = plan
        .roots
        .iter()
        .flat_map(|root| &root.entries)
        .filter_map(|entry| match entry {
            Entry::Plain(file) if file.mode == FileMode::Symlink => Some(file),
            _ => None,
        })
        .collect();
    assert_eq!(links.len(), 2, "both links are entries in their own right");

    let names = stored(&plan);
    assert!(
        !names.iter().any(|name| name.starts_with("A/link/")),
        "the walk followed a symlink: {names:?}"
    );
    assert!(names.contains(&"A/elsewhere/hidden.md".to_owned()));
}

#[test]
fn a_non_utf8_filename_survives_into_the_store_path() {
    use std::ffi::OsStr;
    use std::os::unix::ffi::OsStrExt;

    let dir = tempfile::tempdir().expect("temp dir");
    fs::create_dir_all(dir.path().join("A")).expect("mkdir");
    let mut raw = dir.path().join("A").into_os_string().into_encoded_bytes();
    raw.extend_from_slice(b"/caf\xc3\xa9 \tname.md");
    let hostile = Path::new(OsStr::from_bytes(&raw));
    fs::write(hostile, "x").expect("write");

    let plan = run(&dir, "").expect("the walk succeeds");
    assert_eq!(stored(&plan), vec!["A/café \tname.md"]);
}

#[test]
fn a_root_that_cannot_be_read_fails_the_run() {
    let dir = tempfile::tempdir().expect("temp dir");
    write(&dir.path().join("A/x.md"), "x");
    fs::set_permissions(dir.path().join("A"), PermissionsExt::from_mode(0o000)).expect("chmod");

    let error = run(&dir, "").expect_err("a denied root must fail the run");
    assert!(
        matches!(error, PlanError::RootUnreadable { .. }),
        "expected RootUnreadable, got {error}"
    );

    fs::set_permissions(dir.path().join("A"), PermissionsExt::from_mode(0o755)).expect("chmod");
}

/// The a sibling daemon failure rebuilt inside the tool written to prevent it: a
/// green, empty backup.
#[test]
fn a_root_that_yields_nothing_fails_the_run() {
    let dir = tempfile::tempdir().expect("temp dir");
    fs::create_dir_all(dir.path().join("A")).expect("mkdir");

    let error = run(&dir, "").expect_err("an empty root must fail the run");
    assert!(
        matches!(error, PlanError::RootEmpty { .. }),
        "expected RootEmpty, got {error}"
    );
}

#[test]
fn a_directory_that_cannot_be_read_is_a_warning_and_the_rest_still_plans() {
    let dir = tempfile::tempdir().expect("temp dir");
    write(&dir.path().join("A/keep.md"), "k");
    write(&dir.path().join("A/denied/x.md"), "x");
    fs::set_permissions(
        dir.path().join("A/denied"),
        PermissionsExt::from_mode(0o000),
    )
    .expect("chmod");

    let plan = run(&dir, "").expect("a leaf failure is not fatal");
    assert_eq!(stored(&plan), vec!["A/keep.md"]);
    assert!(
        plan.warnings()
            .any(|warning| matches!(warning, Warning::Unreadable { .. })),
        "the unreadable directory should be named"
    );

    fs::set_permissions(
        dir.path().join("A/denied"),
        PermissionsExt::from_mode(0o755),
    )
    .expect("chmod");
}

#[test]
fn a_large_drop_fails_unless_it_is_allowed() {
    let dir = tempfile::tempdir().expect("temp dir");
    for index in 0..10 {
        write(&dir.path().join(format!("A/f{index}.md")), "x");
    }
    let profile = profile(dir.path(), "");
    let tree = RuleTree::build(&profile.rule_set()).expect("rules compile");

    let mut previous = BTreeMap::new();
    previous.insert("A".to_owned(), 1_000);

    let error = build(&profile, &tree, &previous, false).expect_err("a 99% drop must fail");
    assert!(
        matches!(error, PlanError::RootShrank { .. }),
        "expected RootShrank, got {error}"
    );
    build(&profile, &tree, &previous, true).expect("--allow-shrink accepts it");
}

#[test]
fn a_rule_that_matched_nothing_is_reported() {
    let dir = tempfile::tempdir().expect("temp dir");
    let root = dir.path().display().to_string();
    write(&dir.path().join("A/keep.md"), "k");
    write(&dir.path().join("A/noisy.log"), "l");

    let plan = run(
        &dir,
        &format!("ignore = [\"{root}/A/scrach\", \"*.log\", \"*.never\"]\n"),
    )
    .expect("the walk succeeds");

    let nothing: Vec<&String> = plan
        .excluded
        .iter()
        .filter(|(_, reason)| *reason == ExcludeReason::MatchedNothing)
        .map(|(rule, _)| rule)
        .collect();
    assert!(
        nothing.iter().any(|rule| rule.ends_with("A/scrach")),
        "the typo'd path should be listed: {nothing:?}"
    );
    assert!(
        nothing.iter().any(|rule| *rule == "*.never"),
        "the glob that fired nowhere should be listed: {nothing:?}"
    );
    assert!(
        !nothing.iter().any(|rule| *rule == "*.log"),
        "a glob that did fire must not be listed: {nothing:?}"
    );
    assert_eq!(
        plan.excluded
            .iter()
            .find(|(rule, _)| rule == "*.log")
            .map(|(_, reason)| *reason),
        Some(ExcludeReason::GlobRule),
        "a glob that fired is reported as what it threw away"
    );
    assert!(
        !nothing.iter().any(|rule| *rule == "Thumbs.db"),
        "the default junk list must not bury the row that matters: {nothing:?}"
    );
}

#[test]
fn a_watched_root_may_name_a_single_file() {
    let dir = tempfile::tempdir().expect("temp dir");
    write(&dir.path().join("A"), "just a file");

    let plan = run(&dir, "").expect("the walk succeeds");
    assert_eq!(stored(&plan), vec!["A"]);
}

#[test]
fn the_dry_run_renders_to_the_documented_column_model() {
    let dir = tempfile::tempdir().expect("temp dir");
    write(&dir.path().join("A/one.md"), "0123456789");
    write(&dir.path().join("A/two.md"), "0123456789");
    write(&dir.path().join("A/target/x.o"), "junk");

    let plan = run(&dir, "").expect("the walk succeeds");
    let rendered = tycho::cli::render::dry_run(&plan, &[], true);

    for line in rendered.lines() {
        assert!(
            line.chars().count() <= tycho::cli::render::WIDTH,
            "a row runs past the table width: {line:?}"
        );
    }
    assert!(rendered.contains("roots"), "{rendered}");
    assert!(rendered.contains("  A             "), "{rendered}");
    assert!(rendered.contains("20 B"), "{rendered}");
    assert!(
        rendered.contains("target                                          default junk"),
        "{rendered}"
    );
    assert!(
        !rendered.contains("repositories"),
        "--quick omits the expensive half: {rendered}"
    );
    assert!(rendered.contains("to read"), "{rendered}");
}
