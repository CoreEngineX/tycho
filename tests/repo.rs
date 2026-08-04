//! Layer 2 against real git in temporary directories, which `decisions.md` D6
//! argues is a better double than a mock of the thing the whole design rests on.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use tempfile::TempDir;
use tycho::git::refs::{PushOutcome, Refspec};
use tycho::git::{Hashed, IndexEntry, Repo};
use tycho::primitives::encode::FileMode;
use tycho::primitives::names::RefName;
use tycho::primitives::oid::Oid;
use tycho::primitives::path::{AbsPath, TreePath};
use tycho::sys::process::{Git, Timeout};

fn abs(path: &Path) -> AbsPath {
    AbsPath::from_absolute(path).expect("temp paths are absolute")
}

/// Makes the store readable by someone other than its owner.
#[cfg(unix)]
fn expose(path: &Path) {
    use std::os::unix::fs::PermissionsExt;

    fs::set_permissions(path, PermissionsExt::from_mode(0o755)).expect("chmod");
}

/// `BUILTIN\Users` by SID, because `icacls` takes and prints localised account names
/// and the SID is the same on every install.
#[cfg(windows)]
fn expose(path: &Path) {
    icacls(&[
        &path.display().to_string(),
        "/grant",
        "*S-1-5-32-545:(OI)(CI)(RX)",
    ]);
}

/// Denies `Everyone`, which includes the owner, so the file genuinely cannot be
/// opened - the NTFS equivalent of `chmod 000` and verified to refuse a read.
#[cfg(windows)]
fn make_unreadable(path: &Path) {
    icacls(&[&path.display().to_string(), "/deny", "*S-1-1-0:(R)"]);
}

#[cfg(unix)]
fn make_unreadable(path: &Path) {
    use std::os::unix::fs::PermissionsExt;

    fs::set_permissions(path, PermissionsExt::from_mode(0o000)).expect("chmod");
}

#[cfg(windows)]
fn icacls(args: &[&str]) {
    let out = std::process::Command::new("icacls")
        .args(args)
        .output()
        .expect("icacls runs");
    assert!(
        out.status.success(),
        "icacls {args:?}: {}",
        String::from_utf8_lossy(&out.stdout)
    );
}

fn new_store(dir: &TempDir, name: &str) -> Repo {
    Repo::init_bare(&abs(&dir.path().join(name))).expect("init")
}

fn tree_path(text: &str) -> TreePath {
    TreePath::parse(Path::new(text)).expect("a valid tree path")
}

/// A source repository with one commit on `main`, plus an optional tag.
fn source_repo(dir: &Path, tag: Option<&str>) -> PathBuf {
    let repo = dir.to_path_buf();
    fs::create_dir_all(&repo).expect("mkdir");
    let git = Git::at(&repo);
    git.checked(&["init", "--quiet", "-b", "main", "."], Timeout::QUICK)
        .expect("init");
    fs::write(repo.join("file.txt"), "content\n").expect("write");
    git.checked(&["add", "."], Timeout::QUICK).expect("add");
    git.checked(&["commit", "--quiet", "-m", "one"], Timeout::QUICK)
        .expect("commit");
    if let Some(tag) = tag {
        // Annotated, so ls-remote emits the `^{}` peel line the ref parser skips.
        git.checked(&["tag", "-a", tag, "-m", tag], Timeout::QUICK)
            .expect("tag");
    }
    repo
}

#[test]
fn init_bare_writes_every_setting_store_md_requires() {
    let dir = tempfile::tempdir().expect("temp dir");
    let repo = new_store(&dir, "coreenginex.git");

    for (key, want) in [
        ("core.logAllRefUpdates", "always"),
        ("gc.pruneExpire", "never"),
        ("gc.reflogExpire", "never"),
        ("gc.reflogExpireUnreachable", "never"),
        ("core.bare", "true"),
    ] {
        assert_eq!(
            repo.config(key).expect("read config").as_deref(),
            Some(want),
            "{key}"
        );
    }

    let head = fs::read_to_string(repo.path().as_path().join("HEAD")).expect("HEAD");
    assert_eq!(head.trim(), "ref: refs/heads/main");

    let attributes =
        fs::read_to_string(repo.path().as_path().join("info").join("attributes")).expect("attrs");
    assert_eq!(
        attributes,
        "* -text -diff -filter -ident -export-subst -export-ignore\n"
    );

    // The same guarantee, asked in each platform's own terms: no group or other bits
    // on Unix, no DACL trustee beyond the owner, SYSTEM and Administrators on NTFS.
    // `Repo::open` is what enforces it, so opening is what proves it.
    Repo::open(&abs(repo.path().as_path())).expect("a store only its owner can read opens");

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        let mode = fs::metadata(repo.path().as_path())
            .expect("stat")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(
            mode & 0o077,
            0,
            "the store is readable by others: {mode:04o}"
        );
    }
}

