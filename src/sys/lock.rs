//! The single-writer lock, held for the duration of any store or state mutation.
//!
//! Always a try-lock. A blocking one converts a single hung run into permanently
//! silent backups, which is the failure this project exists to correct.

use std::fs::{File, OpenOptions, TryLockError};
use std::io::{self, Read, Write};
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

/// What a contender learns about the run already in progress. Both fields are
/// optional because the holder may not have finished recording itself, and an
/// unreadable stamp is a worse message rather than permission to proceed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Held {
    pub since_unix: Option<u64>,
    pub pid: Option<u32>,
}

#[derive(Debug, thiserror::Error)]
pub enum LockError {
    #[error("a run is already in progress")]
    Held(Held),
    #[error("could not take the lock at {path}: {source}")]
    Io {
        path: String,
        #[source]
        source: io::Error,
    },
}

/// Released when dropped, and by the kernel if the process dies holding it.
#[derive(Debug)]
pub struct LockGuard {
    _file: File,
}

/// Takes the lock, or reports who holds it. Never blocks.
///
/// # Errors
///
/// [`LockError::Held`] when another handle holds it, [`LockError::Io`] otherwise.
pub fn try_lock(path: &Path) -> Result<LockGuard, LockError> {
    let io_error = |source: io::Error| LockError::Io {
        path: path.display().to_string(),
        source,
    };
    let file = OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(path)
        .map_err(io_error)?;

    match file.try_lock() {
        Ok(()) => {}
        Err(TryLockError::WouldBlock) => return Err(LockError::Held(read_holder(path))),
        Err(TryLockError::Error(source)) => return Err(io_error(source)),
    }

    // Recorded only once the lock is held, or a contender could read a file that
    // was truncated before its new contents landed.
    file.set_len(0).map_err(io_error)?;
    let since = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs())
        .unwrap_or_default();
    (&file)
        .write_all(format!("{since} {}\n", std::process::id()).as_bytes())
        .map_err(io_error)?;
    (&file).flush().map_err(io_error)?;

    Ok(LockGuard { _file: file })
}

fn read_holder(path: &Path) -> Held {
    let mut text = String::new();
    if let Ok(mut file) = File::open(path) {
        let _ = file.read_to_string(&mut text);
    }
    let mut fields = text.split_whitespace();
    Held {
        since_unix: fields.next().and_then(|field| field.parse().ok()),
        pid: fields.next().and_then(|field| field.parse().ok()),
    }
}

#[cfg(test)]
mod tests {
    use super::{LockError, try_lock};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::thread;

    #[test]
    fn a_second_holder_is_refused_and_told_who_holds_it() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("profile.lock");

        let guard = try_lock(&path).expect("the lock is free");
        let error = try_lock(&path).expect_err("the lock is held");
        let LockError::Held(held) = error else {
            panic!("expected Held, got {error}");
        };
        assert_eq!(held.pid, Some(std::process::id()));
        assert!(held.since_unix.is_some_and(|since| since > 1_700_000_000));

        drop(guard);
        try_lock(&path).expect("the lock is free again");
    }

    #[test]
    fn only_one_holder_at_a_time_under_contention() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("profile.lock");
        let inside = AtomicUsize::new(0);
        let acquired = AtomicUsize::new(0);

        thread::scope(|scope| {
            for _ in 0..8 {
                scope.spawn(|| {
                    for _ in 0..200 {
                        let Ok(guard) = try_lock(&path) else { continue };
                        let before = inside.fetch_add(1, Ordering::SeqCst);
                        assert_eq!(before, 0, "two holders at once");
                        acquired.fetch_add(1, Ordering::SeqCst);
                        inside.fetch_sub(1, Ordering::SeqCst);
                        drop(guard);
                    }
                });
            }
        });

        assert!(
            acquired.load(Ordering::SeqCst) > 0,
            "no thread ever took the lock"
        );
    }

    #[test]
    fn a_released_lock_is_immediately_reacquirable() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("profile.lock");
        for round in 0..500 {
            let guard = try_lock(&path).unwrap_or_else(|error| {
                panic!("round {round} could not take a free lock: {error}")
            });
            let held = try_lock(&path);
            assert!(held.is_err(), "round {round} double-locked");
            drop(guard);
        }
    }

    #[test]
    fn the_stamp_is_rewritten_by_each_holder() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("profile.lock");

        drop(try_lock(&path).expect("free"));
        let guard = try_lock(&path).expect("free again");
        let LockError::Held(held) = try_lock(&path).expect_err("held") else {
            panic!("expected Held");
        };
        assert_eq!(held.pid, Some(std::process::id()));
        drop(guard);
    }
}
