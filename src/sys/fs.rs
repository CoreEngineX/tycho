//! Atomic writes, and the exhaustive classification of what a directory entry is.

use crate::primitives::encode::FileMode;
use std::ffi::OsString;
use std::fs::{self, File, Metadata};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

/// Every kind an entry can be, with no implicit remainder. The remainder is what
/// blocks `hash-object` forever: it reads a FIFO or `/dev/zero` and never returns.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FileKind {
    Regular { executable: bool },
    Symlink,
    Directory,
    Skip(SkipReason),
}

/// Windows carries none of the four named kinds because nothing in `std` can tell
/// them apart there: a WSL-created FIFO or socket on NTFS is a reparse point, and the
/// tag that says which is not exposed by `std::os::windows::fs`. They fall into
/// `Unknown`, which is what keeps them out of `hash-object` all the same.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SkipReason {
    #[cfg(unix)]
    Socket,
    #[cfg(unix)]
    Fifo,
    #[cfg(unix)]
    BlockDevice,
    #[cfg(unix)]
    CharDevice,
    /// The bits that matched nothing: `st_mode` on Unix, the file attributes on
    /// Windows.
    Unknown(u32),
}

impl FileKind {
    /// The git tree mode this stores as, or `None` for entries that never become a
    /// tree entry of their own.
    #[must_use]
    pub const fn mode(self) -> Option<FileMode> {
        match self {
            Self::Regular { executable: false } => Some(FileMode::Regular),
            Self::Regular { executable: true } => Some(FileMode::Executable),
            Self::Symlink => Some(FileMode::Symlink),
            Self::Directory | Self::Skip(_) => None,
        }
    }
}

/// Classifies already-read metadata.
#[cfg(unix)]
#[must_use]
pub fn classify(metadata: &Metadata) -> FileKind {
    use std::os::unix::fs::{FileTypeExt, MetadataExt};

    let kind = metadata.file_type();
    if kind.is_symlink() {
        FileKind::Symlink
    } else if kind.is_dir() {
        FileKind::Directory
    } else if kind.is_file() {
        FileKind::Regular {
            executable: metadata.mode() & 0o100 != 0,
        }
    } else if kind.is_socket() {
        FileKind::Skip(SkipReason::Socket)
    } else if kind.is_fifo() {
        FileKind::Skip(SkipReason::Fifo)
    } else if kind.is_block_device() {
        FileKind::Skip(SkipReason::BlockDevice)
    } else if kind.is_char_device() {
        FileKind::Skip(SkipReason::CharDevice)
    } else {
        FileKind::Skip(SkipReason::Unknown(metadata.mode()))
    }
}

/// Classifies already-read metadata.
///
/// **Nothing on NTFS is executable**, so every regular file stores as `100644`. That
/// is not a shortcut: there is no permission bit to read, and Git for Windows makes
/// the same call by defaulting `core.fileMode` to false. A `100755` blob captured on
/// macOS still restores as `100755` in the tree - what is lost is the bit on the
/// restored file, which `store.md` section 7 already counts as metadata.
///
/// A junction reports `is_symlink`, so it classifies as [`FileKind::Symlink`] and is
/// stored as its target rather than descended into. Measured, not assumed: `mklink /J`
/// needs no privilege, so junctions are the link a Windows user actually has.
#[cfg(windows)]
#[must_use]
pub fn classify(metadata: &Metadata) -> FileKind {
    use std::os::windows::fs::MetadataExt;

    let kind = metadata.file_type();
    if kind.is_symlink() {
        FileKind::Symlink
    } else if kind.is_dir() {
        FileKind::Directory
    } else if kind.is_file() {
        FileKind::Regular { executable: false }
    } else {
        FileKind::Skip(SkipReason::Unknown(metadata.file_attributes()))
    }
}

/// Classifies a path without following a symlink, so a link loop is a `Symlink`
/// rather than an infinite descent and its target is never read.
///
/// # Errors
///
/// If the entry's metadata cannot be read.
pub fn classify_path(path: &Path) -> io::Result<FileKind> {
    fs::symlink_metadata(path).map(|metadata| classify(&metadata))
}

/// Writes to a sibling temporary file, flushes it to disk, then renames over the
/// target, so an interrupted write leaves the previous contents intact.
///
/// The rename itself is not fsynced, so a power loss immediately after it can lose
/// the new contents - but never truncate or interleave them.
///
/// # Errors
///
/// If the temporary file cannot be written or the rename fails. The temporary file
/// is removed on either.
pub fn write_atomic(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let mut name = OsString::from(path.as_os_str());
    name.push(format!(".tmp.{}", std::process::id()));
    let temp = PathBuf::from(name);

    let result = write_and_sync(&temp, bytes).and_then(|()| fs::rename(&temp, path));
    if result.is_err() {
        let _ = fs::remove_file(&temp);
    }
    result
}

fn write_and_sync(temp: &Path, bytes: &[u8]) -> io::Result<()> {
    // `create_new`, because a remote folder is a directory other people can write to:
    // `File::create` follows a symlink planted at the predictable temporary name and
    // writes this content through it. The unlink first clears what a killed run left
    // behind - and unlinking a symlink removes the link, never its target - so the
    // exclusive create can still only lose the race by failing, not by redirecting.
    let _ = fs::remove_file(temp);
    let mut file = File::options().write(true).create_new(true).open(temp)?;
    file.write_all(bytes)?;
    file.sync_all()
}