#[test]
fn open_refuses_a_store_others_can_read() {
    let dir = tempfile::tempdir().expect("temp dir");
    let repo = new_store(&dir, "exposed.git");
    let path = abs(repo.path().as_path());

    Repo::open(&path).expect("a store only its owner can read opens");
    expose(path.as_path());
    let error = Repo::open(&path).expect_err("a store others can read is refused");
    assert!(
        error.to_string().contains("gitignored"),
        "the refusal should say why: {error}"
    );
}

/// An exFAT volume, attached for the length of one test and detached whatever happens.
///
/// A disk image rather than a real drive because the property under test belongs to
/// the filesystem, not to the hardware, and a test that needs a drive plugged in is a
/// test nobody runs.
#[cfg(target_os = "macos")]
struct Attached {
    volume: PathBuf,
}

#[cfg(target_os = "macos")]
impl Drop for Attached {
    fn drop(&mut self) {
        let _ = std::process::Command::new("hdiutil")
            .args(["detach", "-quiet", "-force"])
            .arg(&self.volume)
            .output();
    }
}

#[cfg(target_os = "macos")]
fn attach_exfat(dir: &Path, name: &str) -> Attached {
    // An exFAT label is capped at 11 characters and `hdiutil` answers a longer one
    // with `Operation not permitted`, which reads like a privilege problem and is not.
    assert!(name.len() <= 11, "an exFAT volume label is 11 characters");
    let image = dir.join(format!("{name}.dmg"));
    let made = std::process::Command::new("hdiutil")
        .args(["create", "-size", "40m", "-fs", "ExFAT", "-volname", name])
        .arg(&image)
        .output()
        .expect("hdiutil runs");
    assert!(
        made.status.success(),
        "hdiutil create: {}{}",
        String::from_utf8_lossy(&made.stdout),
        String::from_utf8_lossy(&made.stderr)
    );
    let attached = std::process::Command::new("hdiutil")
        .args(["attach", "-quiet"])
        .arg(&image)
        .output()
        .expect("hdiutil runs");
    assert!(
        attached.status.success(),
        "hdiutil attach: {}",
        String::from_utf8_lossy(&attached.stderr)
    );
    Attached {
        volume: PathBuf::from(format!("/Volumes/{name}")),
    }
}

/// **A volume that records no ownership must be refused, and it is the case that looks
/// safest.**
///
/// `chmod` on exFAT succeeds and changes nothing: the mode reads back as `0700`
/// whatever you set, because the kernel synthesises owner and mode from the mount. So
/// the check that asks only for `mode & 0o077` sees the ideal answer and passes a
/// store that every account on the machine can read - while it holds exactly the
/// gitignored content the store exists to keep.
///
/// The Windows arm refuses this as `NO_ACCESS_CONTROL`. This is the same hole on the
/// same kind of drive, which `remotes.md` expects an external backup to live on.
#[cfg(target_os = "macos")]
#[test]
fn open_refuses_a_store_on_a_volume_that_records_no_ownership() {
    let dir = tempfile::tempdir().expect("temp dir");
    let attached = attach_exfat(dir.path(), "TychoNoOwn");

    let store = attached.volume.join("store.git");
    fs::create_dir_all(&store).expect("mkdir on the volume");
    let _ = std::process::Command::new("chmod")
        .arg("700")
        .arg(&store)
        .output();

    // The premise, asserted rather than assumed: the mode this volume reports is the
    // one the old check would have accepted.
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = fs::metadata(&store).expect("stat").permissions().mode() & 0o777;
        assert_eq!(
            mode & 0o077,
            0,
            "this test means nothing unless exFAT reports an owner-only mode: {mode:04o}"
        );
    }

    let error = Repo::open(&abs(&store)).expect_err("a store on a noowners volume is refused");
    let message = error.to_string();
    assert!(
        message.contains("noowners"),
        "the refusal should name the cause: {message}"
    );
    assert!(
        message.contains("gitignored"),
        "and why it matters: {message}"
    );
}

