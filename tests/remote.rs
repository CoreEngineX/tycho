//! Remotes: first contact, the push, and the verification that has to be real.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use tempfile::TempDir;
use tycho::config::Profile;
use tycho::git::refs::PushOutcome;
use tycho::remote::state::RemoteState;
use tycho::remote::{self, RemoteKind};
use tycho::state::{Outcome, State};
use tycho::store::{Store, run};

fn abs(path: &Path) -> tycho::primitives::path::AbsPath {
    tycho::primitives::path::AbsPath::from_absolute(path).expect("temp paths are absolute")
}

fn write(path: &Path, text: &str) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).expect("mkdir");
    }
    fs::write(path, text).expect("write");
}

fn git(dir: &Path, args: &[&str]) -> std::process::Output {
    Command::new("git")
        .current_dir(dir)
        .args(args)
        .output()
        .expect("git runs")
}

fn checked(dir: &Path, args: &[&str]) -> String {
    let out = git(dir, args);
    assert!(out.status.success(), "git {args:?}: {out:?}");
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// One profile, one watched root, and one folder remote inside the same temp
/// directory, so the whole fixture is cleaned up together.
struct Fixture {
    dir: TempDir,
    store: Store,
    profile: Profile,
    state: State,
}

/// A path inside a TOML basic string needs every `\` escaped, and on Windows every
/// separator is one. A person writing a config by hand would use a literal string;
/// a fixture that interpolates a real path has to escape instead.
fn toml_path(path: &Path) -> String {
    path.display().to_string().replace('\\', "\\\\")
}

fn fixture(remote: &str) -> Fixture {
    let dir = tempfile::tempdir().expect("temp dir");
    write(&dir.path().join("A/one.md"), "one\n");
    write(&dir.path().join("A/sub/two.md"), "two\n");

    let root = toml_path(dir.path());
    let extra = if remote.is_empty() {
        "local_only = true\n"
    } else {
        remote
    };
    let text =
        format!("version = 1\n[[profile]]\nname = \"demo\"\nwatch = [\"{root}/A\"]\n{extra}");
    let parsed = tycho::config::parse_with(&text, Some(Path::new("/nowhere")), |_| None)
        .expect("valid TOML");
    assert!(!parsed.has_errors(), "{:#?}", parsed.diagnostics);
    let profile = parsed
        .config
        .profiles
        .into_iter()
        .next()
        .expect("a profile");
    let store = Store::open_or_init(&abs(&dir.path().join("demo.git"))).expect("init");

    Fixture {
        dir,
        store,
        profile,
        state: State::default(),
    }
}

/// A folder remote at `<temp>/<name>`, which does not exist until the run makes it.
fn with_remote(name: &str, optional: bool) -> Fixture {
    rebuild(fixture(""), name, name, optional)
}

/// A remote pointing at a path that will never exist.
fn missing(name: &str, optional: bool) -> Fixture {
    rebuild(fixture(""), name, &format!("never/{name}"), optional)
}

/// The remote's path has to name the fixture's own temp directory, which only exists
/// once the fixture does.
fn rebuild(fixture: Fixture, name: &str, suffix: &str, optional: bool) -> Fixture {
    let root = toml_path(fixture.dir.path());
    let text = format!(
        "version = 1\n[[profile]]\nname = \"demo\"\nwatch = [\"{root}/A\"]\n\
         remotes = [{{ name = \"{name}\", path = \"{root}/{suffix}\", \
         optional = {optional} }}]\n"
    );
    let parsed = tycho::config::parse_with(&text, Some(Path::new("/nowhere")), |_| None)
        .expect("valid TOML");
    assert!(!parsed.has_errors(), "{:#?}", parsed.diagnostics);
    Fixture {
        profile: parsed
            .config
            .profiles
            .into_iter()
            .next()
            .expect("a profile"),
        ..fixture
    }
}

impl Fixture {
    fn paths(&self) -> (PathBuf, PathBuf) {
        (
            self.dir.path().join("demo.lock"),
            self.dir.path().join("state.json"),
        )
    }

    fn run(&mut self) -> run::Completed {
        let (lock, state_path) = self.paths();
        let paths = run::Paths {
            lock: &lock,
            state: &state_path,
            config_text: None,
        };
        run::execute(
            &self.profile,
            &self.store,
            &paths,
            &mut self.state,
            false,
            &mut |_| {},
        )
        .expect("the run")
    }

    fn push(&mut self) -> Option<Vec<run::RemoteResult>> {
        let (lock, state_path) = self.paths();
        let paths = run::Paths {
            lock: &lock,
            state: &state_path,
            config_text: None,
        };
        run::catch_up(
            &self.profile,
            &self.store,
            &paths,
            &mut self.state,
            &mut |_| {},
        )
        .expect("the push")
    }

    fn folder(&self, name: &str) -> PathBuf {
        self.dir.path().join(name)
    }

    fn repo(&self, name: &str) -> PathBuf {
        self.folder(name).join("demo.git")
    }
}

/// First contact against a directory that does not exist: initialise, configure,
/// push, and leave the recovery instructions beside the result.
#[test]
fn a_missing_folder_is_initialised_configured_and_pushed_to() {
    let mut fixture = with_remote("t7", false);
    let done = fixture.run();

    assert_eq!(done.remotes.len(), 1);
    assert!(
        matches!(done.remotes[0].state, RemoteState::Synced { .. }),
        "{:?}",
        done.remotes[0].state
    );

    // receive.autogc is on by default, and a gc inside a cloud-synced folder rewrites
    // packfiles and permanently prunes whatever a forced push orphaned.
    let repo = fixture.repo("t7");
    for (key, expected) in [
        ("receive.autogc", "false"),
        ("gc.auto", "0"),
        ("receive.denyNonFastForwards", "true"),
    ] {
        assert_eq!(
            checked(&repo, &["config", "--get", key]).trim(),
            expected,
            "{key}"
        );
    }
    // A remote whose HEAD dangles clones to an empty repository, and cloning a remote
    // is the disaster path.
    assert_eq!(
        checked(&repo, &["symbolic-ref", "HEAD"]).trim(),
        "refs/heads/main"
    );
    assert!(fixture.folder("t7").join("RECOVERY.md").exists());
}

#[test]
fn the_push_carries_every_ref_and_verification_compares_all_of_them() {
    let mut fixture = with_remote("dest", false);
    fixture.run();

    let repo = fixture.repo("dest");
    let there = checked(&repo, &["for-each-ref", "--format=%(refname)"]);
    let here = checked(
        fixture.store.path_for_test(),
        &["for-each-ref", "--format=%(refname)"],
    );
    assert_eq!(here, there, "the remote holds exactly what the store does");
    assert!(
        remote::verify(&fixture.store, &repo)
            .expect("verify")
            .is_none()
    );
}

/// The case `remotes.md` calls the one worth being loudest about: the push reported
/// success and the remote disagrees.
#[test]
fn a_ref_deleted_behind_tychos_back_fails_verification() {
    let mut fixture = with_remote("dest", false);
    fixture.run();
    let repo = fixture.repo("dest");

    let refs = checked(
        &repo,
        &["for-each-ref", "--format=%(refname)", "refs/tycho/"],
    );
    let victim = refs.lines().next().unwrap_or("refs/heads/main").to_owned();
    assert!(git(&repo, &["update-ref", "-d", &victim]).status.success());

    let complaint = remote::verify(&fixture.store, &repo)
        .expect("verify")
        .expect("a deleted ref must be reported, not shrugged off");
    assert!(complaint.contains("absent"), "{complaint}");
}

#[test]
fn a_directory_of_ordinary_files_is_refused_rather_than_initialised_over() {
    let dir = tempfile::tempdir().expect("temp dir");
    let repo = dir.path().join("demo.git");
    write(&repo.join("holiday.jpg"), "not a repository\n");

    let kind = remote::classify(&repo).expect("classify");
    assert!(matches!(kind, RemoteKind::Foreign { .. }), "{kind:?}");
    assert!(
        kind.refusal()
            .is_some_and(|text| text.contains("holiday.jpg")),
        "the refusal says what is in there"
    );
}

/// A half-synced repository is a different problem with a different remedy, so it is
/// a different variant and a different message.
#[test]
fn a_partially_synced_repository_is_refused_differently() {
    let dir = tempfile::tempdir().expect("temp dir");
    let repo = dir.path().join("demo.git");
    fs::create_dir_all(repo.join("refs")).expect("mkdir");
    write(&repo.join("HEAD"), "ref: refs/heads/main\n");

    let kind = remote::classify(&repo).expect("classify");
    match &kind {
        RemoteKind::Incomplete { missing } => assert!(missing.iter().any(|part| part == "objects")),
        other => panic!("expected Incomplete, got {other:?}"),
    }
    let refusal = kind.refusal().expect("a refusal");
    assert!(refusal.contains("sync client"), "{refusal}");
    assert!(!refusal.contains("somebody's files"), "{refusal}");
}

#[test]
fn an_empty_directory_counts_as_absent_and_gets_initialised() {
    let dir = tempfile::tempdir().expect("temp dir");
    let repo = dir.path().join("demo.git");
    fs::create_dir_all(&repo).expect("mkdir");

    assert_eq!(
        remote::classify(&repo).expect("classify"),
        RemoteKind::Absent
    );
    remote::initialise(&repo).expect("initialise");
    assert_eq!(
        remote::classify(&repo).expect("classify"),
        RemoteKind::Repository
    );
}

/// Directory order is not stable, so first-match-wins could put company backups in an
/// account that must never be written to.
#[test]
fn a_glob_matching_two_directories_fails_and_names_both() {
    let dir = tempfile::tempdir().expect("temp dir");
    for name in ["OneDrive-Personal", "OneDrive-Work"] {
        fs::create_dir_all(dir.path().join(name).join("Backups")).expect("mkdir");
    }

    let error = remote::resolve(&abs(&dir.path().join("OneDrive-*/Backups")))
        .expect_err("two matches is not a choice");
    let text = error.to_string();
    assert!(text.contains("OneDrive-Personal"), "{text}");
    assert!(text.contains("OneDrive-Work"), "{text}");
}

#[test]
fn a_glob_matching_one_directory_resolves_to_it() {
    let dir = tempfile::tempdir().expect("temp dir");
    fs::create_dir_all(dir.path().join("GoogleDrive-me").join("Backups")).expect("mkdir");

    let found =
        remote::resolve(&abs(&dir.path().join("GoogleDrive-*/Backups"))).expect("one match");
    assert!(found.ends_with("GoogleDrive-me/Backups"), "{found:?}");
}

#[test]
fn a_glob_matching_nothing_is_an_error_rather_than_a_guess() {
    let dir = tempfile::tempdir().expect("temp dir");
    assert!(remote::resolve(&abs(&dir.path().join("Nothing-*/Backups"))).is_err());
}

/// An optional remote that is simply out is not a failed backup. A required one is,
/// even though the local commit landed.
#[test]
fn an_unreachable_optional_remote_leaves_the_run_green_and_a_required_one_does_not() {
    let mut optional = missing("gone", true);
    let done = optional.run();
    assert!(
        matches!(done.remotes[0].state, RemoteState::Behind { .. }),
        "{:?}",
        done.remotes[0].state
    );
    assert_eq!(done.record.outcome, Outcome::Partial);

    let mut required = missing("gone", false);
    let done = required.run();
    assert!(
        done.remotes[0].state.is_red(),
        "a required remote's tolerance is 1, so the first miss fails: {:?}",
        done.remotes[0].state
    );
    assert_eq!(done.record.outcome, Outcome::Failed);
}

/// Two machines pushing one profile name are two histories arriving at one
/// repository. The second is rejected and, with `--atomic`, the remote is left
/// exactly as the first machine wrote it.
#[test]
fn a_second_machine_pushing_the_same_profile_name_is_rejected_and_changes_nothing() {
    let mut first = with_remote("shared", false);
    first.run();
    let repo = first.repo("shared");
    let before = checked(
        &repo,
        &["for-each-ref", "--format=%(objectname) %(refname)"],
    );

    // A second machine: its own store, its own content, the same profile name and the
    // same folder.
    let mut second = fixture("");
    write(&second.dir.path().join("A/other.md"), "different\n");
    second.run();

    match second
        .store
        .repo()
        .push(&repo, &remote::refspecs())
        .expect("push runs")
    {
        PushOutcome::Refused { detail } => assert!(!detail.is_empty(), "a refusal says why"),
        PushOutcome::Accepted => {
            panic!("a divergent history must not be accepted onto refs/heads/*")
        }
    }
    assert_eq!(
        before,
        checked(
            &repo,
            &["for-each-ref", "--format=%(objectname) %(refname)"]
        ),
        "--atomic leaves the remote untouched"
    );
}

/// The instructions in `RECOVERY.md` are the disaster path, so they are executed
/// rather than read - against a repository with **no stash**, which is the case that
/// broke two earlier versions of the procedure.
///
/// A refspec naming one exact ref that is absent aborts the entire fetch and writes
/// nothing at all: no branches, no tags. A glob that matches nothing is skipped. So
/// the main fetch has to be globs only.
#[test]
fn the_recovery_instructions_rebuild_a_repository_that_never_had_a_stash() {
    let mut fixture = with_remote("cloud", false);
    let source = fixture.dir.path().join("A/proj");
    fs::create_dir_all(&source).expect("mkdir");
    write(&source.join("tracked.txt"), "committed content\n");
    for args in [
        vec!["init", "-q", "-b", "main"],
        vec!["add", "-A"],
        vec![
            "-c",
            "user.email=a@b",
            "-c",
            "user.name=a",
            "commit",
            "-qm",
            "init",
        ],
    ] {
        assert!(git(&source, &args).status.success(), "{args:?}");
    }
    assert!(
        checked(&source, &["stash", "list"]).is_empty(),
        "the point of the test is that there is no stash"
    );
    fixture.run();

    // Step 1: mirror-clone, never a plain clone, then prove it with fsck.
    let recovery = fixture.dir.path().join("recovery");
    fs::create_dir_all(&recovery).expect("mkdir");
    let clone = recovery.join("store.git");
    assert!(
        Command::new("git")
            .args(["clone", "--mirror", "-q"])
            .arg(fixture.repo("cloud"))
            .arg(&clone)
            .status()
            .expect("git runs")
            .success()
    );
    checked(&clone, &["fsck"]);

    // Step 4: the main fetch, exactly as the file writes it.
    let rebuilt = recovery.join("proj");
    checked(&recovery, &["init", "-q", "-b", "main", "proj"]);
    checked(
        &rebuilt,
        &["symbolic-ref", "HEAD", "refs/heads/__tycho_restore"],
    );
    let store = clone.display().to_string();
    let out = git(
        &rebuilt,
        &[
            "fetch",
            "-q",
            &store,
            "+refs/tycho/A/proj/heads/*:refs/heads/*",
            "+refs/tycho/A/proj/tags/*:refs/tags/*",
            "+refs/tycho/A/proj/remotes/*:refs/remotes/*",
            "+refs/tycho/A/proj/stashes/*:refs/tycho-stash/*",
        ],
    );
    assert!(
        out.status.success(),
        "a fetch of globs must survive every one of them matching nothing: {out:?}"
    );
    checked(&rebuilt, &["checkout", "-q", "main"]);
    // The stored bytes, which is what the recovery has to have preserved. The working
    // file is the operator's own `core.autocrlf`, and these instructions are followed
    // with plain git rather than through Tycho's pins - so on Windows, where that
    // setting defaults to `true`, the checkout lands CRLF. `disaster-recovery.md`
    // records that; it is git's documented behaviour, not lost content.
    let blob = checked(&rebuilt, &["cat-file", "blob", "HEAD:tracked.txt"]);
    assert_eq!(blob, "committed content\n");

    let working = fs::read_to_string(rebuilt.join("tracked.txt")).expect("the file came back");
    let expected = if cfg!(windows) {
        "committed content\r\n"
    } else {
        "committed content\n"
    };
    assert_eq!(working, expected);

    // And the trap itself: the exact-ref stash refspec, on a repository with none.
    let out = git(
        &rebuilt,
        &["fetch", "-q", &store, "+refs/tycho/A/proj/stash:refs/stash"],
    );
    assert!(
        !out.status.success(),
        "an absent exact ref must still fail, which is why it is a separate command"
    );
}

/// A folder that took two profiles gets one `RECOVERY.md` covering both.
#[test]
fn recovery_covers_every_repository_in_the_folder() {
    let mut fixture = with_remote("shared", false);
    fixture.run();
    let folder = fixture.folder("shared");

    remote::initialise(&folder.join("personal.git")).expect("a sibling profile");
    remote::recovery::write(&folder).expect("write");

    let text = fs::read_to_string(folder.join("RECOVERY.md")).expect("read");
    assert!(text.contains("demo.git"), "{text}");
    assert!(text.contains("personal.git"), "{text}");
}

#[test]
fn the_catch_up_push_does_not_capture() {
    let mut fixture = with_remote("dest", false);
    fixture.run();
    let before = checked(
        fixture.store.path_for_test(),
        &["rev-list", "--count", "HEAD"],
    );

    write(
        &fixture.dir.path().join("A/new.md"),
        "added after the run\n",
    );
    let results = fixture.push().expect("the lock was free");

    assert!(matches!(results[0].state, RemoteState::Synced { .. }));
    assert_eq!(
        before,
        checked(
            fixture.store.path_for_test(),
            &["rev-list", "--count", "HEAD"]
        ),
        "capture happens on the backup schedule and nowhere else"
    );
}

/// A held lock means a run is in progress and about to push anyway, so yielding is
/// success rather than a contention error to retry.
#[test]
fn a_catch_up_push_yields_to_a_run_in_progress() {
    let mut fixture = with_remote("dest", false);
    let (lock, state_path) = fixture.paths();
    let held = tycho::sys::lock::try_lock(&lock).expect("the lock is free");

    let paths = run::Paths {
        lock: &lock,
        state: &state_path,
        config_text: None,
    };
    let result = run::catch_up(
        &fixture.profile,
        &fixture.store,
        &paths,
        &mut fixture.state,
        &mut |_| {},
    )
    .expect("a held lock is not an error");
    assert!(result.is_none());
    drop(held);
}

/// Ejecting a drive is simulated by renaming its folder, and Windows refuses to
/// rename a directory while any handle under it is still open. Git leaves one for a
/// moment after a push, so the first attempt can fail with `Access is denied` on a
/// tree nothing is actually using.
///
/// Only the simulation needs this. Tycho never renames a remote - a real ejection is
/// the volume going away, which needs no handle to be released.
fn rename_when_windows_lets_go(from: &Path, to: &Path) {
    let mut last = None;
    for attempt in 0..50 {
        match fs::rename(from, to) {
            Ok(()) => return,
            Err(error) => {
                last = Some(error);
                std::thread::sleep(std::time::Duration::from_millis(20 * (attempt + 1)));
            }
        }
    }
    panic!("plug it back in: {:?}", last.expect("at least one attempt"));
}

/// A remote that goes away and comes back is caught up by the next push, without the
/// run having to know anything happened.
///
/// The drive is modelled by its **mount point**: what disappears when a volume is
/// ejected is `/Volumes/T7`, not the backup folder inside it, which is exactly the
/// distinction that separates unreachable from first contact.
#[test]
fn a_remote_that_returns_is_synced_again_by_the_next_push() {
    let fixture = fixture("");
    let mount = fixture.dir.path().join("mount");
    fs::create_dir_all(&mount).expect("mount it");
    let mut fixture = rebuild(fixture, "t7", "mount/tycho", true);
    fixture.run();

    let unplugged = fixture.folder("unplugged");
    fs::rename(&mount, &unplugged).expect("eject it");
    let done = fixture.push().expect("the lock was free");
    assert!(
        matches!(done[0].state, RemoteState::Behind { runs: 1, .. }),
        "an ejected optional drive is behind, not failed: {:?}",
        done[0].state
    );

    rename_when_windows_lets_go(&unplugged, &mount);
    let done = fixture.push().expect("the lock was free");
    assert!(
        matches!(done[0].state, RemoteState::Synced { .. }),
        "{:?}",
        done[0].state
    );
}

/// The other half of that distinction. A folder whose parent is missing is a volume
/// that is not mounted, and creating it would write the backup to the boot disk
/// underneath the mount point.
#[test]
fn a_missing_mount_point_is_never_created() {
    let mut fixture = missing("t7", true);
    fixture.run();
    assert!(!fixture.dir.path().join("never").exists());
}