#[cfg(test)]
mod tests {
    use super::{FileKind, SkipReason, classify_path, write_atomic};
    use crate::primitives::encode::FileMode;
    use crate::sys::process::{Timeout, command};
    use std::fs;
    use std::path::Path;

    fn write(path: &Path, text: &str) {
        fs::write(path, text).expect("write fixture");
    }

    #[cfg(unix)]
    #[test]
    fn a_regular_file_carries_only_the_owner_execute_bit() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().expect("temp dir");
        let plain = dir.path().join("plain.md");
        let runnable = dir.path().join("run.sh");
        write(&plain, "x");
        write(&runnable, "x");
        fs::set_permissions(&runnable, PermissionsExt::from_mode(0o755)).expect("chmod");

        assert_eq!(
            classify_path(&plain).expect("classified"),
            FileKind::Regular { executable: false }
        );
        assert_eq!(
            classify_path(&runnable).expect("classified"),
            FileKind::Regular { executable: true }
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_symlink_is_never_followed() {
        let dir = tempfile::tempdir().expect("temp dir");
        let target = dir.path().join("target.md");
        write(&target, "x");

        let good = dir.path().join("good");
        let dangling = dir.path().join("dangling");
        let looped = dir.path().join("looped");
        std::os::unix::fs::symlink(&target, &good).expect("symlink");
        std::os::unix::fs::symlink("nowhere", &dangling).expect("symlink");
        std::os::unix::fs::symlink("looped", &looped).expect("symlink");

        for link in [&good, &dangling, &looped] {
            assert_eq!(
                classify_path(link).expect("classified"),
                FileKind::Symlink,
                "{}",
                link.display()
            );
        }
    }

    /// A junction, not a symlink: creating a symlink needs a privilege this process
    /// does not hold by default, and a junction is what a Windows user has without
    /// one. Classifying it as a link is what stops the walk descending into it and
    /// storing the target's contents at a path that never held them.
    #[cfg(windows)]
    #[test]
    fn a_junction_is_a_link_rather_than_a_directory_to_descend() {
        let dir = tempfile::tempdir().expect("temp dir");
        let target = dir.path().join("target");
        fs::create_dir_all(&target).expect("mkdir");
        write(&target.join("inside.md"), "x");

        let junction = dir.path().join("junction");
        let made = command(
            "cmd",
            &[
                "/c",
                "mklink",
                "/J",
                junction.to_str().expect("temp paths are utf-8"),
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

        assert_eq!(
            classify_path(&junction).expect("classified"),
            FileKind::Symlink
        );
    }

    #[test]
    fn a_directory_is_its_own_kind() {
        let dir = tempfile::tempdir().expect("temp dir");
        assert_eq!(
            classify_path(dir.path()).expect("classified"),
            FileKind::Directory
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_fifo_is_skipped_rather_than_read() {
        let dir = tempfile::tempdir().expect("temp dir");
        let fifo = dir.path().join("pipe");
        let made = command(
            "mkfifo",
            &[fifo.to_str().expect("temp paths are utf-8")],
            Timeout::QUICK,
        )
        .expect("mkfifo runs");
        assert!(made.status.success(), "mkfifo: {made:?}");

        assert_eq!(
            classify_path(&fifo).expect("classified"),
            FileKind::Skip(SkipReason::Fifo)
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_character_device_is_skipped() {
        assert_eq!(
            classify_path(Path::new("/dev/zero")).expect("classified"),
            FileKind::Skip(SkipReason::CharDevice)
        );
    }

    #[test]
    fn only_the_storable_kinds_have_a_mode() {
        assert_eq!(
            FileKind::Regular { executable: false }.mode(),
            Some(FileMode::Regular)
        );
        assert_eq!(
            FileKind::Regular { executable: true }.mode(),
            Some(FileMode::Executable)
        );
        assert_eq!(FileKind::Symlink.mode(), Some(FileMode::Symlink));
        assert_eq!(FileKind::Directory.mode(), None);
        assert_eq!(FileKind::Skip(SkipReason::Unknown(0)).mode(), None);
        #[cfg(unix)]
        {
            assert_eq!(FileKind::Skip(SkipReason::Fifo).mode(), None);
            assert_eq!(FileKind::Skip(SkipReason::Socket).mode(), None);
            assert_eq!(FileKind::Skip(SkipReason::BlockDevice).mode(), None);
        }
    }

    #[test]
    fn an_atomic_write_replaces_the_previous_contents() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("state.json");
        write_atomic(&path, b"first").expect("write");
        assert_eq!(fs::read(&path).expect("read"), b"first");
        write_atomic(&path, b"second").expect("write");
        assert_eq!(fs::read(&path).expect("read"), b"second");
    }

    #[test]
    fn a_failed_write_leaves_the_previous_file_and_no_temporary() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("nested").join("state.json");
        assert!(write_atomic(&path, b"never").is_err());
        assert!(!dir.path().join("nested").exists());
        assert_eq!(
            fs::read_dir(dir.path()).expect("read dir").count(),
            0,
            "a temporary file was left behind"
        );
    }
}
