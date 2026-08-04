//! Layer 0. Validated newtypes, the three path encodings, and refname collision
//! detection. Nothing here knows what a backup is.

pub mod bytes;
pub mod encode;
pub mod names;
pub mod oid;
pub mod path;
pub mod refs;
