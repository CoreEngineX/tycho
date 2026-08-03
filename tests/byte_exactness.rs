//! The invariant the whole design rests on: the blob stored for a file is
//! `cmp`-identical to the bytes on disk, and the bytes a restore produces are
//! `cmp`-identical to the blob.
//!
//! It is not free. `hash-object -w` applies gitattributes and clean filters, and
//! `git archive` applies them again on the way out - from a `.gitattributes`
//! captured into the store's own tree, which needs no unusual configuration
//! anywhere. Each half here is run twice: once with its mechanism, and once without
//! it, because a byte-exactness test that passes for the wrong reason is worse than
//! none.

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;
use tempfile::TempDir;
use tycho::git::repo::NEUTRAL_ATTRIBUTES;
use tycho::git::{Hashed, IndexEntry, Repo};
use tycho::primitives::encode::FileMode;
use tycho::primitives::oid::Oid;
use tycho::primitives::path::{AbsPath, TreePath};
use tycho::sys::process::{Git, Timeout};

/// The five cases `store.md` names, plus the `.gitattributes` that arms them.
/// Every one is a file a user could plausibly have.
const ATTRIBUTES: &str = "\
* text=auto
*.lfs filter=fake
*.id ident
*.subst export-subst
hidden.txt export-ignore
";

fn fixtures() -> Vec<(&'static str, Vec<u8>)> {
    vec![
        (".gitattributes", ATTRIBUTES.as_bytes().to_vec()),
        ("windows.txt", b"line one\r\nline two\r\n".to_vec()),
        ("big.lfs", b"pretend this is a large binary\n".to_vec()),
        ("stamped.id", b"$Id$\nbody\n".to_vec()),
        ("expanded.subst", b"$Format:%H$\n".to_vec()),
        ("hidden.txt", b"the file export-ignore drops\n".to_vec()),
    ]
}

fn write_fixtures(dir: &Path) -> Vec<(String, Vec<u8>)> {
    fs::create_dir_all(dir).expect("mkdir");
    let mut written = Vec::new();
    for (name, bytes) in fixtures() {
        fs::write(dir.join(name), &bytes).expect("write fixture");
        written.push((name.to_owned(), bytes));
    }
    written
}

fn store(dir: &TempDir, name: &str) -> Repo {
    let path = AbsPath::from_absolute(&dir.path().join(name)).expect("absolute");
    Repo::init_bare(&path).expect("init")
}

/// A `clean` filter that mangles, and the line-ending conversion that mangles by
/// default. Written into the store's *own* config, which outranks a global one - so
/// beating it is the stronger test, and it needs no environment mutation.
fn arm_the_store_hostilely(repo: &Repo) {
    repo.set_config("core.autocrlf", "true").expect("config");
    repo.set_config("core.eol", "crlf").expect("config");
    repo.set_config("filter.fake.clean", "sed 's/.*/REPLACED BY A FILTER/'")
        .expect("config");
    fs::write(
        repo.path().as_path().join("info").join("attributes"),
        ATTRIBUTES,
    )
    .expect("arm attributes");
}

fn hash_all(repo: &Repo, work: &Path, names: &[String]) -> BTreeMap<String, Oid> {
    let paths: Vec<AbsPath> = names
        .iter()
        .map(|name| AbsPath::from_absolute(&work.join(name)).expect("absolute"))
        .collect();
    let outcomes = repo.hash_object_batch(&paths).expect("batch");
    names
        .iter()
        .zip(outcomes)
        .map(|(name, outcome)| match outcome {
            Hashed::Object(oid) => (name.clone(), oid),
            Hashed::Unreadable { reason } => panic!("{name} was unreadable: {reason}"),
        })
        .collect()
}

/// Capture half: `--no-filters` plus the pinned config must defeat both, even with
/// the store armed against us.
#[test]
fn capture_stores_the_bytes_that_are_on_disk() {
    let dir = tempfile::tempdir().expect("temp dir");
    let repo = store(&dir, "store.git");
    let work = dir.path().join("work");
    let written = write_fixtures(&work);
    let names: Vec<String> = written.iter().map(|(name, _)| name.clone()).collect();

    arm_the_store_hostilely(&repo);

    let oids = hash_all(&repo, &work, &names);
    for (name, want) in &written {
        let blob = repo.cat_file_blob(oids[name]).expect("read the blob back");
        assert_eq!(
            &blob, want,
            "{name} was altered on the way in despite --no-filters"
        );
    }
}

