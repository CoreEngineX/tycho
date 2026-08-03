//! The three-way path resolution in `cli.md` section 7.
//!
//! A path can name a plain file, an overlay entry, or a file that has no path in the
//! backup tree at all because it is tracked and clean inside a captured repository.
//! Getting it wrong is not a crash - it is a restore that quietly answers from the
//! wrong half of the store.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use tempfile::TempDir;
use tycho::config::Profile;
use tycho::primitives::path::AbsPath;
use tycho::restore::resolve::{self, Resolved};
use tycho::state::State;
use tycho::store::{Store, run};

/// A path inside a TOML basic string needs every `\` escaped, and on Windows every
/// separator is one. A person writing a config by hand would use a literal string;
/// a fixture that interpolates a real path has to escape instead.
fn toml_path(path: &Path) -> String {
    path.display().to_string().replace('\\', "\\\\")
}

fn write(path: &Path, text: &str) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).expect("mkdir");
    }
    fs::write(path, text).expect("write");
}

fn git(dir: &Path, args: &[&str]) {
    let out = Command::new("git")
        .current_dir(dir)
        .args(args)
        .output()
        .expect("git runs");
    assert!(out.status.success(), "git {args:?}: {out:?}");
}

struct Fixture {
    dir: TempDir,
    store: Store,
    profile: Profile,
    state: State,
}

/// One watched root holding a plain file and a repository. The repository has a
/// tracked-and-clean file, a modified tracked file, and a gitignored file - which are
/// the three answers, in order.
fn fixture() -> Fixture {
    let dir = tempfile::tempdir().expect("temp dir");
    let root = dir.path().join("A");
    write(&root.join("loose.md"), "a plain watched file\n");

    let repo = root.join("proj");
    write(&repo.join("tracked.md"), "committed and untouched\n");
    write(&repo.join("edited.md"), "committed\n");
    write(&repo.join(".gitignore"), "secret.env\n");
    git(&repo, &["init", "-q", "-b", "main"]);
    git(&repo, &["add", "-A"]);
    git(
        &repo,
        &[
            "-c",
            "user.email=a@b",
            "-c",
            "user.name=a",
            "commit",
            "-qm",
            "the only commit",
        ],
    );
    write(&repo.join("edited.md"), "committed, then edited\n");
    write(&repo.join("secret.env"), "TOKEN=hunter2\n");

    let text = format!(
        "version = 1\n[[profile]]\nname = \"demo\"\nwatch = [\"{}\"]\nlocal_only = true\n",
        toml_path(&root)
    );
    let parsed = tycho::config::parse_with(&text, Some(Path::new("/nowhere")), |_| None)
        .expect("valid TOML");
    assert!(!parsed.has_errors(), "{:#?}", parsed.diagnostics);
    let profile = parsed
        .config
        .profiles
        .into_iter()
        .next()
        .expect("a profile");
    let store = Store::open_or_init(
        &AbsPath::from_absolute(&dir.path().join("demo.git")).expect("absolute"),
    )
    .expect("init");

    let mut fixture = Fixture {
        dir,
        store,
        profile,
        state: State::default(),
    };
    fixture.run();
    fixture
}

impl Fixture {
    fn run(&mut self) {
        let lock = self.dir.path().join("demo.lock");
        let state_path = self.dir.path().join("state.json");
        let paths = run::Paths {
            lock: &lock,
            state: &state_path,
            config_text: None,
        };
        run::execute(&self.profile, &self.store, &paths, &mut self.state, false).expect("the run");
    }

    fn resolve(&self, path: &str) -> Resolved {
        let backup = self.backup();
        resolve::resolve(&self.store, &backup, Path::new(path))
            .unwrap_or_else(|error| panic!("resolving {path}: {error}"))
    }

    fn backup(&self) -> resolve::Backup {
        let commit = self
            .store
            .at(None)
            .expect("at")
            .expect("the run made a backup");
        resolve::Backup::read(&self.store, commit).expect("read the backup")
    }
}

/// A file outside every captured repository is a path in the backup tree and comes
/// straight out of it.
#[test]
fn a_plain_file_resolves_to_the_backup_tree() {
    let fixture = fixture();
    match fixture.resolve("A/loose.md") {
        Resolved::StoreFile { path } => {
            assert_eq!(path.as_path(), Path::new("A/loose.md"));
        }
        other => panic!("expected a store file, got {other:?}"),
    }
}

/// A gitignored file inside a repository is what the overlay exists for: git alone
/// could never bring it back, and this is the exact shape of the July 2026 incident.
#[test]
fn a_gitignored_file_resolves_to_the_overlay() {
    let fixture = fixture();
    match fixture.resolve("A/proj/secret.env") {
        Resolved::Overlay { key, rest, path } => {
            assert_eq!(key, "A/proj");
            assert_eq!(rest, PathBuf::from("secret.env"));
            assert_eq!(
                path.as_path(),
                Path::new(".tycho/repos/A/proj/overlay/secret.env")
            );
        }
        other => panic!("expected the overlay, got {other:?}"),
    }
}

