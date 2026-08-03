//! Tycho captures watched paths into a bare git store and pushes that store to bare
//! repositories in synced cloud folders and on external drives.
//!
//! A module may only depend on modules in a lower layer. `config` and `plan` do no
//! IO, which is what makes the rule tree testable with plain values. In a single
//! crate a violation is visible only as an import, so this table is the whole
//! enforcement.
//!
//! | Layer | Modules | Knows about |
//! | --- | --- | --- |
//! | 0 | `primitives` | nothing Tycho-specific |
//! | 1 | `sys` | processes and files |
//! | 2 | `git` | git, typed |
//! | 3 | `config` | the domain, purely |
//! | 4 | `plan`, `capture`, `store`, `remote`, `state` | backups |
//! | 5 | `config_edit`, `platform`, `cli` | the outside |
//!
//! `docs/architecture/overview.md` is the contract; `docs/build-plan.md` is the order
//! of work.

pub mod capture;
pub mod cli;
pub mod config;
pub mod config_edit;
pub mod git;
pub mod plan;
pub mod platform;
pub mod primitives;
pub mod remote;
pub mod state;
pub mod store;
pub mod sys;
