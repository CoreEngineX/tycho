//! Layer 0's encoders are validated against real git, not against a model of git.
//!
//! Each encoder's contract is "git accepts this and gets back what I meant", so a
//! unit test written from a reading of git's parser proves only that the reading is
//! self-consistent. These tests ask git.

use std::fs::File;
use std::io::Write;
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

/// `None` when the platform has no name for these bytes at all, which is a different
/// thing from a filesystem that has one and refuses it.
#[cfg(unix)]
fn path_from_bytes(raw: &[u8]) -> Option<PathBuf> {
    use std::ffi::OsString;
    use std::os::unix::ffi::OsStringExt;

    Some(PathBuf::from(OsString::from_vec(raw.to_vec())))
}

#[cfg(windows)]
fn path_from_bytes(raw: &[u8]) -> Option<PathBuf> {
    std::str::from_utf8(raw).ok().map(PathBuf::from)
}

fn path_of(dir: &Path, name: &[u8]) -> Option<PathBuf> {
    path_from_bytes(name).map(|name| dir.join(name))
}

/// Whether this platform's filesystem can hold the name at all.
///
/// APFS enforces valid UTF-8, so that is the only row of `capture.md`'s matrix macOS
/// cannot reach.
#[cfg(unix)]
fn nameable_here(name: &[u8]) -> bool {
    std::str::from_utf8(name).is_ok()
}

