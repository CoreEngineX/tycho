//! The run spine, enforced by the type system.
//!
//! A run is a fixed sequence, and `refs/heads/main` moving before the tree has been
//! reconciled would publish a backup that is short of what was planned. Consuming
//! transitions alone already stop that - the method only exists on the state that
//! earned it - but they do not stop someone *adding* a transition that skips a
//! state. Every step therefore goes through [`Run::advance`], which is bounded on
//! [`After`], so a skip requires a new line in `run_spine!` rather than emerging
//! from whichever methods happen to exist.

use crate::config::Profile;
use crate::primitives::oid::Oid;
use crate::store::Store;

mod private {
    pub trait Sealed {}
}

/// One step of the run spine follows another.
///
/// Sealed, so nothing outside this crate can claim an adjacency, and every impl
/// inside it comes from `run_spine!` - a hand-written one is a build failure, see
/// `tests/layering.rs`. `toolkit`'s equivalent marks these `unsafe` as a
/// think-hard signal; here the crate forbids `unsafe_code` outright, and a wrong
/// impl reorders a pipeline rather than breaking memory safety, so borrowing the
/// keyword would cost `grep unsafe` its meaning for no control the seal and the
/// guard do not already give.
pub trait After<S>: private::Sealed {}

/// The symmetric companion to [`After`], so a step can name its successor.
pub trait Before<S>: private::Sealed {}

/// Declares the spine, and nothing else.
///
/// A wall of `impl After<Locked> for Planned {}` lines is the pipeline's
/// most important declaration written in its least readable form. This takes the
/// chain as it appears in `store.md` and expands the sealing, the ordering, and the
/// test that pins it.
macro_rules! run_spine {
    ($($state:ident)->+ $(,)?) => {
        $( impl private::Sealed for $state {} )+
        run_spine!(@link $($state)->+);

        #[cfg(test)]
        mod spine {
            use super::{After, $($state),+};

            const fn assert_after<A, B: After<A>>() {}

            /// Fails to compile if the spine is reordered or a link is dropped.
            #[test]
            fn the_spine_is_the_documented_order() {
                run_spine!(@assert $($state)->+);
            }
        }
    };

    (@link $a:ident -> $b:ident $(-> $rest:ident)*) => {
        impl After<$a> for $b {}
        impl Before<$b> for $a {}
        run_spine!(@link $b $(-> $rest)*);
    };
    (@link $a:ident) => {};

    (@assert $a:ident -> $b:ident $(-> $rest:ident)*) => {
        assert_after::<$a, $b>();
        run_spine!(@assert $b $(-> $rest)*);
    };
    (@assert $a:ident) => {};
}

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

/// The scratch index holds those entries.
#[derive(Debug)]
pub struct Indexed {
    pub plan: crate::plan::Plan,
    pub planned: usize,
    pub unreadable: Vec<String>,
}

/// A tree exists.
#[derive(Debug)]
pub struct Treed {
    pub plan: crate::plan::Plan,
    pub planned: usize,
    pub unreadable: Vec<String>,
    pub tree: Oid,
}

/// The tree holds every entry the run put in the index. Nothing may publish before
/// this, which is the reason the spine is typed at all.
#[derive(Debug)]
pub struct Reconciled {
    pub plan: crate::plan::Plan,
    pub unreadable: Vec<String>,
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

/// The state file records what happened.
#[derive(Debug)]
pub struct Recorded {
    pub commit: Oid,
    pub unreadable: Vec<String>,
    pub summary: crate::store::message::Summary,
    pub record: crate::state::RunRecord,
}

run_spine! {
    Locked -> Planned -> Hashed -> Indexed -> Treed
           -> Reconciled -> Committed -> Published -> Recorded
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
