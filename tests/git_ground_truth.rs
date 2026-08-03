//! Layer 0's encoders are validated against real git, not against a model of git.
//!
//! Each encoder's contract is "git accepts this and gets back what I meant", so a
//! unit test written from a reading of git's parser proves only that the reading is
//! self-consistent. These tests ask git.

use std::ffi::OsString;
use std::fs::File;
use std::io::Write;
use std::os::unix::ffi::OsStringExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use tempfile::TempDir;
use tycho::primitives::encode::{FileMode, index_info_line, percent_component, stdin_paths_line};
use tycho::primitives::oid::Oid;

/// Names covering every row of `capture.md`'s hostile-filename matrix, plus the two
/// printable bytes that still force quoting.
const HOSTILE: [&[u8]; 11] = [
    b"plain.md",
    b"with space.md",
    b"new\nline.md",
    b"tab\there.md",
    b"quote\"inside.md",
    b"back\\slash.md",
    b"\"leading-quote.md",
    b"trailing-cr\r.md",
    b"caf\xc3\xa9.md",
    b"invalid\xffbyte.md",
    b"percent%25.md",
];

fn git(cwd: &Path, args: &[&str]) -> std::process::Output {
    Command::new("git")
        .current_dir(cwd)
        .args(args)
        .output()
        .expect("git is a hard requirement of this project")
}

fn bare_repo() -> TempDir {
    let dir = tempfile::tempdir().expect("temp dir");
    let out = git(dir.path(), &["init", "--bare", "--object-format=sha1", "."]);
    assert!(out.status.success(), "git init: {out:?}");
    dir
}

fn path_of(dir: &Path, name: &[u8]) -> PathBuf {
    let mut raw = dir.as_os_str().as_encoded_bytes().to_vec();
    raw.push(b'/');
    raw.extend_from_slice(name);
    PathBuf::from(OsString::from_vec(raw))
}

/// `hash-object --stdin-paths` must hash the file we meant, for every hostile name.
///
/// Stdin comes from a file rather than a pipe, so this cannot hit the deadlock that
/// layer 1 exists to solve.
#[test]
fn stdin_paths_survives_every_hostile_filename() {
    let work = tempfile::tempdir().expect("temp dir");
    let store = bare_repo();

    let mut batch = Vec::new();
    let mut expected = Vec::new();
    let mut refused = Vec::new();
    for (index, name) in HOSTILE.iter().enumerate() {
        let file = path_of(work.path(), name);
        let content = format!("content of file {index}\n");
        match File::create(&file) {
            Ok(mut handle) => {
                handle.write_all(content.as_bytes()).expect("write");
                batch.extend_from_slice(&stdin_paths_line(&file));
                expected.push(content);
            }
            Err(error) => {
                // APFS enforces valid UTF-8 in filenames, so the non-UTF-8 row of
                // capture.md's matrix is unreachable end to end on macOS. The
                // encoder still handles those bytes; encode.rs covers them purely.
                assert!(
                    std::str::from_utf8(name).is_err(),
                    "the filesystem refused a valid UTF-8 name {:?}: {error}",
                    String::from_utf8_lossy(name)
                );
                refused.push(name);
            }
        }
    }
    assert_eq!(
        expected.len() + refused.len(),
        HOSTILE.len(),
        "every hostile name is either exercised or accounted for"
    );

    let batch_file = work.path().join("batch");
    File::create(&batch_file)
        .expect("create batch")
        .write_all(&batch)
        .expect("write batch");

    let out = Command::new("git")
        .current_dir(store.path())
        .args(["hash-object", "-w", "--no-filters", "--stdin-paths"])
        .stdin(Stdio::from(File::open(&batch_file).expect("open batch")))
        .output()
        .expect("run hash-object");
    assert!(
        out.status.success(),
        "hash-object rejected the batch: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let oids: Vec<Oid> = String::from_utf8(out.stdout)
        .expect("hash-object prints hex")
        .lines()
        .map(|line| Oid::parse(line).expect("a valid object id"))
        .collect();
    assert_eq!(
        oids.len(),
        expected.len(),
        "one hash per path, or a name split a line in two"
    );

    for (oid, want) in oids.iter().zip(&expected) {
        let blob = git(store.path(), &["cat-file", "blob", &oid.to_string()]);
        assert!(blob.status.success());
        assert_eq!(
            String::from_utf8_lossy(&blob.stdout),
            *want,
            "{oid} holds the wrong file's content"
        );
    }
}

/// Every percent-encoded component must assemble into a refname git accepts.
#[test]
fn percent_encoding_always_produces_a_refname_git_accepts() {
    let dir = bare_repo();
    let mut components: Vec<Vec<u8>> = HOSTILE.iter().map(|name| name.to_vec()).collect();
    components.extend([
        b".config".to_vec(),
        b"main.lock".to_vec(),
        b"..".to_vec(),
        b"@{".to_vec(),
        b"~^:?*[".to_vec(),
        b"a//b".to_vec(),
        b"-".to_vec(),
        b".".to_vec(),
        (0u8..=255).collect(),
    ]);

    for raw in &components {
        let refname = format!("refs/tycho/{}/heads/main", percent_component(raw));
        let out = git(dir.path(), &["check-ref-format", &refname]);
        assert!(
            out.status.success(),
            "git rejected {refname} (from {:?})",
            String::from_utf8_lossy(raw)
        );
    }
}

/// `update-index -z --index-info` must round-trip a path's raw bytes untouched,
/// including the two forms that the non-`-z` mode mangles at exit 0.
#[test]
fn index_info_round_trips_raw_path_bytes() {
    let work = tempfile::tempdir().expect("temp dir");
    let store = bare_repo();
    let index = work.path().join("index");

    let empty = git(
        store.path(),
        &["hash-object", "-w", "-t", "blob", "--stdin"],
    );
    assert!(empty.status.success());
    let oid = Oid::parse(String::from_utf8_lossy(&empty.stdout).trim()).expect("valid oid");

    let names: Vec<&[u8]> = vec![
        b"plain.md",
        b"new\nline.md",
        b"\"leading-quote.md",
        b"back\\slash.md",
        b"caf\xc3\xa9.md",
        b"looks.git.md",
    ];

    let mut batch = Vec::new();
    for name in &names {
        let path = PathBuf::from(OsString::from_vec(name.to_vec()));
        batch.extend_from_slice(&index_info_line(FileMode::Regular, oid, &path));
    }

    let batch_file = work.path().join("batch");
    File::create(&batch_file)
        .expect("create batch")
        .write_all(&batch)
        .expect("write batch");

    let out = Command::new("git")
        .current_dir(store.path())
        .env("GIT_INDEX_FILE", &index)
        .args(["update-index", "-z", "--index-info"])
        .stdin(Stdio::from(File::open(&batch_file).expect("open batch")))
        .output()
        .expect("run update-index");
    assert!(
        out.status.success(),
        "update-index rejected the batch: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let listed = Command::new("git")
        .current_dir(store.path())
        .env("GIT_INDEX_FILE", &index)
        .args(["ls-files", "-z"])
        .output()
        .expect("run ls-files");
    assert!(listed.status.success());

    let mut got: Vec<Vec<u8>> = listed
        .stdout
        .split(|&byte| byte == 0)
        .filter(|entry| !entry.is_empty())
        .map(<[u8]>::to_vec)
        .collect();
    got.sort_unstable();

    let mut want: Vec<Vec<u8>> = names.iter().map(|name| name.to_vec()).collect();
    want.sort_unstable();

    assert_eq!(got.len(), want.len(), "an entry was silently discarded");
    assert_eq!(got, want, "a path's bytes were altered in the index");
}
