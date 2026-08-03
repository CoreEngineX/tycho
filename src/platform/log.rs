//! The log file at `~/Library/Logs/tycho/<profile>.log`, rotated by size.
//!
//! Separate from launchd's `.out.log` and `.err.log`, which capture whatever the
//! process printed before Tycho's own logging could start - a crash in argument
//! parsing still leaves a trace there. This is the file `tycho log` tails.

use crate::config::LogLevel;
use crate::platform::log_dir;
use crate::primitives::path::PathError;
use std::fmt::Write as _;
use std::io::Write as _;
use std::path::PathBuf;

/// Rotate at 4 MB, keeping one previous file.
///
/// Big enough that a year of weekly runs never rotates, small enough that a run
/// looping on a per-file warning cannot fill a disk. One generation, because the
/// store's own commit history is the durable record - this file is for diagnosing the
/// last thing that went wrong, not for archaeology.
pub const ROTATE_AT: u64 = 4 * 1024 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Level {
    Error,
    Warn,
    Info,
    Debug,
}

impl Level {
    const fn word(self) -> &'static str {
        match self {
            Self::Error => "error",
            Self::Warn => "warn",
            Self::Info => "info",
            Self::Debug => "debug",
        }
    }

    #[must_use]
    pub const fn from_config(level: LogLevel) -> Self {
        match level {
            LogLevel::Error => Self::Error,
            LogLevel::Warn => Self::Warn,
            LogLevel::Info => Self::Info,
            LogLevel::Debug => Self::Debug,
        }
    }
}

/// One profile's log.
#[derive(Debug)]
pub struct Log {
    path: PathBuf,
    threshold: Level,
}

impl Log {
    /// # Errors
    ///
    /// If there is no home directory.
    pub fn open(profile: &str, threshold: Level) -> Result<Self, PathError> {
        Ok(Self {
            path: path_for(profile)?,
            threshold,
        })
    }

    #[must_use]
    pub fn path(&self) -> &std::path::Path {
        &self.path
    }

    /// Appends one line, rotating first if the file has grown past [`ROTATE_AT`].
    ///
    /// **Never fails a run.** A backup that could not be written down still happened,
    /// and refusing to back up because a log file is unwritable would be the tool
    /// choosing its own diary over its purpose.
    pub fn write(&self, level: Level, message: &str) {
        if level > self.threshold {
            return;
        }
        let _ = self.append(level, message);
    }

    fn append(&self, level: Level, message: &str) -> std::io::Result<()> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        if std::fs::metadata(&self.path).is_ok_and(|meta| meta.len() >= ROTATE_AT) {
            let _ = std::fs::rename(&self.path, self.path.with_extension("log.1"));
        }

        let mut line = String::new();
        let _ = writeln!(
            line,
            "{} {:<5} {message}",
            jiff::Timestamp::now()
                .to_zoned(jiff::tz::TimeZone::system())
                .strftime("%F %H:%M:%S"),
            level.word()
        );
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)?;
        file.write_all(line.as_bytes())
    }
}

/// # Errors
///
/// If there is no home directory.
pub fn path_for(profile: &str) -> Result<PathBuf, PathError> {
    Ok(log_dir()?.as_path().join(format!("{profile}.log")))
}

#[cfg(test)]
mod tests {
    use super::{Level, Log, ROTATE_AT};

    fn at(dir: &std::path::Path, threshold: Level) -> Log {
        Log {
            path: dir.join("demo.log"),
            threshold,
        }
    }

    #[test]
    fn a_line_carries_a_stamp_and_a_level() {
        let dir = tempfile::tempdir().expect("temp dir");
        let log = at(dir.path(), Level::Info);
        log.write(Level::Warn, "a remote is behind");

        let text = std::fs::read_to_string(log.path()).expect("read");
        assert!(text.contains("warn "), "{text}");
        assert!(text.contains("a remote is behind"), "{text}");
        assert!(text.starts_with("20"), "the stamp comes first: {text}");
    }

    #[test]
    fn a_level_below_the_threshold_is_not_written() {
        let dir = tempfile::tempdir().expect("temp dir");
        let log = at(dir.path(), Level::Warn);
        log.write(Level::Debug, "chatter");
        log.write(Level::Error, "the thing that matters");

        let text = std::fs::read_to_string(log.path()).expect("read");
        assert!(!text.contains("chatter"), "{text}");
        assert!(text.contains("the thing that matters"), "{text}");
    }

    #[test]
    fn the_file_rotates_once_past_its_size() {
        let dir = tempfile::tempdir().expect("temp dir");
        let log = at(dir.path(), Level::Info);
        std::fs::write(log.path(), vec![b'x'; ROTATE_AT as usize]).expect("a full log");

        log.write(Level::Info, "the line that rotates it");
        let text = std::fs::read_to_string(log.path()).expect("read");
        assert!(text.contains("the line that rotates it"));
        assert!(
            text.len() < 200,
            "the new file starts fresh rather than appending to a full one"
        );
        assert!(
            dir.path().join("demo.log.1").exists(),
            "one generation kept"
        );
    }

    /// A backup that could not be written down still happened. Refusing to back up
    /// because a log file is unwritable would be the tool choosing its diary over its
    /// purpose.
    #[test]
    fn an_unwritable_log_is_silent_rather_than_fatal() {
        let log = Log {
            path: std::path::PathBuf::from("/this/path/cannot/exist/demo.log"),
            threshold: Level::Info,
        };
        log.write(Level::Error, "nothing catches fire");
    }
}
