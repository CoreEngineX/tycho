//! Layer 2 against real git in temporary directories, which `decisions.md` D6
//! argues is a better double than a mock of the thing the whole design rests on.

use std::collections::BTreeMap;
use std::fs;
use std::os::unix::fs::PermissionsExt;
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

#[test]
fn open_refuses_a_store_others_can_read() {
    let dir = tempfile::tempdir().expect("temp dir");
    let repo = new_store(&dir, "exposed.git");
    let path = abs(repo.path().as_path());

    Repo::open(&path).expect("a 0700 store opens");
    fs::set_permissions(path.as_path(), PermissionsExt::from_mode(0o755)).expect("chmod");
    let error = Repo::open(&path).expect_err("a 0755 store is refused");
    assert!(
        error.to_string().contains("gitignored"),
        "the refusal should say why: {error}"
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
    fs::set_permissions(work.join("file2.txt"), PermissionsExt::from_mode(0o000)).expect("chmod");

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

    let entries: Vec<IndexEntry> = ["plain.md", "a\nb.md", "dir/with space.md", "quote\".md"]
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
    assert!(names.contains(&"a\nb.md".to_owned()), "{names:?}");

    let name = RefName::parse("refs/heads/main").expect("valid");
    repo.update_ref(&name, commit).expect("update-ref");
    let refs = repo.for_each_ref("refs/heads/").expect("for-each-ref");
    assert_eq!(refs.get(&name), Some(&commit));
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
