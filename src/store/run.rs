//! One backup, start to finish, along the spine in `pipeline`.

use crate::capture;
use crate::config::Profile;
use crate::config::rules::RuleTree;
use crate::plan::{self, Plan};
use crate::primitives::path::TreePath;
use crate::remote::{self, state::REQUIRED_TOLERANCE, state::RemoteState};
use crate::state::{Outcome, RunRecord, State};
use crate::store::pipeline::{
    Captured, Committed, Hashed, Indexed, Locked, Mirrored, Planned, Published, Reconciled,
    Recorded, Run, Treed,
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
    pub remotes: Vec<RemoteResult>,
}

/// What a run is doing, reported as it happens.
///
/// `store` is layer 4 and may not print for itself - `lib.rs`'s layer table - so a
/// run hands one of these to an [`Observer`] instead, and `cli` (layer 5) is the only
/// thing that turns it into text. Every variant is a phase slow enough on a real tree
/// to be mistaken for a hang; the instant ones (indexing, writing the tree,
/// reconciling, the commit itself, saving state) are one git call each and are not
/// here.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Step {
    /// Walking every watched root and applying the sanity gate. Nothing is counted
    /// yet - the count is this phase's own output.
    Planning,
    Hashing {
        files: usize,
    },
    Capturing {
        repo: String,
        done: usize,
        total: usize,
    },
    /// Writing the commit, then compacting the store (`git gc --auto`), which is the
    /// one that can run long on a store with a lot of loose objects.
    Publishing,
    Pushing {
        remote: String,
    },
}

