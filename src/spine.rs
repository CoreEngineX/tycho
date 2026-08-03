//! Compile-time ordering for a fixed sequence of steps.
//!
//! Consuming transitions - `fn(self) -> Next` - already stop a step being called
//! early, because the method only exists on the state that earned it. They do not
//! stop someone *adding* a transition that skips a state. Routing every step through
//! one `advance` bounded on `After` means a skip needs a new line in the spine
//! declaration, which is deliberate and reviewable rather than emergent.
//!
//! Each pipeline gets **its own** `After` and `Before`. That is correct rather than
//! merely convenient: "after" only means something within one pipeline, and a shared
//! trait would let a remote state claim to follow a run state.

/// Declares a pipeline's ordering, and nothing else.
///
/// A wall of `impl After<Locked> for Planned {}` lines is the pipeline's most
/// important declaration written in its least readable form. This takes the chain as
/// it appears in the architecture docs and expands the sealed marker trait, the
/// sealing impls, the pairwise ordering, and the test that pins it.
///
/// ```text
/// spine! {
///     Locked -> Planned -> Hashed -> Captured -> Indexed
/// }
/// ```
macro_rules! spine {
    ($($state:ident)->+ $(,)?) => {
        mod sealed {
            pub trait Sealed {}
        }

        /// One step of this pipeline follows another.
        ///
        /// Sealed, so nothing outside the crate can claim an adjacency, and every
        /// impl inside it comes from `spine!` - a hand-written one is a build
        /// failure, see `tests/layering.rs`.
        pub trait After<S>: sealed::Sealed {}

        /// The symmetric companion to [`After`], so a step can name its successor.
        pub trait Before<S>: sealed::Sealed {}

        $( impl sealed::Sealed for $state {} )+
        $crate::spine::spine!(@link $($state)->+);

        #[cfg(test)]
        mod spine_order {
            use super::{After, $($state),+};

            const fn assert_after<A, B: After<A>>() {}

            /// Fails to compile if the spine is reordered or a link is dropped.
            #[test]
            fn the_spine_is_the_documented_order() {
                $crate::spine::spine!(@assert $($state)->+);
            }
        }
    };

    (@link $a:ident -> $b:ident $(-> $rest:ident)*) => {
        impl After<$a> for $b {}
        impl Before<$b> for $a {}
        $crate::spine::spine!(@link $b $(-> $rest)*);
    };
    (@link $a:ident) => {};

    (@assert $a:ident -> $b:ident $(-> $rest:ident)*) => {
        assert_after::<$a, $b>();
        $crate::spine::spine!(@assert $b $(-> $rest)*);
    };
    (@assert $a:ident) => {};
}

pub(crate) use spine;
