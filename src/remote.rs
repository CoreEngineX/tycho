//! Layer 4. Pushing the store to each remote, first-contact initialisation, and
//! post-push verification of the full ref set.
//!
//! A remote is a folder. Pushing to one is a **local filesystem write** - it does not
//! touch the network, so no internet is not the same as unreachable, and `verified`
//! means the bytes are written and handed to the sync client rather than that a
//! server has them.

pub mod recovery;
pub mod state;

use crate::config::Remote;
use crate::git::refs::{PushOutcome, Refspec};
use crate::primitives::path::AbsPath;
use crate::spine::spine;
use crate::store::{Store, StoreError};
use crate::sys::process::{Git, Timeout};
use state::{FailureReason, Observation};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// What the push carries. `refs/heads/*` takes no `+`: Tycho's own history is
/// append-only, so a non-fast-forward there means a second machine is pushing this
/// profile name and must be rejected rather than forced through. `refs/tycho/*` keeps
/// its `+` because those legitimately track a rewritten upstream.
#[must_use]
pub fn refspecs() -> Vec<Refspec> {
    vec![
        Refspec::fast_forward("refs/heads/*", "refs/heads/*"),
        Refspec::forced("refs/tycho/*", "refs/tycho/*"),
    ]
}

#[derive(Debug, thiserror::Error)]
pub enum RemoteError {
    #[error(transparent)]
    Store(#[from] StoreError),
    #[error(transparent)]
    Repo(#[from] crate::git::repo::RepoError),
    #[error(transparent)]
    Run(#[from] crate::sys::process::RunError),
    #[error("{path} matches {count} directories, so which one is a coin flip: {matches}")]
    AmbiguousGlob {
        path: String,
        count: usize,
        matches: String,
    },
    #[error("{path} matches nothing")]
    NoGlobMatch { path: String },
    #[error("{context}: {source}")]
    Io {
        context: String,
        #[source]
        source: std::io::Error,
    },
}

/// What is at a remote path. Three-way rather than a boolean, because `git init
/// --bare` will happily initialise inside a directory holding your photos, and inside
/// a half-synced repository - orphaning its pre-existing pack.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RemoteKind {
    /// Missing, or an empty directory. Initialise and configure it.
    Absent,
    /// A usable bare repository. Push.
    Repository,
    /// Holds something else entirely: you pointed at the wrong folder.
    Foreign { found: Vec<String> },
    /// Looks like a repository with pieces missing: the sync may still be running,
    /// or it needs repair. A different problem with a different remedy, which is why
    /// it is a different variant.
    Incomplete { missing: Vec<String> },
}

impl RemoteKind {
    #[must_use]
    pub fn refusal(&self) -> Option<String> {
        match self {
            Self::Absent | Self::Repository => None,
            Self::Foreign { found } => Some(format!(
                "holds content that is not a tycho repository ({}); pointing a backup at it would \
                 initialise over somebody's files",
                found.join(", ")
            )),
            Self::Incomplete { missing } => Some(format!(
                "looks like a repository with {} missing; the sync client may still be \
                 downloading it, so this is a wait-or-repair rather than a wrong path",
                missing.join(", ")
            )),
        }
    }
}

/// Reads what is at a path without changing it.
///
/// # Errors
///
/// If the directory cannot be read.
pub fn classify(path: &Path) -> Result<RemoteKind, RemoteError> {
    if !path.exists() {
        return Ok(RemoteKind::Absent);
    }
    let mut names: Vec<String> = Vec::new();
    for entry in std::fs::read_dir(path).map_err(|source| RemoteError::Io {
        context: format!("reading {}", path.display()),
        source,
    })? {
        let entry = entry.map_err(|source| RemoteError::Io {
            context: format!("reading {}", path.display()),
            source,
        })?;
        names.push(entry.file_name().to_string_lossy().into_owned());
    }
    if names.is_empty() {
        return Ok(RemoteKind::Absent);
    }

    let missing: Vec<String> = ["HEAD", "objects", "refs"]
        .into_iter()
        .filter(|part| !names.iter().any(|name| name == part))
        .map(str::to_owned)
        .collect();

    // Nothing recognisable at all: this is somebody's folder, not a repository.
    if missing.len() == 3 {
        names.sort();
        names.truncate(4);
        return Ok(RemoteKind::Foreign { found: names });
    }
    if !missing.is_empty() {
        return Ok(RemoteKind::Incomplete { missing });
    }

    let usable = Git::at(path)
        .run(&["rev-parse", "--git-dir"], Timeout::QUICK)
        .is_ok_and(|out| out.status.success());
    if usable {
        Ok(RemoteKind::Repository)
    } else {
        Ok(RemoteKind::Incomplete {
            missing: vec!["a readable repository".to_owned()],
        })
    }
}

/// Expands the single `*` a configured remote path may carry, for the account
/// directory whose exact name you cannot predict.
///
/// **More than one match is an error, not a choice.** Directory order is not stable,
/// and on this machine `OneDrive-Personal` and `OneDrive-Work` invert
/// between readdir order and sorted order - so a first-match-wins rule would put
/// company backups in a university account that must never be written to.
///
/// # Errors
///
/// If the glob matches more than one directory, or none at all.
/// Splitting on `/` alone finds no parent in `C:\Users\me\GoogleDrive-*\Backups`, so
/// the search reads the current directory instead and every glob remote resolves to
/// nothing. `std::path::is_separator` is what knows which characters count here.
fn rsplit_separator(text: &str) -> Option<(&str, &str)> {
    text.rfind(std::path::is_separator)
        .map(|at| (&text[..at], &text[at + 1..]))
}

fn split_separator(text: &str) -> Option<(&str, &str)> {
    text.find(std::path::is_separator)
        .map(|at| (&text[..at], &text[at + 1..]))
}

pub fn resolve(path: &AbsPath) -> Result<PathBuf, RemoteError> {
    let text = path.as_path().to_string_lossy().into_owned();
    if !text.contains('*') {
        return Ok(path.as_path().to_path_buf());
    }

    let mut matches: Vec<PathBuf> = Vec::new();
    if let Some((before, after)) = text.split_once('*') {
        let (parent, prefix) = rsplit_separator(before).unwrap_or((".", before));
        let (suffix, rest) = split_separator(after).unwrap_or((after, ""));
        if let Ok(listing) = std::fs::read_dir(parent) {
            for entry in listing.flatten() {
                let name = entry.file_name().to_string_lossy().into_owned();
                if name.starts_with(prefix) && name.ends_with(suffix) {
                    let mut candidate = PathBuf::from(parent).join(&name);
                    if !rest.is_empty() {
                        candidate = candidate.join(rest);
                    }
                    matches.push(candidate);
                }
            }
        }
    }
    matches.sort();

    match matches.len() {
        1 => Ok(matches.remove(0)),
        0 => Err(RemoteError::NoGlobMatch { path: text }),
        count => Err(RemoteError::AmbiguousGlob {
            path: text,
            count,
            matches: matches
                .iter()
                .map(|p| p.display().to_string())
                .collect::<Vec<_>>()
                .join(", "),
        }),
    }
}

/// The bare repository inside the folder is named after the profile, so one folder
/// holds as many profiles as you point at it and they never interact.
#[must_use]
pub fn repo_path(folder: &Path, profile: &str) -> PathBuf {
    folder.join(format!("{profile}.git"))
}

/// Creates and configures a remote repository.
///
/// Configuration happens before any push, and the order is load-bearing:
/// `receive.autogc` is **on by default**, so a remote that receives a push first
/// would run `git gc` - rewriting packfiles inside a cloud-synced folder and
/// permanently pruning whatever a forced push orphaned. A remote is a write-once,
/// append-only replica that Tycho never compacts.
///
/// # Errors
///
/// If git fails.
pub fn initialise(repo: &Path) -> Result<(), RemoteError> {
    std::fs::create_dir_all(repo).map_err(|source| RemoteError::Io {
        context: format!("creating {}", repo.display()),
        source,
    })?;
    let git = Git::at(repo);
    git.checked(
        &["init", "--bare", "--object-format=sha1", "-b", "main", "."],
        Timeout::QUICK,
    )?;
    for (key, value) in [
        ("receive.autogc", "false"),
        ("gc.auto", "0"),
        ("receive.denyNonFastForwards", "true"),
    ] {
        git.checked(&["config", key, value], Timeout::QUICK)?;
    }
    // A remote whose HEAD dangles clones to an empty repository, and cloning a remote
    // is the disaster path.
    git.checked(&["symbolic-ref", "HEAD", "refs/heads/main"], Timeout::QUICK)?;
    Ok(())
}

/// Compares the **full ref set**, not one ref.
///
/// Checking only `refs/heads/main` lets a run print `verified` while the captured
/// repository history failed to land - and that history is nearly the whole backup.
///
/// # Errors
///
/// If git fails.
pub fn verify(store: &Store, repo: &Path) -> Result<Option<String>, RemoteError> {
    let remote = store.repo().ls_remote(repo)?;
    let mut local = BTreeMap::new();
    for prefix in ["refs/heads/", "refs/tycho/"] {
        local.extend(store.repo().for_each_ref(prefix)?);
    }

    let mut missing = Vec::new();
    for (name, oid) in &local {
        match remote.get(name) {
            Some(there) if there == oid => {}
            Some(there) => missing.push(format!("{name} is {there} there and {oid} here")),
            None => missing.push(format!("{name} is absent")),
        }
    }
    if missing.is_empty() {
        return Ok(None);
    }
    missing.truncate(4);
    Ok(Some(missing.join("; ")))
}

/// Everything one remote needs from a run, in the order it has to happen.
///
/// # Errors
///
/// Only if something outside the remote's own health fails. A remote that is
/// unreachable, refuses, or fails verification is an [`Observation`], not an error -
/// one bad destination must not stop the others.
pub fn publish(store: &Store, remote: &Remote, profile: &str) -> Observation {
    let folder = match resolve(&remote.path) {
        Ok(folder) => folder,
        Err(RemoteError::NoGlobMatch { .. }) => return Observation::Unreachable,
        Err(error) => return Observation::Refused(FailureReason::Unusable(error.to_string())),
    };
    // A missing folder whose **parent** exists is first contact: `/Volumes/T7` is
    // mounted and `/Volumes/T7/tycho` has not been made yet. A missing parent is the
    // drive not being plugged in, which is unreachable and not Tycho's to create -
    // making it would write the backup to the boot disk under the mount point.
    if !folder.exists() {
        if !folder.parent().is_some_and(Path::exists) {
            return Observation::Unreachable;
        }
        if let Err(error) = std::fs::create_dir_all(&folder) {
            return Observation::Refused(FailureReason::Unusable(format!(
                "creating {}: {error}",
                folder.display()
            )));
        }
    }

    let repo = repo_path(&folder, profile);
    let kind = match classify(&repo) {
        Ok(kind) => kind,
        Err(error) => return Observation::Refused(FailureReason::Unusable(error.to_string())),
    };
    if let Some(refusal) = kind.refusal() {
        return Observation::Refused(FailureReason::Unusable(refusal));
    }
    if kind == RemoteKind::Absent
        && let Err(error) = initialise(&repo)
    {
        return Observation::Refused(FailureReason::Unusable(error.to_string()));
    }

    match store.repo().push(&repo, &refspecs()) {
        Ok(PushOutcome::Accepted) => {}
        Ok(PushOutcome::Refused { detail }) => {
            return Observation::Refused(FailureReason::Rejected(detail));
        }
        Err(error) => return Observation::Refused(FailureReason::Other(error.to_string())),
    }

    match verify(store, &repo) {
        Ok(None) => {}
        Ok(Some(detail)) => return Observation::Refused(FailureReason::Unverified(detail)),
        Err(error) => return Observation::Refused(FailureReason::Other(error.to_string())),
    }

    let head = store
        .head()
        .ok()
        .flatten()
        .map_or_else(String::new, |oid| oid.to_string());
    Observation::Verified { head }
}

/// The order a remote is dealt with in. `Ensured` before `Pushed` is the one that
/// matters: a push into an unconfigured remote runs `git gc` on arrival.
#[derive(Debug)]
pub struct Unclassified;
#[derive(Debug)]
pub struct Classified;
#[derive(Debug)]
pub struct Ensured;
#[derive(Debug)]
pub struct Pushed;
#[derive(Debug)]
pub struct Verified;
#[derive(Debug)]
pub struct Recorded;

spine! {
    Unclassified -> Classified -> Ensured -> Pushed -> Verified -> Recorded
}