/// A run's callback for [`Step`]. `&mut dyn FnMut` rather than a generic parameter,
/// so threading it through `advance` does not make `execute` generic over the
/// caller's closure type.
pub type Observer<'a> = &'a mut dyn FnMut(Step);

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
    observe: Observer<'_>,
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
        paths,
        state,
        allow_shrink,
        observe,
    )
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
    observe: Observer<'_>,
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

    observe(Step::Planning);
    let run = run.advance(|Locked| {
        Ok::<_, RunError>(Planned {
            plan: plan::build(profile, &rules, previous, allow_shrink)?,
        })
    })?;

    observe(Step::Hashing {
        files: run.state.plan.files(),
    });
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
            let total = plan.repo_count();

            for root in &plan.roots {
                for repo in root.repos() {
                    let part = capture::capture(store, repo, &rules, &nested)?;
                    contribution.files.extend(part.files);
                    contribution.generated.extend(part.generated);
                    contribution.warnings.extend(part.warnings);
                    repos_captured += 1;
                    observe(Step::Capturing {
                        repo: repo.key.clone(),
                        done: repos_captured,
                        total,
                    });
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

            // Read after the overlay is collected and before anything is hashed, so
            // the manifest covers every path the tree will hold. A git tree records
            // one bit per file, so without this a `0600` file restores `0644` - and
            // the store keeps gitignored content precisely because that is where
            // secrets live.
            let sources: Vec<(crate::primitives::path::AbsPath, TreePath)> = plan
                .roots
                .iter()
                .flat_map(|root| &root.entries)
                .filter_map(|entry| match entry {
                    plan::Entry::Plain(file) => Some((file.source.clone(), file.stored.clone())),
                    plan::Entry::Repo(_) => None,
                })
                .chain(contribution.files.iter().cloned())
                .collect();
            let manifest = crate::metadata::capture(&sources);
            if !manifest.is_empty() {
                contribution.generated.push((
                    tycho_path(crate::metadata::MANIFEST)?,
                    crate::metadata::render(&manifest).into_bytes(),
                ));
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

    observe(Step::Publishing);
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
             mut record,
         }| {
            let remotes = mirror(profile, store, state, observe);
            record.outcome = outcome_of(&remotes);
            Ok::<_, RunError>(Mirrored {
                commit,
                unreadable,
                summary,
                record,
                remotes,
            })
        },
    )?;

    let run = run.advance(
        |Mirrored {
             commit,
             unreadable,
             summary,
             record,
             remotes,
         }| {
            state.record(profile.name.as_str(), record.clone());
            state.save(state_path)?;
            Ok::<_, RunError>(Recorded {
                commit,
                unreadable,
                summary,
                record,
                remotes,
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
        remotes,
    } = run.state;
    Ok(Completed {
        commit,
        summary,
        warnings: unreadable,
        record,
        remotes,
    })
}

/// What one remote did on this run, alongside the state that resulted.
#[derive(Clone, Debug)]
pub struct RemoteResult {
    pub name: String,
    pub optional: bool,
    pub tolerance: u32,
    pub state: RemoteState,
}

/// A remote's tolerance: 1 if it is required, its configured allowance if not.
#[must_use]
pub fn tolerance(remote: &crate::config::Remote) -> u32 {
    if remote.optional {
        remote.behind_tolerance
    } else {
        REQUIRED_TOLERANCE
    }
}

/// Pushes to every configured remote and transitions each one's state.
///
/// Never returns an error. A destination that is unreachable, refuses, or fails
/// verification is an observation about that remote; letting it abort the loop would
/// mean one unplugged drive stopped the cloud copies from being written.
fn mirror(
    profile: &Profile,
    store: &Store,
    state: &mut State,
    observe: Observer<'_>,
) -> Vec<RemoteResult> {
    let now = Timestamp::now().to_string();
    let mut results = Vec::new();
    let mut folders: Vec<std::path::PathBuf> = Vec::new();

    for configured in &profile.remotes {
        let name = configured.name.as_str();
        let allowed = tolerance(configured);
        observe(Step::Pushing {
            remote: name.to_owned(),
        });
        let seen = remote::publish(store, configured, profile.name.as_str());
        if matches!(seen, remote::state::Observation::Verified { .. })
            && let Ok(folder) = remote::resolve(&configured.path)
        {
            folders.push(folder);
        }

        let next = remote::state::advance(
            &state.remote(profile.name.as_str(), name),
            &seen,
            allowed,
            &now,
        );
        state.set_remote(profile.name.as_str(), name, next.clone());
        results.push(RemoteResult {
            name: name.to_owned(),
            optional: configured.optional,
            tolerance: allowed,
            state: next,
        });
    }

    // After every push, so a sibling repository created earlier in this same run is
    // in the scan. A folder that took two profiles is written once.
    folders.sort();
    folders.dedup();
    for folder in &folders {
        let _ = remote::recovery::write(folder);
        // Last, because it must cover `RECOVERY.md` too - macOS writes a sidecar for
        // whatever was touched most recently, so a sweep before this one leaves its
        // own behind.
        remote::sweep_sidecars(folder);
    }
    results
}

/// A run that failed to reach a required remote is `Failed` even though the local
/// commit landed - a backup that has not left the machine is the condition this
/// project treats as not yet a backup.
#[must_use]
pub fn outcome_of(remotes: &[RemoteResult]) -> Outcome {
    if remotes.iter().any(|remote| remote.state.is_red()) {
        return Outcome::Failed;
    }
    if remotes
        .iter()
        .any(|remote| matches!(remote.state, RemoteState::Behind { .. }))
    {
        return Outcome::Partial;
    }
    Outcome::Ok
}

/// `tycho push`: everything a run does about remotes, and nothing it does about
/// capture.
///
/// **Capture happens on the backup schedule and nowhere else**, so what is in a
/// backup never depends on when a drive was plugged in.
///
/// # Errors
///
/// If the state file cannot be written. A held lock is `Ok(None)`: a run in progress
/// is about to push anyway.
pub fn catch_up(
    profile: &Profile,
    store: &Store,
    paths: &Paths<'_>,
    state: &mut State,
    observe: Observer<'_>,
) -> Result<Option<Vec<RemoteResult>>, RunError> {
    let guard = match try_lock(paths.lock) {
        Ok(guard) => guard,
        Err(LockError::Held(_)) => return Ok(None),
        Err(other) => return Err(RunError::Lock(other)),
    };
    let results = mirror(profile, store, state, observe);
    state.save(paths.state)?;
    drop(guard);
    Ok(Some(results))
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
