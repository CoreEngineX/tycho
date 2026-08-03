//! Applying the overlay over a checkout.
//!
//! A plain `cp -R` has two failure modes here, and both are silent or confusing.
//! Where the checkout holds a **symlink** and the overlay a regular file of the same
//! name, `cp` writes **through the link to its target**, creating a file that never
//! existed on the source machine. Where one side is a file and the other a directory,
//! `cp` fails partway with `Not a directory`, having already copied part of the tree.
//!
//! So this refuses both and reports them. A refused file stays in the overlay under
//! `.tycho/repos/<key>/overlay/`, which is why the staging tree is never deleted:
//! it is the only way to settle a conflict by hand afterwards.

use crate::sys::fs::{FileKind, classify_path};
use std::fs;
use std::path::{Path, PathBuf};

/// A file the overlay would not place, and why.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Conflict {
    pub path: PathBuf,
    pub reason: Reason,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reason {
    /// The checkout has a symlink here. Writing the overlay's regular file would go
    /// through the link and fabricate content at its target.
    WouldWriteThroughSymlink,
    /// One side is a file and the other a directory.
    TypeMismatch,
}

impl std::fmt::Display for Reason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.pad(match self {
            Self::WouldWriteThroughSymlink => "a symlink in the checkout has this name",
            Self::TypeMismatch => "one side is a file and the other a directory",
        })
    }
}

#[derive(Debug, Default)]
pub struct Applied {
    pub files: usize,
    pub bytes: u64,
    pub conflicts: Vec<Conflict>,
}

/// Copies every file under `from` into `to`, refusing rather than resolving.
///
/// # Errors
///
/// If a directory cannot be read or a file cannot be written. A **conflict is not an
/// error** - it is reported and the rest of the overlay still lands, because one
/// awkward filename must not cost you the other nine hundred.
pub fn apply(from: &Path, to: &Path) -> std::io::Result<Applied> {
    let mut applied = Applied::default();
    if !from.exists() {
        return Ok(applied);
    }
    walk(from, to, &mut applied)?;
    Ok(applied)
}

fn walk(from: &Path, to: &Path, applied: &mut Applied) -> std::io::Result<()> {
    // Never `metadata`, which follows links: a symlink to a directory would otherwise
    // be walked into and its target's contents copied out.
    let mut entries: Vec<PathBuf> = fs::read_dir(from)?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .collect();
    entries.sort();

    for source in entries {
        let Some(name) = source.file_name() else {
            continue;
        };
        let destination = to.join(name);
        let kind = classify_path(&source)?;

        if let Some(reason) = refuses(&source, &destination, &kind) {
            applied.conflicts.push(Conflict {
                path: destination,
                reason,
            });
            continue;
        }

        match kind {
            FileKind::Directory => {
                fs::create_dir_all(&destination)?;
                walk(&source, &destination, applied)?;
            }
            FileKind::Symlink => {
                let target = fs::read_link(&source)?;
                if destination.symlink_metadata().is_ok() {
                    fs::remove_file(&destination)?;
                }
                if let Some(parent) = destination.parent() {
                    fs::create_dir_all(parent)?;
                }
                std::os::unix::fs::symlink(&target, &destination)?;
                applied.files += 1;
            }
            FileKind::Regular { .. } => {
                if let Some(parent) = destination.parent() {
                    fs::create_dir_all(parent)?;
                }
                let bytes = fs::copy(&source, &destination)?;
                applied.bytes += bytes;
                applied.files += 1;
            }
            FileKind::Skip(_) => {}
        }
    }
    Ok(())
}

/// What is already at the destination decides, and `symlink_metadata` is what asks
/// without following.
fn refuses(source: &Path, destination: &Path, incoming: &FileKind) -> Option<Reason> {
    let Ok(existing) = destination.symlink_metadata() else {
        return None;
    };

    if existing.file_type().is_symlink() && !matches!(incoming, FileKind::Symlink) {
        return Some(Reason::WouldWriteThroughSymlink);
    }
    let existing_is_dir = existing.is_dir();
    let incoming_is_dir = matches!(incoming, FileKind::Directory);
    if existing_is_dir != incoming_is_dir {
        return Some(Reason::TypeMismatch);
    }
    // Two regular files, or two directories, or two symlinks: the overlay wins,
    // because it is what was on disk at backup time.
    let _ = source;
    None
}

#[cfg(test)]
mod tests {
    use super::{Reason, apply};
    use std::fs;
    use std::path::Path;

