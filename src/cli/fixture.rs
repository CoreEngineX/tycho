//! Config files for the command tests.
//!
//! One builder rather than one per module. Three CI failures on Windows were the same
//! fixture written twice and fixed once: a `watch` root hardcoded to `/tmp`, which is
//! not a path there, so the re-validation every write runs called it an error and the
//! test read that as the command failing.
//!
//! Two rules the paths here have to satisfy, both learned from a red run:
//!
//! - **Quoted as TOML literal strings**, so a `C:\Users\...` path needs no escaping.
//! - **The watched root's own name becomes a git refname**, so it cannot be
//!   `tempfile`'s `.tmpXXXX` - git rejects a leading dot. Hence a named subdirectory.

use std::path::{Path, PathBuf};

pub(crate) struct Fixture {
    dir: tempfile::TempDir,
    pub path: PathBuf,
}

impl Fixture {
    pub(crate) fn new() -> Self {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("tycho.toml");
        std::fs::write(&path, "version = 1\n").expect("write");
        Self { dir, path }
    }

    /// A directory that exists, under a name a refname accepts.
    pub(crate) fn dir(&self, name: &str) -> PathBuf {
        let root = self.dir.path().join(name);
        std::fs::create_dir_all(&root).expect("mkdir");
        root
    }

    /// A `[[profile]]` table watching a real directory, with `body` pasted in.
    pub(crate) fn profile(&self, name: &str, body: &str) -> String {
        format!(
            "\n[[profile]]\nname = \"{name}\"\nwatch = [{}]\n{body}",
            literal(&self.dir(&format!("watched-{name}")))
        )
    }

    pub(crate) fn append(&self, toml: &str) {
        let mut text = std::fs::read_to_string(&self.path).expect("read");
        text.push_str(toml);
        std::fs::write(&self.path, text).expect("write");
    }

    pub(crate) fn text(&self) -> String {
        std::fs::read_to_string(&self.path).expect("read back")
    }
}

/// A path as a TOML literal string, which processes no escapes.
pub(crate) fn literal(path: &Path) -> String {
    format!("'{}'", path.display())
}

#[cfg(test)]
mod tests {
    /// The regression itself, pinned rather than remembered.
    ///
    /// A `watch` root written as a POSIX absolute path passes on the machine that
    /// wrote it and fails every Windows run. Twice this reached CI, so it is a test
    /// and not a note.
    #[test]
    fn no_command_test_hardcodes_a_posix_path_as_a_watched_root() {
        let mut offenders = Vec::new();
        for file in ["profile.rs", "remote.rs", "schedule.rs", "fixture.rs"] {
            let path = format!("{}/src/cli/{file}", env!("CARGO_MANIFEST_DIR"));
            let Ok(text) = std::fs::read_to_string(&path) else {
                continue;
            };
            for (number, line) in text.lines().enumerate() {
                let trimmed = line.trim_start();
                if trimmed.starts_with("watch = [\"/") || trimmed.starts_with("watch = ['/") {
                    offenders.push(format!("{file}:{}: {trimmed}", number + 1));
                }
            }
        }
        assert!(
            offenders.is_empty(),
            "a watched root must come from Fixture::dir, not a literal path:\n  {}",
            offenders.join("\n  ")
        );
    }
}
