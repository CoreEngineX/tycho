//! What a remote's condition is, and how it changes.
//!
//! Pure: `advance` is a function of the current state, what the run observed, and
//! the remote's tolerance. `remotes.md` section 4 is the specification and its seven
//! transitions are the test suite.

use serde::{Deserialize, Serialize};

/// A required remote fails on the first missed run. Expressing that as a tolerance
/// rather than a special case is what lets `status` say `behind 3 of 4` for both
/// kinds.
pub const REQUIRED_TOLERANCE: u32 = 1;
pub const DEFAULT_OPTIONAL_TOLERANCE: u32 = 4;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FailureReason {
    /// The path holds something that is not this remote's repository.
    Unusable(String),
    /// A non-fast-forward on `refs/heads/*`, which means a second machine is pushing
    /// this profile name.
    Rejected(String),
    /// The push reported success and the remote disagrees. The case `remotes.md`
    /// calls the one worth being loudest about.
    Unverified(String),
    /// Out of reach for longer than the tolerance allows.
    TooFarBehind {
        runs: u32,
    },
    Other(String),
}

impl std::fmt::Display for FailureReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unusable(detail) => write!(f, "unusable: {detail}"),
            Self::Rejected(detail) => write!(f, "push rejected: {detail}"),
            Self::Unverified(detail) => write!(f, "not verified after push: {detail}"),
            Self::TooFarBehind { runs } => write!(f, "behind {runs} runs"),
            Self::Other(detail) => write!(f, "{detail}"),
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum RemoteState {
    #[default]
    Unseen,
    Synced {
        at: String,
        head: String,
    },
    Behind {
        runs: u32,
        last_seen: String,
    },
    Failed {
        at: String,
        reason: FailureReason,
    },
}

/// What a run observed about one remote.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Observation {
    /// Pushed, and the full ref set compared equal.
    Verified { head: String },
    /// Out of reach: the path is gone, the drive is unplugged, the sync client is
    /// signed out. Not an error in itself.
    Unreachable,
    /// Reached, and something was wrong with it.
    Refused(FailureReason),
}

/// The one transition function.
///
/// `tolerance` is how many consecutive missed runs a remote may accumulate before
/// being behind becomes failing: 1 for a required remote, `behind_tolerance` for an
/// optional one.
#[must_use]
pub fn advance(state: &RemoteState, seen: &Observation, tolerance: u32, now: &str) -> RemoteState {
    match seen {
        // T1, T4, T7, and the unremarkable Synced-to-Synced.
        Observation::Verified { head } => RemoteState::Synced {
            at: now.to_owned(),
            head: head.clone(),
        },

        // T5 and T6's sibling: something was wrong with the remote itself.
        Observation::Refused(reason) => RemoteState::Failed {
            at: now.to_owned(),
            reason: reason.clone(),
        },

        Observation::Unreachable => match state {
            // T2. A required remote unreachable on its first run has a tolerance of
            // 1, so it fails immediately rather than passing through Behind.
            RemoteState::Unseen if tolerance <= 1 => RemoteState::Failed {
                at: now.to_owned(),
                reason: FailureReason::TooFarBehind { runs: 1 },
            },
            RemoteState::Unseen => RemoteState::Behind {
                runs: 1,
                last_seen: String::new(),
            },

            // T3, then T6 once the lag exceeds what is allowed.
            RemoteState::Synced { at, .. } => behind_or_failed(1, at, tolerance, now),
            RemoteState::Behind { runs, last_seen } => {
                behind_or_failed(runs.saturating_add(1), last_seen, tolerance, now)
            }

            // Already failed and still out of reach: nothing new to say.
            RemoteState::Failed { .. } => state.clone(),
        },
    }
}

fn behind_or_failed(runs: u32, last_seen: &str, tolerance: u32, now: &str) -> RemoteState {
    if runs > tolerance {
        return RemoteState::Failed {
            at: now.to_owned(),
            reason: FailureReason::TooFarBehind { runs },
        };
    }
    RemoteState::Behind {
        runs,
        last_seen: last_seen.to_owned(),
    }
}

impl RemoteState {
    /// Whether this state should make a run, and `status --check`, exit non-zero.
    #[must_use]
    pub const fn is_red(&self) -> bool {
        matches!(self, Self::Failed { .. })
    }

    /// Yellow: worth showing, not worth failing over. An optional drive that is
    /// merely unplugged lives here, which is what keeps a weekly monitor from crying
    /// wolf every time it is out.
    #[must_use]
    pub const fn is_yellow(&self) -> bool {
        matches!(self, Self::Behind { .. } | Self::Unseen)
    }

