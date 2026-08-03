//! Layer 2. Typed plumbing over a bare repository, speaking `Oid` and `RefName`
//! rather than strings. The handle here is `Repo`; backup-store semantics are `store`.
//!
//! Settled invariants are structure rather than parameters: the push is always
//! atomic, the fetch never prunes and never follows tags, and the hash batch always
//! runs `--no-filters`. Each of those, left as a flag, is a call site that can be
//! wrong once and fail green.

pub mod read;
pub mod refs;
pub mod repo;

pub use repo::{Hashed, Index, IndexEntry, Repo, RepoError};
