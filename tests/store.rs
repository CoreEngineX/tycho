//! The store, a real run, and the guarantees they carry.

use std::fs;
use std::path::Path;
use std::process::Command;
use tempfile::TempDir;
use tycho::config::Profile;
use tycho::git::refs::Refspec;
use tycho::primitives::path::AbsPath;
use tycho::state::State;
use tycho::store::{Store, run};

fn abs(path: &Path) -> AbsPath {
    AbsPath::from_absolute(path).expect("temp paths are absolute")
}

fn write(path: &Path, text: &str) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).expect("mkdir");
    }
    fs::write(path, text).expect("write");
}

/// A path inside a TOML basic string needs every `\` escaped, and on Windows every
/// separator is one. A person writing a config by hand would use a literal string;
/// a fixture that interpolates a real path has to escape instead.
fn toml_path(path: &Path) -> String {
    path.display().to_string().replace('\\', "\\\\")
}

fn profile(dir: &Path) -> Profile {
    let root = toml_path(dir);
    let text = format!(
        "version = 1\n[[profile]]\nname = \"demo\"\nwatch = [\"{root}/A\"]\nlocal_only = true\n"
    );
    let parsed = tycho::config::parse_with(&text, Some(Path::new("/nowhere")), |_| None)
        .expect("valid TOML");
    assert!(!parsed.has_errors(), "{:#?}", parsed.diagnostics);
    parsed
        .config
        .profiles
        .into_iter()
        .next()
        .expect("a profile")
}

struct Fixture {
    dir: TempDir,
    store: Store,
    profile: Profile,
    state: State,
}

fn fixture() -> Fixture {
    let dir = tempfile::tempdir().expect("temp dir");
    write(&dir.path().join("A/one.md"), "one\n");
    write(&dir.path().join("A/sub/two.md"), "two\n");
    let store = Store::open_or_init(&abs(&dir.path().join("demo.git"))).expect("init");
    let profile = profile(dir.path());
    Fixture {
        dir,
        store,
        profile,
        state: State::default(),
    }
}

impl Fixture {
    fn run(&mut self) -> Result<run::Completed, run::RunError> {
        let lock = self.dir.path().join("demo.lock");
        let state_path = self.dir.path().join("state.json");
        let paths = run::Paths {
            lock: &lock,
            state: &state_path,
            config_text: None,
        };
        run::execute(&self.profile, &self.store, &paths, &mut self.state, false)
    }

    fn git(&self, args: &[&str]) -> String {
        let out = Command::new("git")
            .current_dir(self.store.path_for_test())
            .args(args)
            .output()
            .expect("git runs");
        assert!(out.status.success(), "git {args:?}: {out:?}");
        String::from_utf8_lossy(&out.stdout).into_owned()
    }
}

#[test]
fn a_run_produces_a_store_plain_git_can_read() {
    let mut fixture = fixture();
    let done = fixture.run().expect("the run succeeds");

    let listed = fixture.git(&["ls-tree", "-r", "--name-only", "HEAD"]);
    let mut names: Vec<&str> = listed.lines().collect();
    names.sort_unstable();
    assert_eq!(names, vec!["A/one.md", "A/sub/two.md"]);

    // The bytes, not just the names.
    let blob = fixture.git(&["show", "HEAD:A/one.md"]);
    assert_eq!(blob, "one\n");

    let subject = fixture.git(&["log", "-1", "--format=%s"]);
    assert!(subject.starts_with("backup "), "{subject}");
    assert!(subject.trim().ends_with("- demo"), "{subject}");
    assert_eq!(done.summary.added, 2);
}

/// D12. A gap in the history is otherwise ambiguous between "nothing changed" and
/// "the backup did not run", and a year of the second going unnoticed is why this
/// project exists.
#[test]
fn a_run_that_changed_nothing_still_commits() {
    let mut fixture = fixture();
    fixture.run().expect("first run");
    let second = fixture.run().expect("second run");

    let count = fixture.git(&["rev-list", "--count", "HEAD"]);
    assert_eq!(count.trim(), "2", "the empty run must still be a commit");
    assert!(second.summary.is_empty());

    let body = fixture.git(&["log", "-1", "--format=%B"]);
    assert!(body.contains("no changes"), "{body}");
}

#[test]
fn each_run_builds_on_the_last() {
    let mut fixture = fixture();
    let first = fixture.run().expect("first run");
    write(&fixture.dir.path().join("A/three.md"), "three\n");
    let second = fixture.run().expect("second run");

    let parent = fixture.git(&["rev-parse", "HEAD^"]);
    assert_eq!(parent.trim(), first.commit.to_string());
    assert_eq!(second.summary.added, 1);

    let head = fixture.git(&["rev-parse", "HEAD"]);
    assert_eq!(head.trim(), second.commit.to_string());
}

