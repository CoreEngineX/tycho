//! Atomic writes, and the exhaustive classification of what a directory entry is.

use crate::primitives::encode::FileMode;
use std::ffi::OsString;
use std::fs::{self, File, Metadata};
use std::io::{self, Write};
use std::os::unix::fs::{FileTypeExt, MetadataExt};
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SkipReason {
    Socket,
    Fifo,
    BlockDevice,
    CharDevice,
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
#[must_use]
pub fn classify(metadata: &Metadata) -> FileKind {
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
    let mut file = File::create(temp)?;
    file.write_all(bytes)?;
    file.sync_all()
}

#[cfg(test)]
mod tests {
    use super::{FileKind, SkipReason, classify_path, write_atomic};
    use crate::primitives::encode::FileMode;
    use crate::sys::process::{Timeout, command};
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use std::path::Path;

    fn write(path: &Path, text: &str) {
        fs::write(path, text).expect("write fixture");
    }

    #[test]
    fn a_regular_file_carries_only_the_owner_execute_bit() {
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

    #[test]
    fn a_directory_is_its_own_kind() {
        let dir = tempfile::tempdir().expect("temp dir");
        assert_eq!(
            classify_path(dir.path()).expect("classified"),
            FileKind::Directory
        );
    }

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
        assert_eq!(FileKind::Skip(SkipReason::Fifo).mode(), None);
        assert_eq!(FileKind::Skip(SkipReason::Socket).mode(), None);
        assert_eq!(FileKind::Skip(SkipReason::BlockDevice).mode(), None);
        assert_eq!(FileKind::Skip(SkipReason::Unknown(0)).mode(), None);
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
