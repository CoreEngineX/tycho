//! The batch pipeline wedges if stdin is written before stdout is read.
//!
//! `store.md` measured it at roughly 2,300 files with 145-byte paths, and the
//! threshold scales with path length - so every other fixture in this codebase is
//! far too small to reach it. That is exactly why the bug survived design review.

use std::path::Path;
use std::process::Command;
use tempfile::TempDir;
use tycho::primitives::encode::stdin_paths_line;
use tycho::primitives::oid::Oid;
use tycho::sys::process::{Git, Timeout};

const FILES: usize = 5_001;

fn bare_repo() -> TempDir {
    let dir = tempfile::tempdir().expect("temp dir");
    let out = Command::new("git")
        .current_dir(dir.path())
        .args(["init", "--bare", "--object-format=sha1", "."])
        .output()
        .expect("git is a hard requirement of this project");
    assert!(out.status.success(), "git init: {out:?}");
    dir
}

#[test]
fn a_batch_past_the_pipe_buffer_does_not_deadlock() {
    let work = tempfile::tempdir().expect("temp dir");
    let store = bare_repo();

    // Long names, because the wedge threshold is measured in bytes rather than
    // entries.
    let padding = "d".repeat(96);
    let mut paths = Vec::with_capacity(FILES);
    for index in 0..FILES {
        let path = work.path().join(format!("{padding}-{index:06}.txt"));
        std::fs::write(&path, format!("{index}\n")).expect("write fixture");
        paths.push(path);
    }
    assert!(
        total_bytes(&paths) > 512 * 1024,
        "the fixture must exceed any plausible pipe buffer"
    );

    let out = Git::at(store.path())
        .stream(
            &["hash-object", "-w", "--no-filters", "--stdin-paths"],
            paths.iter().map(|path| stdin_paths_line(path)),
            Timeout::WORK,
        )
        .expect("the batch runs to completion");
    assert!(
        out.status.success(),
        "hash-object failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let oids: Vec<Oid> = String::from_utf8(out.stdout)
        .expect("hash-object prints hex")
        .lines()
        .map(|line| Oid::parse(line).expect("a valid object id"))
        .collect();
    assert_eq!(oids.len(), FILES, "one hash per path");
    assert_eq!(
        oids.iter().collect::<std::collections::BTreeSet<_>>().len(),
        FILES,
        "each file has distinct content, so each hash must differ"
    );
}

fn total_bytes(paths: &[std::path::PathBuf]) -> usize {
    paths.iter().map(|path| stdin_paths_line(path).len()).sum()
}

/// A watched tree can outlive a single fetch, so the streaming helper must also
/// cope with a command that reads nothing from stdin while it writes.
#[test]
fn a_command_that_ignores_stdin_still_completes() {
    let store = bare_repo();
    let noise: Vec<Vec<u8>> = (0..20_000).map(|n| format!("{n}\n").into_bytes()).collect();
    let out = Git::at(store.path())
        .stream(
            &["rev-parse", "--git-dir"],
            noise.into_iter(),
            Timeout::QUICK,
        )
        .expect("git ran despite unread stdin");
    assert!(out.status.success());
    assert_eq!(
        String::from_utf8_lossy(&out.stdout).trim(),
        Path::new(".").display().to_string()
    );
}
