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
    /// Windows refused the link: creating one needs `SeCreateSymbolicLinkPrivilege`,
    /// which a normal account holds only under Developer Mode or elevation.
    ///
    /// Reported rather than written as a regular file holding the target path. That
    /// substitution is what Git for Windows does under `core.symlinks=false`, and it
    /// is the one thing this module exists to refuse: it puts a file where a link
    /// belonged, and the next capture would store it as one.
    SymlinkNotPermitted,
}

impl std::fmt::Display for Reason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.pad(match self {
            Self::WouldWriteThroughSymlink => "a symlink in the checkout has this name",
            Self::TypeMismatch => "one side is a file and the other a directory",
            Self::SymlinkNotPermitted => {
                "creating a symlink needs Developer Mode or an elevated process"
            }
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

        if let Some(reason) = refuses(&destination, &kind) {
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
                    remove_existing(&destination)?;
                }
                if let Some(parent) = destination.parent() {
                    fs::create_dir_all(parent)?;
                }
                match symlink(&source, &target, &destination) {
                    Ok(()) => applied.files += 1,
                    Err(error) if refused_for_privilege(&error) => {
                        applied.conflicts.push(Conflict {
                            path: destination,
                            reason: Reason::SymlinkNotPermitted,
                        });
                    }
                    Err(error) => return Err(error),
                }
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

/// Removes what is at the destination without touching what a link points at.
#[cfg(unix)]
fn remove_existing(destination: &Path) -> std::io::Result<()> {
    fs::remove_file(destination)
}

/// A directory symlink needs `remove_dir` here; `remove_file` refuses it.
#[cfg(windows)]
fn remove_existing(destination: &Path) -> std::io::Result<()> {
    use std::os::windows::fs::FileTypeExt;

    if destination.symlink_metadata()?.file_type().is_symlink_dir() {
        return fs::remove_dir(destination);
    }
    fs::remove_file(destination)
}

#[cfg(unix)]
fn symlink(_source: &Path, target: &Path, destination: &Path) -> std::io::Result<()> {
    std::os::unix::fs::symlink(target, destination)
}

/// Windows needs to be told at creation time whether the link names a directory, and
/// a dangling link cannot be probed for it - so the answer comes from what the source
/// link itself is, which is the only reading that survives a target that is not there.
#[cfg(windows)]
fn symlink(source: &Path, target: &Path, destination: &Path) -> std::io::Result<()> {
    use std::os::windows::fs::FileTypeExt;

    if source.symlink_metadata()?.file_type().is_symlink_dir() {
        std::os::windows::fs::symlink_dir(target, destination)
    } else {
        std::os::windows::fs::symlink_file(target, destination)
    }
}

/// Nothing here needs a privilege, so no error is that error.
#[cfg(unix)]
fn refused_for_privilege(_error: &std::io::Error) -> bool {
    false
}

/// `ERROR_PRIVILEGE_NOT_HELD`. Matched on the raw code because `io::ErrorKind` maps
/// it to `Uncategorized`, which is measured rather than assumed.
#[cfg(windows)]
fn refused_for_privilege(error: &std::io::Error) -> bool {
    error.raw_os_error() == Some(1314)
}

/// What is already at the destination decides, and `symlink_metadata` is what asks
/// without following.
fn refuses(destination: &Path, incoming: &FileKind) -> Option<Reason> {
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

    /// A junction, because `SeCreateSymbolicLinkPrivilege` is not held by a normal
    /// account and a junction is the link a Windows user has without it. `std`
    /// reports one as `is_symlink`, which is what these tests turn on.
    #[cfg(windows)]
    fn link_to_dir(link: &Path, target: &Path) {
        use crate::sys::process::{Timeout, command};

        let made = command(
            "cmd",
            &[
                "/c",
                "mklink",
                "/J",
                link.to_str().expect("temp paths are utf-8"),
                target.to_str().expect("temp paths are utf-8"),
            ],
            Timeout::QUICK,
        )
        .expect("mklink runs");
        assert!(
            made.status.success(),
            "mklink /J: {}",
            String::from_utf8_lossy(&made.stdout)
        );
    }

    #[cfg(unix)]
    fn link_to_dir(link: &Path, target: &Path) {
        std::os::unix::fs::symlink(target, link).expect("symlink");
    }

    /// The failure `cp -R` produces silently: the link's target gets the content and a
    /// file appears where none existed on the source machine.
    #[cfg(unix)]
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

    #[cfg(unix)]
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

    /// Whether an account may create a symlink is a property of the machine -
    /// `SeCreateSymbolicLinkPrivilege`, or Developer Mode, which CI runners have and
    /// stock workstations do not. So both outcomes are legitimate here and this pins
    /// the pair that is not: a silent skip, or a regular file holding the target path
    /// where a link belonged.
    #[cfg(windows)]
    #[test]
    fn a_link_is_either_recreated_as_a_link_or_reported_never_silently_skipped() {
        let dir = tempfile::tempdir().expect("temp dir");
        let (from, to) = (dir.path().join("overlay"), dir.path().join("checkout"));
        let target = dir.path().join("real");
        fs::create_dir_all(&target).expect("mkdir");
        fs::create_dir_all(&from).expect("mkdir");
        link_to_dir(&from.join("link"), &target);

        let applied = apply(&from, &to).expect("apply");
        if let Ok(written) = to.join("link").symlink_metadata() {
            assert!(
                written.file_type().is_symlink(),
                "a link may not land as a regular file: {applied:?}"
            );
            assert!(applied.conflicts.is_empty(), "{applied:?}");
        } else {
            assert_eq!(
                applied.conflicts.len(),
                1,
                "a refused link must be reported: {applied:?}"
            );
            assert_eq!(
                applied.conflicts[0].reason,
                Reason::SymlinkNotPermitted,
                "{applied:?}"
            );
            assert_eq!(applied.files, 0);
        }
    }

    /// The predicate the refusal path turns on. The test above cannot reach it on a
    /// machine that may create symlinks, and that machine is the one running CI.
    #[cfg(windows)]
    #[test]
    fn only_the_privilege_error_counts_as_a_refusal() {
        use super::refused_for_privilege;
        use std::io::Error;
        assert!(refused_for_privilege(&Error::from_raw_os_error(1314)));
        assert!(!refused_for_privilege(&Error::from_raw_os_error(5)));
        assert!(!refused_for_privilege(&Error::other("unrelated")));
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
        link_to_dir(&from.join("link"), &dir.path().join("real"));

        let applied = apply(&from, &to).expect("apply");
        // Windows refuses to recreate the link, which is its own reported outcome.
        // What both platforms must agree on is that the target was never walked.
        assert_eq!(
            applied.files + applied.conflicts.len(),
            1,
            "walking the link would have copied the target's contents too: {applied:?}"
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
