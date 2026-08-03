//! Tycho captures watched paths into a bare git store and pushes that store to bare
//! repositories in synced cloud folders and on external drives.
//!
//! The name is Tycho Brahe's. He spent two decades at Uraniborg recording star and
//! planet positions with the naked eye; his own model of the solar system was wrong,
//! but the records outlived him and were exact enough for Kepler to derive the real
//! one from them. That is what a backup is for: data that survives whoever made it.
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
//! | 4 | `plan`, `capture`, `store`, `remote`, `restore`, `state` | backups |
//! | 5 | `config_edit`, `platform`, `service`, `doctor`, `cli` | the outside |
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
pub mod restore;
pub mod service;
pub(crate) mod spine;
pub mod state;
pub mod store;
pub mod sys;