    fn write(path: &Path, text: &str) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("mkdir");
        }
        fs::write(path, text).expect("write");
    }

    /// The failure `cp -R` produces silently: the link's target gets the content and a
    /// file appears where none existed on the source machine.
    #[test]
    fn a_symlink_in_the_checkout_is_never_written_through() {
        let dir = tempfile::tempdir().expect("temp dir");
        let (from, to) = (dir.path().join("overlay"), dir.path().join("checkout"));
        write(&from.join("config"), "the overlay's regular file\n");

        let target = dir.path().join("elsewhere.txt");
        write(&target, "untouched\n");
        fs::create_dir_all(&to).expect("mkdir");
        std::os::unix::fs::symlink(&target, to.join("config")).expect("symlink");

        let applied = apply(&from, &to).expect("apply");
        assert_eq!(applied.conflicts.len(), 1);
        assert_eq!(
            applied.conflicts[0].reason,
            Reason::WouldWriteThroughSymlink
        );
        assert_eq!(
            fs::read_to_string(&target).expect("read"),
            "untouched\n",
            "the link's target must not have been written"
        );
    }

    /// `cp` fails partway here, having already copied part of the tree.
    #[test]
    fn a_file_meeting_a_directory_is_refused_and_the_rest_still_lands() {
        let dir = tempfile::tempdir().expect("temp dir");
        let (from, to) = (dir.path().join("overlay"), dir.path().join("checkout"));
        write(&from.join("thing"), "the overlay has a file here\n");
        write(&from.join("fine.txt"), "and an ordinary one\n");
        fs::create_dir_all(to.join("thing")).expect("the checkout has a directory");

        let applied = apply(&from, &to).expect("apply");
        assert_eq!(applied.conflicts.len(), 1);
        assert_eq!(applied.conflicts[0].reason, Reason::TypeMismatch);
        assert!(to.join("thing").is_dir(), "the directory survives");
        assert_eq!(
            fs::read_to_string(to.join("fine.txt")).expect("read"),
            "and an ordinary one\n",
            "one awkward name must not cost the rest of the overlay"
        );
        assert_eq!(applied.files, 1);
    }

    #[test]
    fn an_overlay_symlink_is_recreated_as_a_symlink() {
        let dir = tempfile::tempdir().expect("temp dir");
        let (from, to) = (dir.path().join("overlay"), dir.path().join("checkout"));
        fs::create_dir_all(&from).expect("mkdir");
        std::os::unix::fs::symlink("../target", from.join("link")).expect("symlink");

        apply(&from, &to).expect("apply");
        assert_eq!(
            fs::read_link(to.join("link")).expect("read_link"),
            Path::new("../target")
        );
    }

    /// A symlink to a directory must not be walked into, or the copy fabricates the
    /// target's contents at a path that never held them.
    #[test]
    fn a_symlink_to_a_directory_is_copied_rather_than_followed() {
        let dir = tempfile::tempdir().expect("temp dir");
        let (from, to) = (dir.path().join("overlay"), dir.path().join("checkout"));
        write(
            &dir.path().join("real/inside.txt"),
            "should not be copied\n",
        );
        fs::create_dir_all(&from).expect("mkdir");
        std::os::unix::fs::symlink(dir.path().join("real"), from.join("link")).expect("symlink");

        let applied = apply(&from, &to).expect("apply");
        assert!(
            to.join("link")
                .symlink_metadata()
                .expect("stat")
                .is_symlink()
        );
        assert_eq!(
            applied.files, 1,
            "walking the link would have copied the target's contents too"
        );
        // The path `to/link/inside.txt` resolves *through* the link, so its existence
        // proves nothing. What proves it is that nothing was written into the target.
        let target = fs::read_dir(dir.path().join("real"))
            .expect("listing")
            .count();
        assert_eq!(target, 1, "the link's target must not have been written to");
    }

    /// The overlay is what was on disk at backup time, so where both sides are the
    /// same kind it wins.
    #[test]
    fn the_overlay_replaces_a_regular_file_of_the_same_name() {
        let dir = tempfile::tempdir().expect("temp dir");
        let (from, to) = (dir.path().join("overlay"), dir.path().join("checkout"));
        write(&from.join("notes.md"), "edited, never committed\n");
        write(&to.join("notes.md"), "the committed version\n");

        let applied = apply(&from, &to).expect("apply");
        assert!(applied.conflicts.is_empty());
        assert_eq!(
            fs::read_to_string(to.join("notes.md")).expect("read"),
            "edited, never committed\n"
        );
    }

    #[test]
    fn a_missing_overlay_is_nothing_to_do_rather_than_an_error() {
        let dir = tempfile::tempdir().expect("temp dir");
        let applied = apply(&dir.path().join("no-overlay"), dir.path()).expect("apply");
        assert_eq!(applied.files, 0);
    }
}