/// The lock is a try-lock, so a second run reports the first rather than blocking -
/// a blocking one converts a single hung run into permanently silent backups.
#[test]
fn a_second_run_is_refused_rather_than_interleaved() {
    let mut fixture = fixture();
    let lock = fixture.dir.path().join("demo.lock");
    let held = tycho::sys::lock::try_lock(&lock).expect("take the lock first");

    let error = fixture.run().expect_err("the lock is held");
    assert!(
        matches!(error, run::RunError::InProgress(_)),
        "expected InProgress, got {error}"
    );

    drop(held);
    fixture.run().expect("the lock is free again");
}

#[test]
fn the_run_is_recorded_for_the_next_gate() {
    let mut fixture = fixture();
    let done = fixture.run().expect("run");

    let entries = fixture.state.last_entries("demo");
    assert_eq!(entries.get("A"), Some(&2));
    assert_eq!(done.record.files, 2);

    let saved = State::load(&fixture.dir.path().join("state.json")).expect("load");
    assert_eq!(saved.last("demo").map(|r| r.files), Some(2));
}

/// `history` reads out of the store's own commits, so it works on a replacement
/// machine where the state file is already gone - which is the disaster path.
#[test]
fn history_reads_the_store_rather_than_the_state_file() {
    let mut fixture = fixture();
    fixture.run().expect("first run");
    // Big enough to move the figure after compression: `count-objects -v` reports
    // whole KiB, so a run whose new objects total under one of them records `0 B
    // written` truthfully. Six bytes of `three\n` did, and this test passed on macOS
    // by luck of what the surrounding fixture happened to weigh.
    let bulky: String = (0..4000)
        .map(|line| format!("line {line} carries its own number so it does not fold away\n"))
        .collect();
    write(&fixture.dir.path().join("A/three.md"), &bulky);
    fixture.run().expect("second run");

    let state_path = fixture.dir.path().join("state.json");
    assert!(state_path.exists());
    fs::remove_file(&state_path).expect("simulate a replacement machine");

    let backups = fixture.store.history(10).expect("history");
    assert_eq!(backups.len(), 2);
    let newest = backups[0].summary.as_ref().expect("tycho wrote it");
    assert_eq!(newest.added, 1);
    assert!(
        newest.written_bytes > 0,
        "the written figure must survive in the commit message"
    );
}

/// D13, the slice's named bar. Captured history is reachable only through
/// `refs/tycho/*`, so a pruned ref is garbage rather than "still reachable from an
/// older backup commit" - and git's default two-week expiry would then destroy it.
#[test]
fn a_branch_deleted_upstream_survives_gc_prune_now() {
    let dir = tempfile::tempdir().expect("temp dir");
    let store = Store::open_or_init(&abs(&dir.path().join("store.git"))).expect("init");

    let source = dir.path().join("source");
    fs::create_dir_all(&source).expect("mkdir");
    let git = |args: &[&str]| {
        let out = Command::new("git")
            .current_dir(&source)
            .args(args)
            .output()
            .expect("git runs");
        assert!(out.status.success(), "git {args:?}: {out:?}");
        String::from_utf8_lossy(&out.stdout).trim().to_owned()
    };
    git(&["init", "--quiet", "-b", "main", "."]);
    fs::write(source.join("a.txt"), "a\n").expect("write");
    git(&["add", "."]);
    git(&[
        "-c",
        "user.name=t",
        "-c",
        "user.email=t@t",
        "commit",
        "-qm",
        "one",
    ]);

    // A feature branch with a commit that is unique to it.
    git(&["checkout", "--quiet", "-b", "feature"]);
    fs::write(source.join("b.txt"), "b\n").expect("write");
    git(&["add", "."]);
    git(&[
        "-c",
        "user.name=t",
        "-c",
        "user.email=t@t",
        "commit",
        "-qm",
        "two",
    ]);
    let unique = git(&["rev-parse", "HEAD"]);
    git(&["checkout", "--quiet", "main"]);

    let spec = Refspec::forced("refs/*", "refs/tycho/src/*");
    store
        .repo()
        .fetch_refs(&source, &spec)
        .expect("first fetch");

    // Deleted upstream, then captured again. Without --prune the ref stays, which is
    // what a backup should do.
    git(&["branch", "-D", "feature"]);
    store
        .repo()
        .fetch_refs(&source, &spec)
        .expect("second fetch");

    let out = Command::new("git")
        .current_dir(store.path_for_test())
        .args(["gc", "--prune=now", "--quiet"])
        .output()
        .expect("gc runs");
    assert!(out.status.success(), "gc: {out:?}");

    let readable = Command::new("git")
        .current_dir(store.path_for_test())
        .args(["cat-file", "-e", &unique])
        .output()
        .expect("git runs");
    assert!(
        readable.status.success(),
        "the deleted branch's commit was destroyed; --prune or a default \
         gc.pruneExpire would do exactly this"
    );

    let refs = store
        .repo()
        .for_each_ref("refs/tycho/")
        .expect("for-each-ref");
    assert!(
        refs.keys().any(|name| name.as_str().ends_with("/feature")),
        "the ref itself must survive too: {refs:?}"
    );
}

