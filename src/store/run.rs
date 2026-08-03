//! One backup, start to finish, along the spine in `pipeline`.

use crate::capture;
use crate::config::Profile;
use crate::config::rules::RuleTree;
use crate::plan::{self, Plan};
use crate::primitives::path::TreePath;
use crate::state::{Outcome, RunRecord, State};
use crate::store::pipeline::{
    Captured, Committed, Hashed, Indexed, Locked, Planned, Published, Reconciled, Recorded, Run,
    Treed,
};
use crate::store::{Store, StoreError, message};
use crate::sys::lock::{LockError, LockGuard, try_lock};
use jiff::{Timestamp, tz::TimeZone};
use std::collections::BTreeMap;
use std::path::Path;
use std::time::Instant;

#[derive(Debug, thiserror::Error)]
pub enum RunError {
    #[error("a run is already in progress{}", since(*.0))]
    InProgress(Option<u64>),
    #[error(transparent)]
    Lock(#[from] LockError),
    #[error(transparent)]
    Plan(#[from] plan::PlanError),
    #[error(transparent)]
    Rules(#[from] crate::config::rules::RuleError),
    #[error(transparent)]
    Store(#[from] StoreError),
    #[error(transparent)]
    State(#[from] crate::state::StateError),
    #[error(transparent)]
    Capture(#[from] capture::CaptureError),
}

fn tycho_path(text: &str) -> Result<TreePath, RunError> {
    TreePath::parse(std::path::Path::new(text)).map_err(|error| {
        RunError::Capture(capture::CaptureError::NotStorable {
            path: text.to_owned(),
            reason: error.to_string(),
        })
    })
}

fn since(started: Option<u64>) -> String {
    started.map_or_else(String::new, |unix| {
        Timestamp::from_second(i64::try_from(unix).unwrap_or_default()).map_or_else(
            |_| String::new(),
            |stamp| {
                format!(
                    " since {}",
                    stamp.to_zoned(TimeZone::system()).strftime("%F %H:%M")
                )
            },
        )
    })
}

/// What one run produced, for the caller to render.
#[derive(Debug)]
pub struct Completed {
    pub commit: crate::primitives::oid::Oid,
    pub summary: message::Summary,
    pub warnings: Vec<String>,
    pub record: RunRecord,
}

/// Takes the profile lock, walks the spine, and records the result.
///
/// The lock is a try-lock: a blocking one converts a single hung run into
/// permanently silent backups.
///
/// # Errors
///
/// If the lock is held, the plan fails its gate, or git fails.
pub fn execute(
    profile: &Profile,
    store: &Store,
    paths: &Paths<'_>,
    state: &mut State,
    allow_shrink: bool,
) -> Result<Completed, RunError> {
    let guard = match try_lock(paths.lock) {
        Ok(guard) => guard,
        Err(LockError::Held(held)) => return Err(RunError::InProgress(held.since_unix)),
        Err(other) => return Err(RunError::Lock(other)),
    };
    let previous = state.last_entries(profile.name.as_str());
    let started = Instant::now();
    let run = Run::start(profile, store);
    finish(run, guard, started, &previous, paths, state, allow_shrink)
}

/// Where a run's two files live.
#[derive(Clone, Copy, Debug)]
pub struct Paths<'a> {
    pub lock: &'a Path,
    pub state: &'a Path,
    /// The resolved config, captured into the tree so the description of what a
    /// backup should contain travels with the backup.
    pub config_text: Option<&'a str>,
}

#[allow(clippy::needless_pass_by_value, clippy::too_many_arguments)]
fn finish(
    run: Run<'_, Locked>,
    guard: LockGuard,
    started: Instant,
    previous: &BTreeMap<String, usize>,
    paths: &Paths<'_>,
    state: &mut State,
    allow_shrink: bool,
) -> Result<Completed, RunError> {
    let config_text = paths.config_text;
    let state_path = paths.state;
    let (profile, store) = (run.profile, run.store);
    let rules = RuleTree::build(&profile.rule_set())?;
    // Measured before anything is hashed, and read again once the tree exists. The
    // commit object is deliberately outside that window: store.md wants a run that
    // changed nothing to report 0 B honestly, rather than the couple of hundred
    // bytes its own commit costs.
    let before = store.size()?;

    let run = run.advance(|Locked| {
        Ok::<_, RunError>(Planned {
            plan: plan::build(profile, &rules, previous, allow_shrink)?,
        })
    })?;

    let run = run.advance(|Planned { plan }| {
        let (entries, unreadable) = store.hash(&plan)?;
        Ok::<_, RunError>(Hashed {
            plan,
            entries,
            unreadable,
        })
    })?;

    let run = run.advance(
        |Hashed {
             plan,
             mut entries,
             mut unreadable,
         }| {
            let nested = capture::nested_roots(&plan);
            let mut contribution = capture::Contribution::default();
            let mut repos_captured = 0;

            for root in &plan.roots {
                for repo in root.repos() {
                    let part = capture::capture(store, repo, &rules, &nested)?;
                    contribution.files.extend(part.files);
                    contribution.generated.extend(part.generated);
                    contribution.warnings.extend(part.warnings);
                    repos_captured += 1;
                }
            }

            // The definition of what a backup contains travels with the backup.
            // Without it a recovery restores the data but not the description of
            // what was meant to be in it, and on a replacement machine that
            // description exists nowhere else.
            if let Some(text) = config_text {
                contribution
                    .generated
                    .push((tycho_path(".tycho/config.toml")?, text.as_bytes().to_vec()));
            }

            let (overlay, missed) = store.hash_files(&contribution.files)?;
            entries.extend(overlay);
            entries.extend(store.hash_generated(&contribution.generated)?);
            unreadable.extend(missed);
            unreadable.extend(contribution.warnings);

            Ok::<_, RunError>(Captured {
                plan,
                entries,
                unreadable,
                repos_captured,
            })
        },
    )?;

    let run = run.advance(
        |Captured {
             plan,
             entries,
             unreadable,
             repos_captured,
         }| {
            let planned = store.index(&entries)?;
            Ok::<_, RunError>(Indexed {
                plan,
                planned,
                unreadable,
                repos_captured,
            })
        },
    )?;

    let run = run.advance(
        |Indexed {
             plan,
             planned,
             unreadable,
             repos_captured,
         }| {
            Ok::<_, RunError>(Treed {
                plan,
                planned,
                unreadable,
                repos_captured,
                tree: store.tree()?,
            })
        },
    )?;

    // Nothing may publish before this succeeds, which is the reason the spine is
    // typed at all.
    let run = run.advance(
        |Treed {
             plan,
             planned,
             unreadable,
             repos_captured,
             tree,
         }| {
            store.reconcile(tree, planned)?;
            Ok::<_, RunError>(Reconciled {
                plan,
                unreadable,
                repos_captured,
                tree,
            })
        },
    )?;

    let parent = store.head()?;

    let run = run.advance(
        |Reconciled {
             plan,
             unreadable,
             repos_captured,
             tree,
         }| {
            let changes = store.changes(parent, tree)?;
            let mut summary = message::Summary::from_changes(&changes, plan.roots.len());
            summary.repos_found = plan.repo_count();
            summary.repos_captured = repos_captured;
            summary.tracked_bytes = plan.bytes();
            summary.written_bytes = store.size()?.saturating_sub(before);
            summary.seconds = started.elapsed().as_secs();

            let stamp = Timestamp::now()
                .to_zoned(TimeZone::UTC)
                .strftime("%F %H:%M UTC")
                .to_string();
            let text = message::render(&stamp, profile.name.as_str(), &summary);
            let commit = store.commit(tree, parent, &text)?;
            Ok::<_, RunError>(Committed {
                plan,
                unreadable,
                commit,
                summary,
            })
        },
    )?;

    let run = run.advance(
        |Committed {
             plan,
             unreadable,
             commit,
             summary,
         }| {
            store.publish(commit)?;
            store.gc()?;
            let store_bytes = store.size()?;
            Ok::<_, RunError>(Published {
                commit,
                record: RunRecord {
                    when: Timestamp::now().to_string(),
                    outcome: Outcome::Ok,
                    commit: Some(commit.to_string()),
                    entries: plan
                        .roots
                        .iter()
                        .map(|root| (root.alias.clone(), root.capturable()))
                        .collect(),
                    files: plan.files(),
                    bytes: plan.bytes(),
                    store_bytes,
                    warnings: unreadable.clone(),
                },
                unreadable,
                summary,
            })
        },
    )?;

    let run = run.advance(
        |Published {
             commit,
             unreadable,
             summary,
             record,
         }| {
            state.record(profile.name.as_str(), record.clone());
            state.save(state_path)?;
            Ok::<_, RunError>(Recorded {
                commit,
                unreadable,
                summary,
                record,
            })
        },
    )?;

    // Held until here, so no second run can interleave with any of it.
    drop(guard);

    let Recorded {
        commit,
        unreadable,
        summary,
        record,
    } = run.state;
    Ok(Completed {
        commit,
        summary,
        warnings: unreadable,
        record,
    })
}

/// The plan a run would produce, for `--dry-run`.
///
/// # Errors
///
/// As [`execute`], minus everything that touches the store.
pub fn dry(
    profile: &Profile,
    previous: &BTreeMap<String, usize>,
    allow_shrink: bool,
) -> Result<Plan, RunError> {
    let tree = RuleTree::build(&profile.rule_set())?;
    Ok(plan::build(profile, &tree, previous, allow_shrink)?)
}