/// A modified tracked file is also in the overlay, because the overlay holds what was
/// on disk rather than what was committed.
#[test]
fn a_modified_tracked_file_resolves_to_the_overlay_not_history() {
    let fixture = fixture();
    let resolved = fixture.resolve("A/proj/edited.md");
    assert!(
        matches!(resolved, Resolved::Overlay { .. }),
        "the on-disk version is the one that was backed up: {resolved:?}"
    );
    assert!(resolved.source().contains("uncommitted"));
}

/// The case that decides how restore works. A clean tracked file has **no path in the
/// backup tree at all** - its content is in the object database under
/// `refs/tycho/<key>/*` - and it is the normal case, not an edge case.
#[test]
fn a_tracked_and_clean_file_resolves_to_the_repositorys_own_history() {
    let fixture = fixture();
    let backup = fixture.backup();
    assert!(
        !backup
            .tree()
            .iter()
            .any(|entry| entry.as_path() == Path::new("A/proj/tracked.md")),
        "if this path were in the tree the test would be proving nothing"
    );

    match fixture.resolve("A/proj/tracked.md") {
        Resolved::Tracked {
            key, rest, subject, ..
        } => {
            assert_eq!(key, "A/proj");
            assert_eq!(rest, PathBuf::from("tracked.md"));
            assert_eq!(subject, "the only commit");
        }
        other => panic!("expected repository history, got {other:?}"),
    }
}

/// Longest prefix, not first. A machine with both `A/proj` and `A/proj/nested`
/// captured must resolve a path inside the nested one against the nested one, or every
/// file in it would be looked for in its parent's history and not found.
#[test]
fn the_longest_matching_repository_key_wins() {
    let dir = tempfile::tempdir().expect("temp dir");
    let root = dir.path().join("A");
    for name in ["proj", "proj/nested"] {
        let repo = root.join(name);
        write(&repo.join("f.txt"), &format!("{name}\n"));
        git(&repo, &["init", "-q", "-b", "main"]);
        git(&repo, &["add", "-A"]);
        git(
            &repo,
            &[
                "-c",
                "user.email=a@b",
                "-c",
                "user.name=a",
                "commit",
                "-qm",
                name,
            ],
        );
    }

    let text = format!(
        "version = 1\n[[profile]]\nname = \"demo\"\nwatch = [\"{}\"]\nlocal_only = true\n",
        toml_path(&root)
    );
    let parsed = tycho::config::parse_with(&text, Some(Path::new("/nowhere")), |_| None)
        .expect("valid TOML");
    let profile = parsed
        .config
        .profiles
        .into_iter()
        .next()
        .expect("a profile");
    let store = Store::open_or_init(
        &AbsPath::from_absolute(&dir.path().join("demo.git")).expect("absolute"),
    )
    .expect("init");
    let mut fixture = Fixture {
        dir,
        store,
        profile,
        state: State::default(),
    };
    fixture.run();

    match fixture.resolve("A/proj/nested/f.txt") {
        Resolved::Tracked { key, subject, .. } => {
            assert_eq!(key, "A/proj/nested");
            assert_eq!(subject, "proj/nested", "the nested repo's own commit");
        }
        other => panic!("expected the nested repository, got {other:?}"),
    }
}

#[test]
fn a_path_that_is_in_no_backup_says_so_rather_than_guessing() {
    let fixture = fixture();
    let backup = fixture.backup();
    let error = resolve::resolve(&fixture.store, &backup, Path::new("A/nothing.md"))
        .expect_err("not in the backup");
    assert!(error.to_string().contains("A/nothing.md"), "{error}");
}

/// A plain file's history is backup runs; a tracked file's history is its own
/// repository's commits, which is what you want, because "the version before I broke
/// it" is a commit in your repo, not a Sunday.
#[test]
fn the_two_answer_shapes_read_from_different_ref_sets() {
    let fixture = fixture();

    let plain = resolve::history(&fixture.store, &fixture.resolve("A/loose.md"), 20).expect("log");
    assert_eq!(plain.len(), 1, "one backup run so far");
    assert!(plain[0].subject.contains("demo"), "{}", plain[0].subject);

    let tracked =
        resolve::history(&fixture.store, &fixture.resolve("A/proj/tracked.md"), 20).expect("log");
    assert_eq!(tracked.len(), 1);
    assert_eq!(
        tracked[0].subject, "the only commit",
        "the repository's own history, not the backup run"
    );
}