    /// The short word `status` prints. Meaning never depends on colour.
    #[must_use]
    pub fn word(&self, tolerance: u32) -> String {
        match self {
            Self::Unseen => "unseen".to_owned(),
            Self::Synced { .. } => "ok".to_owned(),
            Self::Behind { runs, .. } => format!("behind {runs} of {tolerance}"),
            Self::Failed { .. } => "failed".to_owned(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{FailureReason, Observation, RemoteState, advance};

    const NOW: &str = "2026-11-02T09:14:00Z";
    const OPTIONAL: u32 = 4;
    const REQUIRED: u32 = 1;

    fn synced() -> RemoteState {
        RemoteState::Synced {
            at: "2026-11-01T12:00:00Z".to_owned(),
            head: "abc".to_owned(),
        }
    }

    fn verified() -> Observation {
        Observation::Verified {
            head: "def".to_owned(),
        }
    }

    /// T1: first push succeeds and the full ref comparison matches.
    #[test]
    fn t1_unseen_to_synced() {
        let next = advance(&RemoteState::Unseen, &verified(), OPTIONAL, NOW);
        assert_eq!(
            next,
            RemoteState::Synced {
                at: NOW.to_owned(),
                head: "def".to_owned()
            }
        );
    }

    /// T2: a required remote unreachable on its first run.
    #[test]
    fn t2_unseen_to_failed_when_required() {
        let next = advance(
            &RemoteState::Unseen,
            &Observation::Unreachable,
            REQUIRED,
            NOW,
        );
        assert!(next.is_red(), "a required remote must not sit at Behind");

        // The same observation on an optional remote is merely yellow.
        let optional = advance(
            &RemoteState::Unseen,
            &Observation::Unreachable,
            OPTIONAL,
            NOW,
        );
        assert!(optional.is_yellow() && !optional.is_red());
    }

    /// T3: unreachable, for required and optional alike. The difference is the
    /// tolerance, not the state.
    #[test]
    fn t3_synced_to_behind() {
        let next = advance(&synced(), &Observation::Unreachable, OPTIONAL, NOW);
        assert_eq!(
            next,
            RemoteState::Behind {
                runs: 1,
                last_seen: "2026-11-01T12:00:00Z".to_owned()
            }
        );
    }

    /// T4: reachable again. The next push carries everything missed.
    #[test]
    fn t4_behind_to_synced() {
        let behind = RemoteState::Behind {
            runs: 3,
            last_seen: "2026-10-11T12:00:00Z".to_owned(),
        };
        assert!(matches!(
            advance(&behind, &verified(), OPTIONAL, NOW),
            RemoteState::Synced { .. }
        ));
    }

    /// T5: push rejected, or a ref missing after a reported success.
    #[test]
    fn t5_synced_to_failed() {
        let next = advance(
            &synced(),
            &Observation::Refused(FailureReason::Rejected("non-fast-forward".to_owned())),
            OPTIONAL,
            NOW,
        );
        assert!(next.is_red());
    }

    /// T6: lag exceeds the tolerance.
    #[test]
    fn t6_behind_to_failed_past_tolerance() {
        let mut state = synced();
        for run in 1..=OPTIONAL {
            state = advance(&state, &Observation::Unreachable, OPTIONAL, NOW);
            assert!(
                !state.is_red(),
                "run {run} is within tolerance and must stay yellow: {state:?}"
            );
        }
        state = advance(&state, &Observation::Unreachable, OPTIONAL, NOW);
        assert!(state.is_red(), "the fifth miss exceeds a tolerance of four");
    }

    /// T7: the cause was fixed and the next run pushed cleanly.
    #[test]
    fn t7_failed_to_synced() {
        let failed = RemoteState::Failed {
            at: NOW.to_owned(),
            reason: FailureReason::TooFarBehind { runs: 9 },
        };
        assert!(matches!(
            advance(&failed, &verified(), OPTIONAL, NOW),
            RemoteState::Synced { .. }
        ));
    }

    #[test]
    fn the_lag_reads_against_its_tolerance() {
        let behind = RemoteState::Behind {
            runs: 3,
            last_seen: String::new(),
        };
        assert_eq!(behind.word(OPTIONAL), "behind 3 of 4");
        assert_eq!(synced().word(OPTIONAL), "ok");
    }

    /// A remote that is already failing and still out of reach has nothing new to
    /// report, and must not have its recorded cause overwritten by a vaguer one.
    #[test]
    fn a_failed_remote_keeps_the_reason_it_failed_for() {
        let failed = RemoteState::Failed {
            at: NOW.to_owned(),
            reason: FailureReason::Rejected("non-fast-forward".to_owned()),
        };
        assert_eq!(
            advance(&failed, &Observation::Unreachable, OPTIONAL, NOW),
            failed
        );
    }
}
