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
    /// A config path written as a POSIX absolute literal passes on the machine that
    /// wrote it and fails every Windows run, because `AbsPath` drops what the host
    /// cannot hold and the assertion then blames the code rather than the fixture.
    /// Three separate CI failures were this, in three different files, so the check
    /// is over the whole tree rather than the files that had already gone wrong.
    ///
    /// A path inside a **string being displayed** is fine - only one that gets parsed
    /// matters - so this looks for the two keys that are read back as paths.
    #[test]
    fn no_fixture_hardcodes_a_posix_path_where_one_is_parsed() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut offenders = Vec::new();
        walk(&root, &mut |file: &std::path::Path, text: &str| {
            for (number, line) in text.lines().enumerate() {
                let trimmed = line.trim_start();
                let parsed_key = trimmed.starts_with("watch = [\"/")
                    || trimmed.starts_with("watch = ['/")
                    || trimmed.starts_with("path = \"/")
                    || trimmed.starts_with("path: \"/")
                    || (trimmed.contains("new_remote(") && trimmed.contains(", \"/"));
                if parsed_key {
                    offenders.push(format!(
                        "{}:{}: {trimmed}",
                        file.strip_prefix(&root).unwrap_or(file).display(),
                        number + 1
                    ));
                }
            }
        });
        assert!(
            offenders.is_empty(),
            "a parsed path must come from Fixture::dir or a #[cfg]-selected constant, \
             never a POSIX literal:\n  {}",
            offenders.join("\n  ")
        );
    }

    fn walk(dir: &std::path::Path, visit: &mut impl FnMut(&std::path::Path, &str)) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                walk(&path, visit);
            } else if path.extension().is_some_and(|ext| ext == "rs")
                && let Ok(text) = std::fs::read_to_string(&path)
            {
                visit(&path, &text);
            }
        }
    }
}