#[test]
fn a_batch_names_the_file_it_could_not_read_and_keeps_going() {
    let dir = tempfile::tempdir().expect("temp dir");
    let repo = new_store(&dir, "store.git");
    let work = dir.path().join("work");
    fs::create_dir_all(&work).expect("mkdir");

    let mut paths = Vec::new();
    for index in 0..5 {
        let path = work.join(format!("file{index}.txt"));
        fs::write(&path, format!("content {index}\n")).expect("write");
        paths.push(abs(&path));
    }
    // The middle one, because a naive implementation truncates everything after it.
    make_unreadable(&work.join("file2.txt"));

    let outcomes = repo.hash_object_batch(&paths).expect("the batch completes");
    assert_eq!(
        outcomes.len(),
        paths.len(),
        "one outcome per planned path, always"
    );
    for (index, outcome) in outcomes.iter().enumerate() {
        match (index, outcome) {
            (2, Hashed::Unreadable { reason }) => {
                assert!(
                    reason.contains("file2.txt"),
                    "the reason names it: {reason}"
                );
            }
            (2, other) => panic!("file2 should be unreadable, got {other:?}"),
            (_, Hashed::Object(oid)) => {
                let blob = repo.cat_file_blob(*oid).expect("read back");
                assert_eq!(blob, format!("content {index}\n").into_bytes());
            }
            (_, other) => panic!("file{index} should have hashed, got {other:?}"),
        }
    }
}

#[test]
fn an_index_becomes_a_tree_that_holds_hostile_paths() {
    let dir = tempfile::tempdir().expect("temp dir");
    let repo = new_store(&dir, "store.git");
    let oid = repo.hash_blob(b"body\n").expect("blob");

    // `a\nb.md` and `quote".md` hold characters NTFS reserves, and git for Windows
    // will not put such a path in the index. They stay in the Unix list because there
    // they are ordinary names; `a_path_git_will_not_index_is_an_error_not_a_shortfall`
    // covers what Windows does with them.
    let hostile: &[&str] = if cfg!(windows) {
        &["plain.md", "dir/with space.md"]
    } else {
        &["plain.md", "a\nb.md", "dir/with space.md", "quote\".md"]
    };
    let entries: Vec<IndexEntry> = hostile
        .iter()
        .map(|name| IndexEntry {
            mode: FileMode::Regular,
            oid,
            path: tree_path(name),
        })
        .collect();

    let index = repo
        .scratch_index(dir.path().join("scratch-index"))
        .expect("index");
    index.update(&entries).expect("update-index");
    assert_eq!(index.len().expect("count"), entries.len());

    let tree = index.write_tree().expect("write-tree");
    let commit = repo.commit_tree(tree, None, "backup").expect("commit-tree");

    let changes = repo.diff_tree(None, tree).expect("diff-tree");
    assert_eq!(changes.len(), entries.len(), "a first run lists every path");
    assert!(
        changes
            .iter()
            .all(|change| change.status == tycho::git::read::ChangeStatus::Added)
    );
    let names: Vec<String> = changes
        .iter()
        .map(|change| change.path.to_string())
        .collect();
    assert!(names.contains(&"dir/with space.md".to_owned()), "{names:?}");
    #[cfg(unix)]
    assert!(names.contains(&"a\nb.md".to_owned()), "{names:?}");

    let name = RefName::parse("refs/heads/main").expect("valid");
    repo.update_ref(&name, commit).expect("update-ref");
    let refs = repo.for_each_ref("refs/heads/").expect("for-each-ref");
    assert_eq!(refs.get(&name), Some(&commit));
}

/// The trapdoor under every capture on Windows.
///
/// `update-index --index-info` answers a path holding a character NTFS reserves by
/// printing `Ignoring path` and **exiting 0**. A caller reading only the status
/// writes a tree short of its plan - the empty tree, if every path was refused - and
/// publishes it as a backup. `\` is in that set, and a `PathBuf` built by `join` is
/// `\`-separated here, so this is one `TreePath` slip away rather than exotic.
#[cfg(windows)]
#[test]
fn a_path_git_will_not_index_is_an_error_not_a_shortfall() {
    let dir = tempfile::tempdir().expect("temp dir");
    let repo = new_store(&dir, "store.git");
    let oid = repo.hash_blob(b"body\n").expect("blob");

    let index = repo
        .scratch_index(dir.path().join("scratch-index"))
        .expect("index");
    let error = index
        .update(&[IndexEntry {
            mode: FileMode::Regular,
            oid,
            path: tree_path("quote\".md"),
        }])
        .expect_err("a refused path must not pass for a stored one");

    assert!(
        error.to_string().contains("did not fail doing it"),
        "the refusal must name itself: {error}"
    );
    assert_eq!(
        index.len().expect("count"),
        0,
        "nothing was stored, which is what the error is about"
    );
}

