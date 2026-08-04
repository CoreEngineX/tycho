//! Layer 4. Pushing the store to each remote, first-contact initialisation, and
//! post-push verification of the full ref set.
//!
//! A remote is a folder. Pushing to one is a **local filesystem write** - it does not
//! touch the network, so no internet is not the same as unreachable, and `verified`
//! means the bytes are written and handed to the sync client rather than that a
//! server has them.

pub mod recovery;
pub mod state;
pub mod trust;

use crate::config::Remote;
use crate::git::refs::{PushOutcome, Refspec};
use crate::primitives::path::AbsPath;
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

/// Removes the AppleDouble sidecars macOS leaves on a volume that cannot hold an
/// extended attribute.
///
/// **Not cosmetic, and not optional.** Every exFAT and FAT32 drive is such a volume,
/// which is what `remotes.md` expects an external backup to live on, and one push to a
/// freshly formatted one produced 185 `._` files. Git globs `objects/pack/*.idx`, so
/// it opens `._pack-<hex>.idx` as a pack index and answers `non-monotonic index`;
/// every `._` under `refs/` is a `badRefName`. Measured on real hardware, twice.
///
/// Swept rather than reported, because reporting it made `doctor` fail on a healthy
/// backup on every macOS-written exFAT drive forever - and a row that is red on every
/// machine is a row people learn to skim. `doctor`'s check stays, so a sidecar that
/// survives this is still a finding.
///
/// Best-effort: a failure here is not a reason to fail a push that succeeded, and
/// `verify` immediately afterwards is what says whether the remote is sound.
#[cfg(target_os = "macos")]
pub fn sweep_sidecars(repo: &Path) {
    // `-m` deletes rather than merging the sidecar back into the file it shadows,
    // which is what we want: the data is git's, and the metadata being carried is
    // macOS's own noise.
    let _ = crate::sys::process::command(
        "dot_clean",
        &["-m", &repo.display().to_string()],
        Timeout::WORK,
    );
}

/// AppleDouble is Apple's, and so is the sweep.
#[cfg(not(target_os = "macos"))]
pub fn sweep_sidecars(_repo: &Path) {}

/// Names the cause when git's own answer does not.
///
/// Git refuses a repository on a filesystem that records no ownership - exFAT and
/// FAT32, which is what an external drive usually is. The refusal makes every command
/// after `init` fail, and the first one Tycho runs is `git config`, which answers a
/// non-repository with `fatal: not in a git directory`: true, useless, and silent
/// about ownership. Asking git directly gets the real complaint, and git's own text
/// carries the remedy with the exact path spelling `safe.directory` has to match -
/// which is backslashes here, and does not match the forward-slash form.
fn explain(repo: &Path, failure: &str) -> String {
    const GUARD: &str = "dubious ownership";
    if failure.contains(GUARD) {
        return failure.to_owned();
    }
    let Ok(out) = Git::at(repo).run(&["rev-parse", "--git-dir"], Timeout::QUICK) else {
        return failure.to_owned();
    };
    let stderr = String::from_utf8_lossy(&out.stderr);
    if !stderr.contains(GUARD) {
        return failure.to_owned();
    }
    format!(
        "git will not use {}: it is on a filesystem that records no ownership. {}",
        repo.display(),
        stderr.trim()
    )
}

/// Everything one remote needs from a run, in the order it has to happen.
///
/// A remote that is unreachable, refuses, or fails verification is an [`Observation`]
/// rather than an error - one bad destination must not stop the others - so this
/// never fails outright.
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
        Err(error) => {
            return Observation::Refused(FailureReason::Unusable(explain(
                &repo,
                &error.to_string(),
            )));
        }
    };
    if let Some(refusal) = kind.refusal() {
        return Observation::Refused(FailureReason::Unusable(refusal));
    }
    if kind == RemoteKind::Absent
        && let Err(error) = initialise(&repo)
    {
        return Observation::Refused(FailureReason::Unusable(explain(&repo, &error.to_string())));
    }

    match store.repo().push(&repo, &refspecs()) {
        Ok(PushOutcome::Accepted) => {}
        Ok(PushOutcome::Refused { detail }) => {
            return Observation::Refused(FailureReason::Rejected(detail));
        }
        Err(error) => return Observation::Refused(FailureReason::Other(error.to_string())),
    }

    // Before verification, because the sidecars are what `verify` would trip over.
    sweep_sidecars(&repo);

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

// `publish` runs classify -> initialise -> push -> verify in that order, and
// `Ensured` before `Pushed` is the one that matters: a push into an unconfigured
// remote runs `git gc` on arrival.
//
// There is deliberately no `spine!` here. One stood in this file declaring those six
// states, and not one of them was ever constructed - `publish` is a plain sequence of
// early returns with no connection to them. It read exactly like the enforced spines
// in `store::pipeline` and `restore`, which is worse than having none: it advertised
// a compile-time guarantee against the very reordering its own comment warned about,
// while providing nothing. Ordering here is enforced by `publish` being one short
// function with a single path through it, and that is what a reader should check.
