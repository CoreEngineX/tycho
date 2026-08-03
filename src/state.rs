//! Layer 4. Run records, written by atomic rename.
//!
//! The state file is an observation log of what happened, never a competing
//! definition of what should exist - the config is that. It is also not where
//! anything a restore depends on lives: the store carries its own history, because
//! the disaster path is a replacement machine where this file is already gone.

use crate::sys::fs::write_atomic;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::Path;

/// How a run ended. A partial run - the commit landed but an optional remote was
/// unreachable - is not a failure, and an unreachable *required* remote is, even
/// though the local commit landed: a backup that has not left the machine is the
/// condition this project treats as not yet a backup.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Outcome {
    Ok,
    Partial,
    Failed,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RunRecord {
    /// RFC 3339, so the file stays readable without this program.
    pub when: String,
    pub outcome: Outcome,
    pub commit: Option<String>,
    /// Per root alias, the entry count the sanity gate compares the next run against.
    #[serde(default)]
    pub entries: BTreeMap<String, usize>,
    pub files: usize,
    pub bytes: u64,
    pub store_bytes: u64,
    #[serde(default)]
    pub warnings: Vec<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct State {
    #[serde(default)]
    pub profiles: BTreeMap<String, Vec<RunRecord>>,
}

/// How many runs are kept per profile. The store keeps history forever; this file
/// only needs enough to answer "when did it last work" and "how big was it".
pub const KEPT_RUNS: usize = 64;

#[derive(Debug, thiserror::Error)]
pub enum StateError {
    #[error("{context}: {source}")]
    Io {
        context: String,
        #[source]
        source: std::io::Error,
    },
    #[error("the state file at {path} is not readable as state: {source}")]
    Malformed {
        path: String,
        #[source]
        source: serde_json::Error,
    },
}

impl State {
    /// Reads the state file. A missing one is an empty state, not an error - the
    /// first run on a machine has nothing to read.
    ///
    /// # Errors
    ///
    /// If the file exists but cannot be read or parsed.
    pub fn load(path: &Path) -> Result<Self, StateError> {
        let text = match std::fs::read_to_string(path) {
            Ok(text) => text,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Self::default());
            }
            Err(source) => {
                return Err(StateError::Io {
                    context: format!("reading {}", path.display()),
                    source,
                });
            }
        };
        serde_json::from_str(&text).map_err(|source| StateError::Malformed {
            path: path.display().to_string(),
            source,
        })
    }

    /// # Errors
    ///
    /// If the file cannot be written.
    pub fn save(&self, path: &Path) -> Result<(), StateError> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|source| StateError::Io {
                context: format!("creating {}", parent.display()),
                source,
            })?;
        }
        let text = serde_json::to_vec_pretty(self).map_err(|source| StateError::Malformed {
            path: path.display().to_string(),
            source,
        })?;
        write_atomic(path, &text).map_err(|source| StateError::Io {
            context: format!("writing {}", path.display()),
            source,
        })
    }

    pub fn record(&mut self, profile: &str, run: RunRecord) {
        let runs = self.profiles.entry(profile.to_owned()).or_default();
        runs.insert(0, run);
        runs.truncate(KEPT_RUNS);
    }

    #[must_use]
    pub fn last(&self, profile: &str) -> Option<&RunRecord> {
        self.profiles.get(profile)?.first()
    }

    /// What the sanity gate compares against: the entry counts of the last run that
    /// actually succeeded, so a failed run cannot lower the bar for the next one.
    #[must_use]
    pub fn last_entries(&self, profile: &str) -> BTreeMap<String, usize> {
        self.profiles
            .get(profile)
            .into_iter()
            .flatten()
            .find(|run| run.outcome != Outcome::Failed)
            .map(|run| run.entries.clone())
            .unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::{Outcome, RunRecord, State};
    use std::collections::BTreeMap;

    fn run(outcome: Outcome, entries: usize) -> RunRecord {
        RunRecord {
            when: "2026-08-02T12:00:00Z".to_owned(),
            outcome,
            commit: Some("8f2a10c".to_owned()),
            entries: BTreeMap::from([("A".to_owned(), entries)]),
            files: entries,
            bytes: 0,
            store_bytes: 0,
            warnings: Vec::new(),
        }
    }

    #[test]
    fn a_missing_file_is_an_empty_state() {
        let dir = tempfile::tempdir().expect("temp dir");
        let state = State::load(&dir.path().join("nothing.json")).expect("missing is fine");
        assert!(state.profiles.is_empty());
    }

    #[test]
    fn records_round_trip_through_the_file() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("state.json");

        let mut state = State::default();
        state.record("work", run(Outcome::Ok, 10));
        state.save(&path).expect("save");

        let read = State::load(&path).expect("load");
        assert_eq!(read.last("work").map(|r| r.files), Some(10));
    }

    #[test]
    fn the_newest_run_is_first_and_the_list_is_bounded() {
        let mut state = State::default();
        for index in 0..super::KEPT_RUNS + 10 {
            state.record("work", run(Outcome::Ok, index));
        }
        assert_eq!(state.profiles["work"].len(), super::KEPT_RUNS);
        assert_eq!(
            state.last("work").map(|r| r.files),
            Some(super::KEPT_RUNS + 9)
        );
    }

    /// A failed run must not become the baseline, or one bad run would let the next
    /// one shrink the backup without the gate noticing.
    #[test]
    fn the_gate_compares_against_the_last_run_that_worked() {
        let mut state = State::default();
        state.record("work", run(Outcome::Ok, 1_000));
        state.record("work", run(Outcome::Failed, 2));

        assert_eq!(state.last_entries("work").get("A"), Some(&1_000));
    }
}