/// NTFS needs UTF-16, and additionally refuses `<>:"|?*`, both separators, and every
/// byte below `0x20` - the control characters are the part `config.md` section 10 did
/// not list, and a newline in a name is refused with `InvalidFilename` rather than
/// stored. Six rows of the matrix are therefore unreachable here rather than one,
/// measured by this test rather than read from a table.
#[cfg(windows)]
fn nameable_here(name: &[u8]) -> bool {
    std::str::from_utf8(name).is_ok_and(|text| {
        !text.contains(['<', '>', ':', '"', '|', '?', '*', '\\', '/'])
            && !text.chars().any(|c| (c as u32) < 0x20)
    })
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
        let content = format!("content of file {index}\n");
        let created = path_of(work.path(), name).and_then(|file| {
            File::create(&file)
                .map(|handle| (file, handle))
                .map_err(|error| {
                    // A name the platform can hold and the filesystem still refuses
                    // is a bug in this test's model of the filesystem, not a skip.
                    assert!(
                        !nameable_here(name),
                        "the filesystem refused a name this platform can hold, {:?}: {error}",
                        String::from_utf8_lossy(name)
                    );
                })
                .ok()
        });
        match created {
            Some((file, mut handle)) => {
                handle.write_all(content.as_bytes()).expect("write");
                batch.extend_from_slice(&stdin_paths_line(&file));
                expected.push(content);
            }
            None => refused.push(name),
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

    // On Unix a path is an index entry and never a file, so every hostile name is
    // fair game. Git for Windows refuses to hold any name NTFS could not - the same
    // predicate the filesystem answers with - and drops it at exit 0, which
    // `a_reserved_name_is_dropped_at_exit_zero` pins separately.
    let names: Vec<&[u8]> = [
        b"plain.md".as_slice(),
        b"new\nline.md",
        b"\"leading-quote.md",
        b"caf\xc3\xa9.md",
        b"looks.git.md",
        b"back\\slash.md",
    ]
    .into_iter()
    .filter(|name| nameable_here(name))
    .collect();

    let mut batch = Vec::new();
    for name in &names {
        let path = path_from_bytes(name).expect("an index path this platform can build");
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

/// The ground truth the whole of `TreePath`'s separator contract rests on.
///
/// Git for Windows refuses to put a path holding any character NTFS reserves into the
/// index. It prints `Ignoring path` on **stderr** and exits **0**, and `write-tree`
/// then yields a tree short of that entry - the empty tree, if every path was
/// refused. `\` is in that set, and a `PathBuf` built from components is
/// `\`-separated here, so an unfixed run commits an empty backup and reports success.
///
/// Pinned per character rather than argued from one case, because the exit status is
/// what a caller would otherwise trust.
#[cfg(windows)]
#[test]
fn a_reserved_name_is_dropped_at_exit_zero() {
    let work = tempfile::tempdir().expect("temp dir");
    let store = bare_repo();

    let empty = git(
        store.path(),
        &["hash-object", "-w", "-t", "blob", "--stdin"],
    );
    assert!(empty.status.success());
    let oid = Oid::parse(String::from_utf8_lossy(&empty.stdout).trim()).expect("valid oid");

    for (index, name) in [
        r"sub\file.md",
        "star*.md",
        "colon:x.md",
        "q?.md",
        "pipe|x.md",
        "quote\".md",
        "tab\there.md",
    ]
    .into_iter()
    .enumerate()
    {
        let index_file = work.path().join(format!("index{index}"));
        let batch = index_info_line(FileMode::Regular, oid, Path::new(name));
        let batch_file = work.path().join(format!("batch{index}"));
        File::create(&batch_file)
            .expect("create batch")
            .write_all(&batch)
            .expect("write batch");

        let out = Command::new("git")
            .current_dir(store.path())
            .env("GIT_INDEX_FILE", &index_file)
            .args(["update-index", "-z", "--index-info"])
            .stdin(Stdio::from(File::open(&batch_file).expect("open batch")))
            .output()
            .expect("run update-index");

        assert!(
            out.status.success(),
            "the danger is precisely that this succeeds: {name:?}"
        );
        assert!(
            String::from_utf8_lossy(&out.stderr).contains("Ignoring path"),
            "git changed how it reports a rejected path {name:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );

        let listed = Command::new("git")
            .current_dir(store.path())
            .env("GIT_INDEX_FILE", &index_file)
            .args(["ls-files", "-z"])
            .output()
            .expect("run ls-files");
        assert!(listed.status.success());
        assert!(
            listed.stdout.is_empty(),
            "{name:?} was dropped, so nothing may be listed"
        );
    }
}

/// The ground truth `capture::check_collisions` exists for.
///
/// A repository holding both `Feature` and `feature` maps two refs onto one file on a
/// case-insensitive volume. The comment on that check used to say git errors on first
/// exposure and only the following fetch is silent. On NTFS git never says anything:
/// every fetch exits 0 and keeps whichever it wrote last, so the captured branch
/// oscillates between runs and each run drops the other one.
///
/// Pinned because the check is the *only* thing that notices. If a future git starts
/// refusing, this fails and the check can be reconsidered; until then it must not be
/// weakened into trusting git to complain.
#[cfg(windows)]
#[test]
fn colliding_refs_are_fetched_silently_and_oscillate() {
    let work = tempfile::tempdir().expect("temp dir");
    let src = work.path().join("src");
    std::fs::create_dir_all(&src).expect("mkdir");

    assert!(
        git(&src, &["init", "-q", "-b", "main", "."])
            .status
            .success()
    );
    let commit = |message: &str| {
        let out = Command::new("git")
            .current_dir(&src)
            .args([
                "-c",
                "user.email=a@b",
                "-c",
                "user.name=a",
                "commit",
                "-q",
                "--allow-empty",
                "-m",
                message,
            ])
            .output()
            .expect("git runs");
        assert!(out.status.success(), "{out:?}");
    };
    commit("one");
    let first = String::from_utf8_lossy(&git(&src, &["rev-parse", "HEAD"]).stdout)
        .trim()
        .to_owned();
    commit("two");
    let second = String::from_utf8_lossy(&git(&src, &["rev-parse", "HEAD"]).stdout)
        .trim()
        .to_owned();
    assert_ne!(first, second, "the two tips must differ for a swap to show");

    // `git branch feature` refuses here - the pair only arrives via packed-refs, the
    // way a clone of a case-sensitive filesystem delivers it.
    std::fs::write(
        src.join(".git").join("packed-refs"),
        format!(
            "# pack-refs with: peeled fully-peeled sorted\n\
             {first} refs/heads/Feature\n{second} refs/heads/feature\n{second} refs/heads/main\n"
        ),
    )
    .expect("write packed-refs");
    let _ = std::fs::remove_file(src.join(".git").join("refs").join("heads").join("main"));

    let store = bare_repo();
    let source = src.display().to_string();
    let mut seen = Vec::new();
    for round in 0..4 {
        let out = git(
            store.path(),
            &[
                "fetch",
                "-q",
                "--no-tags",
                &source,
                "+refs/*:refs/tycho/demo/*",
            ],
        );
        assert!(
            out.status.success(),
            "round {round}: git is expected to say nothing at all: {out:?}"
        );
        assert!(
            out.stderr.is_empty(),
            "round {round}: git complained, which it never did when measured: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let listed = git(
            store.path(),
            &[
                "for-each-ref",
                "--format=%(refname)",
                "refs/tycho/demo/heads/",
            ],
        );
        let names = String::from_utf8_lossy(&listed.stdout);
        let held: Vec<&str> = names
            .lines()
            .filter(|line| line.to_lowercase().ends_with("feature"))
            .collect();
        assert_eq!(held.len(), 1, "exactly one of the pair survives: {names}");
        seen.push(held[0].to_owned());
    }

    assert!(
        seen.iter().any(|name| name != &seen[0]),
        "the captured branch is expected to swap between fetches, got {seen:?}"
    );
}
