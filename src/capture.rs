//! Layer 4. Hashing plain files, fetching each repository's refs into the store, and
//! building the overlay of what git history alone cannot restore.

use crate::plan::RepoHead;
use crate::primitives::names::BranchName;
use crate::primitives::oid::Oid;
use crate::sys::process::{Git, RunError, Timeout};
use std::path::Path;

/// What a repository looks like right now, for the dry run's head and state columns.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Inspection {
    pub head: RepoHead,
    pub modified: usize,
    pub untracked: usize,
    /// The repository's object database size. On a machine where nearly everything
    /// is inside a repository, this is the bulk of what a run reads, and a plan that
    /// reported only loose files would understate it by orders of magnitude.
    pub objects: u64,
}

impl Inspection {
    /// The dry run's `state` column: `clean`, or the dominant kind of dirtiness.
    #[must_use]
    pub fn state(&self) -> String {
        match (self.modified, self.untracked) {
            (0, 0) => "clean".to_owned(),
            (0, untracked) => format!("{untracked} untracked"),
            (modified, 0) => format!("{modified} modified"),
            (modified, untracked) => format!("{modified} modified, {untracked} untracked"),
        }
    }
}

/// Reads a repository's head and working-tree state without writing to it.
///
/// `--no-optional-locks` comes from the runner, and it is what makes "Tycho never
/// writes to a source" true rather than aspirational: plain `git status` rewrites
/// `.git/index` as a cache update, so without it every dry run would modify every
/// repository it looked at.
///
/// # Errors
///
/// If git cannot be run at all. A repository with no commits is `Unborn`, which is
/// an answer rather than a failure.
pub fn inspect(repo: &Path) -> Result<Inspection, RunError> {
    let git = Git::at(repo);

    let branch = git.run(
        &["symbolic-ref", "--quiet", "--short", "HEAD"],
        Timeout::QUICK,
    )?;
    let sha = git.run(&["rev-parse", "HEAD"], Timeout::QUICK)?;

    let head = if sha.status.success() {
        let oid = Oid::parse(String::from_utf8_lossy(&sha.stdout).trim()).ok();
        match (oid, branch.status.success()) {
            (Some(sha), true) => {
                let name = String::from_utf8_lossy(&branch.stdout).trim().to_owned();
                BranchName::parse(&name).map_or(RepoHead::Detached { sha }, |name| {
                    RepoHead::Branch { name, sha }
                })
            }
            (Some(sha), false) => RepoHead::Detached { sha },
            (None, _) => RepoHead::Unborn,
        }
    } else {
        RepoHead::Unborn
    };

    let status = git.run(
        &["status", "--porcelain=v1", "-z", "--untracked-files=all"],
        Timeout::WORK,
    )?;

    let mut modified = 0;
    let mut untracked = 0;
    for record in status
        .stdout
        .split(|&byte| byte == 0)
        .filter(|record| record.len() > 2)
    {
        if record.starts_with(b"??") {
            untracked += 1;
        } else {
            modified += 1;
        }
    }

    Ok(Inspection {
        head,
        modified,
        untracked,
        objects: object_bytes(&git)?,
    })
}

/// `count-objects -v` reports `size` and `size-pack` in kibibytes.
fn object_bytes(git: &Git<'_>) -> Result<u64, RunError> {
    let out = git.run(&["count-objects", "-v"], Timeout::QUICK)?;
    if !out.status.success() {
        return Ok(0);
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let mut total = 0;
    for line in text.lines() {
        if let Some(value) = line
            .strip_prefix("size: ")
            .or_else(|| line.strip_prefix("size-pack: "))
            && let Ok(kib) = value.trim().parse::<u64>()
        {
            total += kib * 1024;
        }
    }
    Ok(total)
}