/// The teeth for the half above. Without `--no-filters`, git applies exactly what
/// the store was armed with - so the test proves a mechanism rather than a habit.
#[test]
fn without_no_filters_the_same_bytes_are_altered() {
    let dir = tempfile::tempdir().expect("temp dir");
    let repo = store(&dir, "store.git");
    let work = dir.path().join("work");
    let written = write_fixtures(&work);

    arm_the_store_hostilely(&repo);

    let mut altered = Vec::new();
    for (name, want) in &written {
        let path = work.join(name);
        let out = Git::at(repo.path().as_path())
            .checked(
                &["hash-object", "-w", "--", &path.display().to_string()],
                Timeout::QUICK,
            )
            .expect("hash-object without --no-filters");
        let oid = Oid::parse(String::from_utf8_lossy(&out.stdout).trim()).expect("oid");
        if repo.cat_file_blob(oid).expect("blob") != *want {
            altered.push(name.clone());
        }
    }

    assert!(
        !altered.is_empty(),
        "nothing was altered without --no-filters, so the flag guards nothing here"
    );
    assert!(
        altered.iter().any(|name| name == "big.lfs"),
        "the clean filter did not fire; altered: {altered:?}"
    );
}

fn build_tree(repo: &Repo, dir: &TempDir, oids: &BTreeMap<String, Oid>) -> Oid {
    let entries: Vec<IndexEntry> = oids
        .iter()
        .map(|(name, oid)| IndexEntry {
            mode: FileMode::Regular,
            oid: *oid,
            path: TreePath::parse(Path::new(name)).expect("valid"),
        })
        .collect();
    let index = repo
        .scratch_index(dir.path().join("scratch-index"))
        .expect("index");
    index.update(&entries).expect("update");
    index.write_tree().expect("write-tree")
}

fn extract(repo: &Repo, tree: Oid, into: &Path) -> BTreeMap<String, Vec<u8>> {
    let tar = into.with_extension("tar");
    repo.archive(tree, &[], &tar).expect("archive");
    fs::create_dir_all(into).expect("mkdir");
    let status = std::process::Command::new("tar")
        .arg("-xf")
        .arg(&tar)
        .current_dir(into)
        .status()
        .expect("tar runs");
    assert!(status.success(), "tar failed");

    let mut found = BTreeMap::new();
    for entry in fs::read_dir(into).expect("read dir") {
        let path = entry.expect("entry").path();
        if path.is_file() {
            let name = path
                .file_name()
                .expect("a name")
                .to_string_lossy()
                .into_owned();
            found.insert(name, fs::read(&path).expect("read"));
        }
    }
    found
}

/// Restore half: the store's own `info/attributes` must neutralise the
/// `.gitattributes` sitting in the tree being archived.
#[test]
fn restore_reproduces_the_bytes_that_were_captured() {
    let dir = tempfile::tempdir().expect("temp dir");
    let repo = store(&dir, "store.git");
    let work = dir.path().join("work");
    let written = write_fixtures(&work);
    let names: Vec<String> = written.iter().map(|(name, _)| name.clone()).collect();
    let oids = hash_all(&repo, &work, &names);
    let tree = build_tree(&repo, &dir, &oids);

    let found = extract(&repo, tree, &dir.path().join("restored"));

    for (name, want) in &written {
        let got = found
            .get(name)
            .unwrap_or_else(|| panic!("{name} was not extracted at all"));
        assert_eq!(got, want, "{name} came back with different bytes");
    }
}

/// The teeth for the half above. `info/attributes` does not survive a mirror clone,
/// so a restore that forgets to re-establish it takes this path - which is the
/// disaster case, and it fails at exit 0.
#[test]
fn without_the_stores_attributes_a_restore_drops_and_rewrites() {
    let dir = tempfile::tempdir().expect("temp dir");
    let repo = store(&dir, "store.git");
    let work = dir.path().join("work");
    let written = write_fixtures(&work);
    let names: Vec<String> = written.iter().map(|(name, _)| name.clone()).collect();
    let oids = hash_all(&repo, &work, &names);
    let tree = build_tree(&repo, &dir, &oids);

    let attributes = repo.path().as_path().join("info").join("attributes");
    assert_eq!(
        fs::read_to_string(&attributes).expect("attrs"),
        NEUTRAL_ATTRIBUTES
    );
    fs::remove_file(&attributes).expect("simulate a mirror clone");

    let found = extract(&repo, tree, &dir.path().join("restored"));

    assert!(
        !found.contains_key("hidden.txt"),
        "export-ignore did not drop the file, so info/attributes guards nothing"
    );
    let mut rewritten = Vec::new();
    for (name, want) in &written {
        if found.get(name).is_some_and(|got| got != want) {
            rewritten.push(name.clone());
        }
    }
    assert!(
        !rewritten.is_empty(),
        "nothing was rewritten without info/attributes"
    );

    // And the whole point: git said everything was fine.
    repo.write_attributes().expect("re-establish");
    let repaired = extract(&repo, tree, &dir.path().join("repaired"));
    for (name, want) in &written {
        assert_eq!(
            repaired.get(name),
            Some(want),
            "{name} is still wrong after re-establishing the attributes"
        );
    }
}
