//! Layer 1 exists so that nothing above it spawns a process directly. In a single
//! crate that rule has no compiler behind it, so it has this instead.

use std::fs;
use std::path::{Path, PathBuf};

const DOOR: &str = "sys/process.rs";

fn rust_files(dir: &Path, found: &mut Vec<PathBuf>) {
    for entry in fs::read_dir(dir).expect("src is readable") {
        let path = entry.expect("a directory entry").path();
        if path.is_dir() {
            rust_files(&path, found);
        } else if path.extension().is_some_and(|extension| extension == "rs") {
            found.push(path);
        }
    }
}

#[test]
fn only_sys_process_spawns_a_child() {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = Vec::new();
    rust_files(&src, &mut files);
    assert!(files.len() > 5, "the source tree was not found");

    let offenders: Vec<String> = files
        .iter()
        .filter(|path| !path.ends_with(DOOR))
        .filter(|path| {
            fs::read_to_string(path)
                .expect("a source file")
                .contains("Command::new")
        })
        .map(|path| {
            path.strip_prefix(&src)
                .unwrap_or(path)
                .display()
                .to_string()
        })
        .collect();

    assert!(
        offenders.is_empty(),
        "these spawn a child outside {DOOR}, bypassing the config pins and the \
         timeout: {offenders:?}"
    );
}

/// The pins only hold if they are on the invocation, so the door must actually
/// carry them.
#[test]
fn the_door_pins_what_it_promises() {
    let door = Path::new(env!("CARGO_MANIFEST_DIR")).join("src").join(DOOR);
    let text = fs::read_to_string(&door).expect("the runner source");
    for pin in [
        "--no-optional-locks",
        "core.autocrlf=false",
        "core.eol=lf",
        "core.attributesFile=/dev/null",
        "core.quotePath=false",
        "user.name=tycho",
        "user.email=tycho@localhost",
        "GIT_DIR",
        "GIT_INDEX_FILE",
        "GIT_TERMINAL_PROMPT",
    ] {
        assert!(text.contains(pin), "{DOOR} no longer mentions {pin}");
    }
}
