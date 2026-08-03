//! Why `sys::process` strips git's environment before spawning.
//!
//! The runner cannot be handed an environment by its caller, so its immunity is
//! asserted as a unit test on the constructed command. This file establishes the
//! other half: that the hazard the stripping prevents is real, and would still be
//! real if nobody had thought about it.

use std::path::Path;
use std::process::Command;
use tempfile::TempDir;

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

fn has_object(repo: &Path, oid: &str) -> bool {
    Command::new("git")
        .current_dir(repo)
        .args(["cat-file", "-e", oid])
        .output()
        .expect("git runs")
        .status
        .success()
}

/// The identity variables outrank `-c user.name` and `-c user.email`, so an
/// inherited pair would author a backup commit as whoever the surrounding hook was
/// running for rather than as tycho.
#[test]
fn the_identity_variables_outrank_the_config_pins() {
    let store = bare_repo();
    let tree = Command::new("git")
        .current_dir(store.path())
        .args(["hash-object", "-w", "-t", "tree", "--stdin"])
        .stdin(std::process::Stdio::null())
        .output()
        .expect("git ran");
    let tree = String::from_utf8_lossy(&tree.stdout).trim().to_owned();

    let commit = |extra: &[(&str, &str)]| {
        let mut command = Command::new("git");
        command
            .current_dir(store.path())
            .args(["-c", "user.name=tycho", "-c", "user.email=tycho@localhost"])
            .args(["commit-tree", &tree, "-m", "x"]);
        for (key, value) in extra {
            command.env(key, value);
        }
        let out = command.output().expect("git ran");
        assert!(out.status.success(), "commit-tree: {out:?}");
        let oid = String::from_utf8_lossy(&out.stdout).trim().to_owned();
        let shown = Command::new("git")
            .current_dir(store.path())
            .args(["log", "-1", "--format=%an <%ae>", &oid])
            .output()
            .expect("git ran");
        String::from_utf8_lossy(&shown.stdout).trim().to_owned()
    };

    assert_eq!(commit(&[]), "tycho <tycho@localhost>");
    assert_eq!(
        commit(&[
            ("GIT_AUTHOR_NAME", "hooked"),
            ("GIT_AUTHOR_EMAIL", "hooked@example.com"),
        ]),
        "hooked <hooked@example.com>",
        "the pins won, so stripping the identity variables guards nothing"
    );
}

/// `-C` changes directory; `GIT_DIR` overrides repository discovery. An inherited
/// one therefore silently redirects a write to the wrong repository, at exit 0.
#[test]
fn git_dir_overrides_the_directory_c_names() {
    let store = bare_repo();
    let decoy = bare_repo();

    let out = Command::new("git")
        .env("GIT_DIR", decoy.path())
        .arg("-C")
        .arg(store.path())
        .args(["hash-object", "-w", "-t", "blob", "--stdin"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .and_then(|mut child| {
            use std::io::Write;
            if let Some(mut stdin) = child.stdin.take() {
                stdin.write_all(b"redirected\n")?;
            }
            child.wait_with_output()
        })
        .expect("git ran");

    assert!(
        out.status.success(),
        "the write succeeded, which is what makes this dangerous"
    );
    let oid = String::from_utf8_lossy(&out.stdout).trim().to_owned();

    assert!(
        has_object(decoy.path(), &oid),
        "GIT_DIR did not win, so the stripping guards a hazard that no longer exists"
    );
    assert!(
        !has_object(store.path(), &oid),
        "the object reached the repository -C named, contradicting the above"
    );
}