#[test]
fn a_scratch_index_never_inherits_a_stale_entry() {
    let dir = tempfile::tempdir().expect("temp dir");
    let repo = new_store(&dir, "store.git");
    let oid = repo.hash_blob(b"x\n").expect("blob");
    let path = dir.path().join("scratch-index");

    let first = repo.scratch_index(path.clone()).expect("index");
    first
        .update(&[IndexEntry {
            mode: FileMode::Regular,
            oid,
            path: tree_path("stale.md"),
        }])
        .expect("update");
    assert_eq!(first.len().expect("count"), 1);

    let second = repo.scratch_index(path).expect("index");
    assert!(
        second.is_empty().expect("count"),
        "the second session inherited the first's entries"
    );
}

#[test]
fn a_fetch_never_follows_tags_into_the_stores_own_namespace() {
    let dir = tempfile::tempdir().expect("temp dir");
    let store = new_store(&dir, "store.git");

    for key in ["one", "two"] {
        // Both source repositories tag v1.0, which is the collision --no-tags exists
        // to prevent.
        let source = source_repo(&dir.path().join(key), Some("v1.0"));
        store
            .fetch_refs(
                &source,
                &Refspec::forced("refs/*", &format!("refs/tycho/{key}/*")),
            )
            .expect("fetch");
    }

    let own_tags = store.for_each_ref("refs/tags/").expect("for-each-ref");
    assert!(
        own_tags.is_empty(),
        "tags leaked into the store's own namespace: {own_tags:?}"
    );

    let captured = store.for_each_ref("refs/tycho/").expect("for-each-ref");
    let names: Vec<&str> = captured.keys().map(RefName::as_str).collect();
    for key in ["one", "two"] {
        assert!(
            names.contains(&format!("refs/tycho/{key}/tags/v1.0").as_str()),
            "{key}'s tag is missing from {names:?}"
        );
    }
}

#[test]
fn an_atomic_push_that_is_refused_leaves_the_remote_untouched() {
    let dir = tempfile::tempdir().expect("temp dir");
    let machine_a = new_store(&dir, "a.git");
    let machine_b = new_store(&dir, "b.git");
    let remote = new_store(&dir, "remote.git");

    let specs = [
        Refspec::fast_forward("refs/heads/*", "refs/heads/*"),
        Refspec::forced("refs/tycho/*", "refs/tycho/*"),
    ];
    assert!(!specs[0].is_forced(), "heads must never be forced");
    assert!(specs[1].is_forced());

    let main = RefName::parse("refs/heads/main").expect("valid");
    let captured = RefName::parse("refs/tycho/one/heads/main").expect("valid");

    // Machine A publishes a backup, and a captured ref alongside it.
    let a_commit = commit_in(&machine_a, "from a");
    machine_a.update_ref(&main, a_commit).expect("update-ref");
    machine_a
        .update_ref(&captured, a_commit)
        .expect("update-ref");
    assert_eq!(
        machine_a
            .push(remote.path().as_path(), &specs)
            .expect("push"),
        PushOutcome::Accepted
    );
    let after_a: BTreeMap<RefName, Oid> = machine_a
        .ls_remote(remote.path().as_path())
        .expect("ls-remote");
    assert_eq!(after_a.get(&main), Some(&a_commit));
    assert_eq!(after_a.get(&captured), Some(&a_commit));

    // Machine B has unrelated history under the same profile name, and a captured
    // ref that a non-atomic push would happily force through.
    let b_commit = commit_in(&machine_b, "from b");
    machine_b.update_ref(&main, b_commit).expect("update-ref");
    machine_b
        .update_ref(&captured, b_commit)
        .expect("update-ref");

    let outcome = machine_b
        .push(remote.path().as_path(), &specs)
        .expect("push runs");
    let PushOutcome::Refused { detail } = outcome else {
        panic!("a second machine's push must be refused, not accepted");
    };
    assert!(!detail.is_empty(), "a refusal must say why");

    let after_b = machine_b
        .ls_remote(remote.path().as_path())
        .expect("ls-remote");
    assert_eq!(
        after_b, after_a,
        "the refused push still moved a ref, so --atomic did not hold"
    );
}

fn commit_in(repo: &Repo, message: &str) -> Oid {
    let oid = repo.hash_blob(message.as_bytes()).expect("blob");
    let index = repo
        .scratch_index(repo.path().as_path().join(format!("idx-{message}")))
        .expect("index");
    index
        .update(&[IndexEntry {
            mode: FileMode::Regular,
            oid,
            path: tree_path("file.md"),
        }])
        .expect("update");
    let tree = index.write_tree().expect("write-tree");
    repo.commit_tree(tree, None, message).expect("commit-tree")
}
