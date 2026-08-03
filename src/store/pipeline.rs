//! The run spine, enforced by the type system.
//!
//! A run is a fixed sequence, and `refs/heads/main` moving before the tree has been
//! reconciled would publish a backup that is short of what was planned. Consuming
//! transitions alone already stop that - the method only exists on the state that
//! earned it - but they do not stop someone *adding* a transition that skips a
//! state. Every step therefore goes through [`Run::advance`], which is bounded on
//! [`After`], so a skip requires a new line in the spine declaration rather than
//! emerging from whichever methods happen to exist.

use crate::config::Profile;
use crate::primitives::oid::Oid;
use crate::spine::spine;
use crate::store::Store;

/// The profile lock is held.
#[derive(Debug)]
pub struct Locked;

/// The walk finished and the sanity gate passed.
#[derive(Debug)]
pub struct Planned {
    pub plan: crate::plan::Plan,
}

/// Every planned file has an outcome: an object id, or a named reason it has none.
#[derive(Debug)]
pub struct Hashed {
    pub plan: crate::plan::Plan,
    pub entries: Vec<crate::git::IndexEntry>,
    pub unreadable: Vec<String>,
}

/// Every repository's history is in `refs/tycho/*`, its overlay and `REPO.txt` are
/// hashed, and the config is captured alongside them.
///
/// Those refs move here, before the commit, so an interrupted run can leave captured
/// refs ahead of the last backup commit. That is safe only because nothing is ever
/// pruned, which is why the absence of `--prune` is an invariant rather than a
/// preference.
#[derive(Debug)]
pub struct Captured {
    pub plan: crate::plan::Plan,
    pub entries: Vec<crate::git::IndexEntry>,
    pub unreadable: Vec<String>,
    pub repos_captured: usize,
}

/// The scratch index holds those entries.
#[derive(Debug)]
pub struct Indexed {
    pub plan: crate::plan::Plan,
    pub planned: usize,
    pub unreadable: Vec<String>,
    pub repos_captured: usize,
}

/// A tree exists.
#[derive(Debug)]
pub struct Treed {
    pub plan: crate::plan::Plan,
    pub planned: usize,
    pub unreadable: Vec<String>,
    pub repos_captured: usize,
    pub tree: Oid,
}

/// The tree holds every entry the run put in the index. Nothing may publish before
/// this, which is the reason the spine is typed at all.
#[derive(Debug)]
pub struct Reconciled {
    pub plan: crate::plan::Plan,
    pub unreadable: Vec<String>,
    pub repos_captured: usize,
    pub tree: Oid,
}

/// A commit exists, but no ref points at it yet.
#[derive(Debug)]
pub struct Committed {
    pub plan: crate::plan::Plan,
    pub unreadable: Vec<String>,
    pub commit: Oid,
    pub summary: crate::store::message::Summary,
}

/// `refs/heads/main` points at the commit. The run is durable from here.
#[derive(Debug)]
pub struct Published {
    pub commit: Oid,
    pub unreadable: Vec<String>,
    pub summary: crate::store::message::Summary,
    pub record: crate::state::RunRecord,
}

/// Every remote has been attempted and its state transitioned.
///
/// After `Published` because a remote must never receive a commit the store itself
/// has not adopted, and before `Recorded` because the run's outcome depends on what
/// the remotes did: a commit that never left the machine is the condition this
/// project treats as not yet a backup.
#[derive(Debug)]
pub struct Mirrored {
    pub commit: Oid,
    pub unreadable: Vec<String>,
    pub summary: crate::store::message::Summary,
    pub record: crate::state::RunRecord,
    pub remotes: Vec<crate::store::run::RemoteResult>,
}

/// The state file records what happened.
#[derive(Debug)]
pub struct Recorded {
    pub commit: Oid,
    pub unreadable: Vec<String>,
    pub summary: crate::store::message::Summary,
    pub record: crate::state::RunRecord,
    pub remotes: Vec<crate::store::run::RemoteResult>,
}

spine! {
    Locked -> Planned -> Hashed -> Captured -> Indexed -> Treed
           -> Reconciled -> Committed -> Published -> Mirrored -> Recorded
}

/// A run in progress, carrying what it has established so far.
///
/// ```compile_fail
/// # use tycho::store::pipeline::*;
/// // Skipping reconciliation must not compile.
/// fn skip(run: Run<Treed>) -> Result<Run<Committed>, ()> {
///     run.advance(|_| unimplemented!())
/// }
/// ```
///
/// ```compile_fail
/// # use tycho::store::pipeline::*;
/// // Nor may the spine run backwards.
/// fn backwards(run: Run<Published>) -> Result<Run<Treed>, ()> {
///     run.advance(|_| unimplemented!())
/// }
/// ```
#[derive(Debug)]
pub struct Run<'a, S> {
    pub profile: &'a Profile,
    pub store: &'a Store,
    pub state: S,
}

impl<'a> Run<'a, Locked> {
    /// The only way in. Everything after it is an `advance`.
    #[must_use]
    pub const fn start(profile: &'a Profile, store: &'a Store) -> Self {
        Self {
            profile,
            store,
            state: Locked,
        }
    }
}

impl<'a, S> Run<'a, S> {
    /// The one chokepoint. `step` builds the next state out of this one - which is
    /// what makes each state's payload the only thing the next step can start from -
    /// and the [`After`] bound means the spine cannot be short-cut without declaring
    /// the short cut.
    ///
    /// # Errors
    ///
    /// Whatever `step` returns.
    pub fn advance<T: After<S>, E>(
        self,
        step: impl FnOnce(S) -> Result<T, E>,
    ) -> Result<Run<'a, T>, E> {
        Ok(Run {
            profile: self.profile,
            store: self.store,
            state: step(self.state)?,
        })
    }
}