/// The check `store.md` calls the one that catches the short-batch case, the
/// silently-discarded-path case, and future variants together.
#[test]
fn a_tree_short_of_its_plan_is_refused() {
    let dir = tempfile::tempdir().expect("temp dir");
    let store = Store::open_or_init(&abs(&dir.path().join("store.git"))).expect("init");
    let oid = store.repo().hash_blob(b"x\n").expect("blob");

    let entries = vec![tycho::git::IndexEntry {
        mode: tycho::primitives::encode::FileMode::Regular,
        oid,
        path: tycho::primitives::path::TreePath::parse(Path::new("only.md")).expect("valid"),
    }];
    store.index(&entries).expect("index");
    let tree = store.tree().expect("write-tree");

    store.reconcile(tree, 1).expect("one entry, one planned");
    let error = store
        .reconcile(tree, 2)
        .expect_err("a tree short of the plan must be refused");
    assert!(error.to_string().contains("short of its plan"), "{error}");
}

/// A profile whose store lives elsewhere is not a special case.
#[test]
fn the_store_is_created_where_the_profile_says() {
    let dir = tempfile::tempdir().expect("temp dir");
    let path = dir.path().join("nested").join("elsewhere").join("demo.git");
    let store = Store::open_or_init(&abs(&path)).expect("init creates the parents");
    assert!(path.join("HEAD").exists());
    assert_eq!(
        store.head().expect("head"),
        None,
        "a new store has no backup"
    );
}

/// `hash-object` follows a symlink and stores what it points at. Storing that under
/// mode 120000 would fabricate a file on restore that never existed on the source
/// machine, and a dangling link fails the batch outright.
#[cfg(unix)]
#[test]
fn a_symlink_stores_its_target_rather_than_the_target_s_content() {
    let dir = tempfile::tempdir().expect("temp dir");
    write(&dir.path().join("A/real.md"), "THE TARGET CONTENT\n");
    std::os::unix::fs::symlink("real.md", dir.path().join("A/link.md")).expect("symlink");
    std::os::unix::fs::symlink("nowhere.md", dir.path().join("A/dangling.md")).expect("symlink");

    let store = Store::open_or_init(&abs(&dir.path().join("demo.git"))).expect("init");
    let profile = profile(dir.path());
    let mut fixture = Fixture {
        dir,
        store,
        profile,
        state: State::default(),
    };
    fixture
        .run()
        .expect("a dangling link must not fail the run");

    let listed = fixture.git(&["ls-tree", "-r", "HEAD"]);
    assert!(
        listed.contains("120000 blob") && listed.contains("A/link.md"),
        "the link should be stored as a link: {listed}"
    );
    assert_eq!(
        fixture.git(&["show", "HEAD:A/link.md"]),
        "real.md",
        "the blob must be the link target, not what it points at"
    );
    assert_eq!(
        fixture.git(&["show", "HEAD:A/dangling.md"]),
        "nowhere.md",
        "a dangling link is still a link, and still worth keeping"
    );
    assert_eq!(
        fixture.git(&["show", "HEAD:A/real.md"]),
        "THE TARGET CONTENT\n"
    );
}

/// The same property against the link a Windows account can create without a
/// privilege. What the blob holds is `read_link`'s answer rather than a literal, so
/// this pins the rule - a link stores its target - without asserting a form of the
/// target that is Windows' business and not Tycho's.
#[cfg(windows)]
#[test]
fn a_junction_stores_its_target_rather_than_the_target_s_content() {
    let dir = tempfile::tempdir().expect("temp dir");
    write(&dir.path().join("A/real.md"), "THE TARGET CONTENT\n");
    std::fs::create_dir_all(dir.path().join("A/elsewhere")).expect("mkdir");
    write(&dir.path().join("A/elsewhere/hidden.md"), "HIDDEN\n");

    // Each component joined separately: `join("A/link")` keeps the forward slash on
    // Windows, and `cmd` then reads `/link` as a switch.
    let junction = dir.path().join("A").join("link");
    let made = std::process::Command::new("cmd")
        .args(["/c", "mklink", "/J"])
        .arg(&junction)
        .arg(dir.path().join("A").join("elsewhere"))
        .output()
        .expect("mklink runs");
    assert!(
        made.status.success(),
        "mklink /J: {}",
        String::from_utf8_lossy(&made.stderr)
    );
    let target = std::fs::read_link(&junction).expect("read_link");

    let store = Store::open_or_init(&abs(&dir.path().join("demo.git"))).expect("init");
    let profile = profile(dir.path());
    let mut fixture = Fixture {
        dir,
        store,
        profile,
        state: State::default(),
    };
    fixture.run().expect("a junction must not fail the run");

    let listed = fixture.git(&["ls-tree", "-r", "HEAD"]);
    assert!(
        listed.contains("120000 blob") && listed.contains("A/link"),
        "the junction should be stored as a link: {listed}"
    );
    assert_eq!(
        fixture.git(&["show", "HEAD:A/link"]),
        target.display().to_string(),
        "the blob must be the junction's target, not what it points at"
    );
    assert!(
        !listed.contains("A/link/"),
        "the walk descended through the junction: {listed}"
    );
}
