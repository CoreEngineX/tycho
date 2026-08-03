//! One backup, start to finish, along the spine in `pipeline`.

use crate::config::Profile;
use crate::config::rules::RuleTree;
use crate::plan::{self, Plan};
use crate::state::{Outcome, RunRecord, State};
use crate::store::pipeline::{
    Committed, Hashed, Indexed, Locked, Planned, Published, Reconciled, Recorded, Run, Treed,
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
    finish(
        run,
        guard,
        started,
        &previous,
        paths.state,
        state,
        allow_shrink,
    )
}

/// Where a run's two files live.
#[derive(Clone, Copy, Debug)]
pub struct Paths<'a> {
    pub lock: &'a Path,
    pub state: &'a Path,
}

#[allow(clippy::needless_pass_by_value, clippy::too_many_arguments)]
fn finish(
    run: Run<'_, Locked>,
    guard: LockGuard,
    started: Instant,
    previous: &BTreeMap<String, usize>,
    state_path: &Path,
    state: &mut State,
    allow_shrink: bool,
) -> Result<Completed, RunError> {
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
             entries,
             unreadable,
         }| {
            let planned = store.index(&entries)?;
            Ok::<_, RunError>(Indexed {
                plan,
                planned,
                unreadable,
            })
        },
    )?;

    let run = run.advance(
        |Indexed {
             plan,
             planned,
             unreadable,
         }| {
            Ok::<_, RunError>(Treed {
                plan,
                planned,
                unreadable,
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
             tree,
         }| {
            store.reconcile(tree, planned)?;
            Ok::<_, RunError>(Reconciled {
                plan,
                unreadable,
                tree,
            })
        },
    )?;

    let parent = store.head()?;

    let run = run.advance(
        |Reconciled {
             plan,
             unreadable,
             tree,
         }| {
            let changes = store.changes(parent, tree)?;
            let mut summary = message::Summary::from_changes(&changes, plan.roots.len());
            summary.repos_found = plan.repo_count();
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
