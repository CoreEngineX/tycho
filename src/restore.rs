//! Layer 4. Reading a backup back out: whole-tree, single files, and captured
//! repositories with their history.
//!
//! Restore **never writes into your live tree**. It puts files somewhere you name and
//! you do the copy, because a restore that overwrote in place would be one typo away
//! from turning a one-file problem into a directory-sized one.

pub mod overlay;
pub mod resolve;
pub mod when;

use crate::capture::{Recorded, parse_repo_txt};
use crate::git::Repo;
use crate::git::refs::{Refspec, fetch_from};
use crate::metadata;
use crate::primitives::oid::Oid;
use crate::spine::spine;
use crate::store::{REPOS_PREFIX, Store, StoreError};
use crate::sys::process::{Git, Timeout};
use resolve::{Backup, ResolveError, Resolved};
use std::path::{Path, PathBuf};

#[derive(Debug, thiserror::Error)]
pub enum RestoreError {
    #[error(transparent)]
    Store(#[from] StoreError),
    #[error(transparent)]
    Resolve(#[from] ResolveError),
    #[error(transparent)]
    Repo(#[from] crate::git::RepoError),
    #[error(transparent)]
    Run(#[from] crate::sys::process::RunError),
    #[error("{context}: {source}")]
    Io {
        context: String,
        #[source]
        source: std::io::Error,
    },
    #[error("this store holds no backup at or before {0}")]
    NoBackup(String),
    #[error("{path} already has things in it; --force to restore into it anyway")]
    NotEmpty { path: String },
    #[error("extracting the backup failed and left nothing to blame it on: {detail}")]
    Extract { detail: String },
}

fn io(context: String) -> impl FnOnce(std::io::Error) -> RestoreError {
    move |source| RestoreError::Io { context, source }
}

/// What a repository's history comes back through. These four refspecs are the whole
/// of `disaster-recovery.md` step 5, in code where every test run executes them.
///
/// **All four are globs.** A glob that matches nothing is skipped silently, so a
/// repository with no tags still gets its branches.
#[must_use]
pub fn repo_refspecs(key: &str) -> Vec<Refspec> {
    vec![
        Refspec::forced(&format!("refs/tycho/{key}/heads/*"), "refs/heads/*"),
        Refspec::forced(&format!("refs/tycho/{key}/tags/*"), "refs/tags/*"),
        Refspec::forced(&format!("refs/tycho/{key}/remotes/*"), "refs/remotes/*"),
        Refspec::forced(&format!("refs/tycho/{key}/stashes/*"), "refs/tycho-stash/*"),
    ]
}

/// The stash's top entry, which is **one exact ref** rather than a pattern.
///
/// Fetched separately because an exact-ref refspec that is absent makes git abort the
/// whole fetch with `couldn't find remote ref` and write nothing at all - not the
/// branches, not the tags. On its own, failing means only that there was no stash.
///
/// Only the top entry can become `refs/stash`: git's stash stack is a reflog and
/// cannot be rebuilt from refs, so the rest arrive under `refs/tycho-stash/`.
#[must_use]
pub fn stash_refspec(key: &str) -> Refspec {
    Refspec::forced(&format!("refs/tycho/{key}/stash"), "refs/stash")
}

/// What one repository came back as.
#[derive(Clone, Debug)]
pub struct Rebuilt {
    pub key: String,
    pub at: PathBuf,
    /// The branch or sha `REPO.txt` recorded, if the checkout took.
    pub head: Option<String>,
    pub stashes: usize,
    pub overlay: usize,
    pub conflicts: Vec<overlay::Conflict>,
}

/// Everything a restore produced.
#[derive(Debug, Default)]
pub struct Done {
    pub commit: Option<Oid>,
    pub files: usize,
    pub bytes: u64,
    pub repos: Vec<Rebuilt>,
    /// A single-file restore names what answered for each path.
    pub resolved: Vec<(PathBuf, Resolved)>,
    pub bundle: Option<PathBuf>,
    /// Tree paths the extraction did not put on disk.
    ///
    /// Named rather than counted, and measured from the filesystem rather than taken
    /// from `tar`'s report: on Windows `tar` cannot create a symlink without a
    /// privilege, says so, and **carries on with the rest**, so the only honest
    /// account of what a restore produced is what is actually there.
    pub missing: Vec<String>,
    /// What the metadata manifest put back, when the backup carried one.
    ///
    /// `None` for a backup taken before manifests existed, or by a platform that
    /// records none - which is not the same as one that recorded nothing.
    pub metadata: Option<metadata::Applied>,
}

impl Done {
    #[must_use]
    pub fn conflicts(&self) -> usize {
        self.repos.iter().map(|repo| repo.conflicts.len()).sum()
    }
}

/// The store is open and the destination is usable.
#[derive(Debug)]
pub struct Opened {
    pub into: PathBuf,
}

/// A backup commit has been chosen.
#[derive(Debug)]
pub struct Selected {
    pub into: PathBuf,
    pub backup: Backup,
}

/// `info/attributes` is in place.
///
/// **It does not survive `git clone --mirror`** - it lives in the store directory,
/// not the object database - and restoring from a mirror clone is the entire disaster
/// path. Archive without it and a `.gitattributes` that was itself backed up makes
/// `git archive` drop every `export-ignore`d path and rewrite the line endings of
/// text files, **at exit 0**, while `git ls-tree` still lists the file that was never
/// written.
#[derive(Debug)]
pub struct Neutralised {
    pub into: PathBuf,
    pub backup: Backup,
}

/// The tree is on disk: plain files, and the overlays under `.tycho/`.
#[derive(Debug)]
pub struct Extracted {
    pub into: PathBuf,
    pub backup: Backup,
    pub done: Done,
}

/// Every captured repository is a repository again, with its history.
#[derive(Debug)]
pub struct Rebuilding {
    pub into: PathBuf,
    pub done: Done,
}

/// Each checkout carries its uncommitted work on top.
#[derive(Debug)]
pub struct Overlaid {
    pub done: Done,
}

spine! {
    Opened -> Selected -> Neutralised -> Extracted -> Rebuilding -> Overlaid
}

/// A restore in progress.
///
/// ```compile_fail
/// # use tycho::restore::*;
/// // Extracting before the attributes are neutralised must not compile: it is a
/// // silent partial restore that reports success.
/// fn skip(run: Restore<Selected>) -> Result<Restore<Extracted>, ()> {
///     run.advance(|_| unimplemented!())
/// }
/// ```
#[derive(Debug)]
pub struct Restore<'a, S> {
    pub store: &'a Store,
    pub state: S,
}

impl<'a> Restore<'a, Opened> {
    /// The only way in.
    ///
    /// # Errors
    ///
    /// If the destination exists and holds anything, without `force`.
    pub fn open(store: &'a Store, into: &Path, force: bool) -> Result<Self, RestoreError> {
        if into.exists() {
            let empty = std::fs::read_dir(into)
                .map_err(io(format!("reading {}", into.display())))?
                .next()
                .is_none();
            if !empty && !force {
                return Err(RestoreError::NotEmpty {
                    path: into.display().to_string(),
                });
            }
        }
        std::fs::create_dir_all(into).map_err(io(format!("creating {}", into.display())))?;
        Ok(Self {
            store,
            state: Opened {
                into: into.to_path_buf(),
            },
        })
    }
}

impl<'a, S> Restore<'a, S> {
    /// The one chokepoint, bounded on [`After`], so the ordering cannot be short-cut
    /// without declaring the short cut.
    ///
    /// # Errors
    ///
    /// Whatever `step` returns.
    pub fn advance<T: After<S>, E>(
        self,
        step: impl FnOnce(S) -> Result<T, E>,
    ) -> Result<Restore<'a, T>, E> {
        Ok(Restore {
            store: self.store,
            state: step(self.state)?,
        })
    }
}

/// What a restore is being asked for. Empty paths mean the whole backup.
#[derive(Clone, Debug, Default)]
pub struct Wanted {
    pub paths: Vec<PathBuf>,
    /// Write a bundle of the named repository's refs instead of a working tree.
    pub bundle: bool,
}

/// Restores a backup into a destination.
///
/// # Errors
///
/// If the destination is unusable, the backup does not exist, or git fails.
pub fn execute(
    store: &Store,
    into: &Path,
    at: Option<&jiff::Timestamp>,
    wanted: &Wanted,
    force: bool,
) -> Result<Done, RestoreError> {
    let run = Restore::open(store, into, force)?;

    let run = run.advance(|Opened { into }| {
        let Some(commit) = store.at(at)? else {
            return Err(RestoreError::NoBackup(
                at.map_or_else(|| "any time".to_owned(), ToString::to_string),
            ));
        };
        Ok(Selected {
            into,
            backup: Backup::read(store, commit)?,
        })
    })?;

    // Nothing may be extracted before this succeeds, which is the reason the spine is
    // typed at all.
    let run = run.advance(|Selected { into, backup }| {
        store.repo().write_attributes()?;
        Ok::<_, RestoreError>(Neutralised { into, backup })
    })?;

    let run = run.advance(|Neutralised { into, backup }| {
        let mut done = Done {
            commit: Some(backup.commit()),
            ..Done::default()
        };
        if wanted.bundle {
            done.bundle = Some(write_bundle(store, &into, &backup, wanted)?);
        } else if wanted.paths.is_empty() {
            extract_all(store, &into, &backup, &mut done)?;
        } else {
            extract_paths(store, &into, &backup, &wanted.paths, &mut done)?;
        }
        Ok::<_, RestoreError>(Extracted { into, backup, done })
    })?;

    let run = run.advance(|Extracted { into, backup, done }| {
        let mut done = done;
        // After the extraction and before anything reads the tree: `tar` writes the
        // archive's modes, which git reduced to 0644 and 0755, so until this runs a
        // restored `.env` is readable by everyone on the machine.
        done.metadata = read_manifest(&into).map(|manifest| metadata::apply(&into, &manifest));
        // A single-file or bundle restore rebuilds nothing: you asked for a file.
        if wanted.paths.is_empty() && !wanted.bundle {
            for key in backup.keys() {
                done.repos.push(rebuild(store, &into, key, &backup)?);
            }
        }
        Ok::<_, RestoreError>(Rebuilding { into, done })
    })?;

    let run = run.advance(|Rebuilding { into, done }| {
        let mut done = done;
        for repo in &mut done.repos {
            let from = into.join(REPOS_PREFIX).join(&repo.key).join("overlay");
            let applied = overlay::apply(&from, &repo.at)
                .map_err(io(format!("applying the overlay to {}", repo.at.display())))?;
            repo.overlay = applied.files;
            repo.conflicts = applied.conflicts;
        }
        Ok::<_, RestoreError>(Overlaid { done })
    })?;

    Ok(run.state.done)
}

/// Nothing to add, and adding it would do harm.
///
/// **Not because macOS has a different `tar`** - it ships the same bsdtar, which was
/// the obvious reason to think this arm was wrong. Measured instead: `Café — supplier
/// agrément.md` extracts byte-identical under `en_CA.UTF-8`, under `LC_ALL=C`, and
/// under `env -i`, which is the empty environment a launchd agent actually gets. So
/// there is no case here for the option to fix.
///
/// And passing it anyway **changes the name**: with `hdrcharset=UTF-8` the same
/// archive extracts `agre\u{301}ment` - `e` plus a combining acute - where the tree
/// holds `é`. Forcing the conversion path turns NFC into NFD, which is precisely the
/// silent mutation this whole design exists to refuse.
#[cfg(not(windows))]
fn charset_options() -> Vec<&'static str> {
    Vec::new()
}

/// Tells libarchive that the names in the archive are UTF-8, which `git archive`
/// writes and which bsdtar does not assume.
///
/// Windows ships bsdtar as `C:\Windows\System32\tar.exe`, and without this it reads
/// header names in the machine's ANSI codepage: `Café — supplier agrément.md` lands
/// as mojibake, so the file is on disk under a name nothing will ever look for.
/// Measured against a real backup, and caught by the `missing` reconciliation rather
/// than by reading, which is the whole argument for that reconciliation existing.
///
/// Asked rather than assumed, because `tar` may equally resolve to GNU tar - Git for
/// Windows ships one - and GNU tar answers `--options` with `unrecognized option`
/// and extracts nothing at all. GNU tar reads UTF-8 names correctly on its own, so
/// there is nothing to add for it.
#[cfg(windows)]
fn charset_options() -> Vec<&'static str> {
    let libarchive = crate::sys::process::command("tar", &["--version"], Timeout::QUICK)
        .is_ok_and(|out| String::from_utf8_lossy(&out.stdout).contains("bsdtar"));
    if libarchive {
        return vec!["--options", "hdrcharset=UTF-8"];
    }
    Vec::new()
}

/// The whole tree, through `git archive` and `tar`.
///
/// `--output` to a file rather than a pipe: in a pipeline the shell reports only the
/// last command's status, so a store with a missing object prints an error, extracts
/// zero files, and still exits 0.
fn extract_all(
    store: &Store,
    into: &Path,
    backup: &Backup,
    done: &mut Done,
) -> Result<(), RestoreError> {
    let tar = into.join(".tycho-restore.tar");
    store.repo().archive(backup.commit(), &[], &tar)?;

    let archive = tar.display().to_string();
    let destination = into.display().to_string();
    let mut args = charset_options();
    args.extend(["-xf", &archive, "-C", &destination]);
    let out = crate::sys::process::command("tar", &args, Timeout::WORK)?;
    std::fs::remove_file(&tar).map_err(io(format!("removing {}", tar.display())))?;

    // `command` hands back the output whatever the status, so a non-zero exit here
    // was previously discarded entirely. It is not fatal - `tar` extracts what it can
    // and reports the rest - but it must not pass for success, and what it could not
    // write is established by looking rather than by parsing its complaints.
    done.missing = backup
        .tree()
        .iter()
        .filter(|path| into.join(path.as_path()).symlink_metadata().is_err())
        .map(ToString::to_string)
        .collect();
    done.files = backup.tree().len() - done.missing.len();
    done.bytes = tree_bytes(into)?;

    if !out.status.success() && done.missing.is_empty() {
        return Err(RestoreError::Extract {
            detail: String::from_utf8_lossy(&out.stderr).trim().to_owned(),
        });
    }
    Ok(())
}

/// Named paths only, each through the three-way resolver.
fn extract_paths(
    store: &Store,
    into: &Path,
    backup: &Backup,
    paths: &[PathBuf],
    done: &mut Done,
) -> Result<(), RestoreError> {
    for wanted in paths {
        let resolved = resolve::resolve(store, backup, wanted)?;
        let bytes = match &resolved {
            Resolved::StoreFile { path } | Resolved::Overlay { path, .. } => {
                store.repo().blob_at(backup.commit(), path.as_path())?
            }
            Resolved::Tracked { commit, rest, .. } => store.repo().blob_at(*commit, rest)?,
        };

        // Under the path you asked for, not under the store's internal layout: an
        // overlay entry comes back at `A/proj/secret.env`, never at
        // `.tycho/repos/A/proj/overlay/secret.env`.
        let destination = into.join(wanted);
        if let Some(parent) = destination.parent() {
            std::fs::create_dir_all(parent)
                .map_err(io(format!("creating {}", parent.display())))?;
        }
        std::fs::write(&destination, &bytes)
            .map_err(io(format!("writing {}", destination.display())))?;

        done.files += 1;
        done.bytes += bytes.len() as u64;
        done.resolved.push((wanted.clone(), resolved));
    }
    Ok(())
}

/// A captured repository comes back as a repository, not a copy of files.
fn rebuild(
    store: &Store,
    into: &Path,
    key: &str,
    backup: &Backup,
) -> Result<Rebuilt, RestoreError> {
    let at = into.join(key);
    std::fs::create_dir_all(&at).map_err(io(format!("creating {}", at.display())))?;
    let git = Git::at(&at);
    git.checked(&["init", "-q"], Timeout::QUICK)?;

    // Not optional, and the step people trip on. `git init` leaves HEAD on an unborn
    // `refs/heads/main`, and git refuses to fetch into a branch that is checked out.
    // Parking HEAD on a name that never gets created lets the fetch write every
    // branch as a real local branch, which is what a restore should produce.
    git.checked(
        &["symbolic-ref", "HEAD", "refs/heads/__tycho_restore"],
        Timeout::QUICK,
    )?;

    let store_path = store.repo().path().as_path();
    fetch_from(&at, store_path, &repo_refspecs(key))?;
    // Allowed to fail: a repository that had no stash has no such ref, which is the
    // normal case rather than a problem.
    let _ = fetch_from(&at, store_path, &[stash_refspec(key)]);

    let recorded = read_recorded(store, backup, key)?;
    let head = recorded.head.as_ref().and_then(|head| {
        let out = git.run(&["checkout", "-q", head], Timeout::WORK).ok()?;
        out.status.success().then(|| head.clone())
    });

    Ok(Rebuilt {
        key: key.to_owned(),
        at,
        head,
        stashes: recorded.stashes,
        overlay: 0,
        conflicts: Vec::new(),
    })
}

/// Read off the extracted tree rather than out of the store, so a restore of a
/// subset carries only what that subset needs.
fn read_manifest(into: &Path) -> Option<metadata::Manifest> {
    let text = std::fs::read_to_string(into.join(metadata::MANIFEST)).ok()?;
    Some(metadata::parse(&text))
}

fn read_recorded(store: &Store, backup: &Backup, key: &str) -> Result<Recorded, RestoreError> {
    // Assembled the way `capture` writes it rather than with `join`, which separates
    // with `\` on Windows and asks git for a path the tree does not hold.
    let path = format!("{REPOS_PREFIX}{key}/REPO.txt");
    let bytes = store.repo().blob_at(backup.commit(), Path::new(&path))?;
    Ok(parse_repo_txt(&String::from_utf8_lossy(&bytes)))
}

/// `--bundle`: history in one file, for handing to someone without the store.
///
/// Built from a **restored** bare repository rather than straight from the store,
/// because a bundle of `refs/tycho/<key>/*` carries the store's internal ref names:
/// cloning it yields a repository with no branch and no HEAD. Verified - `git clone`
/// of such a bundle fails outright. The bare repository is left beside the bundle,
/// since restore deletes nothing.
fn write_bundle(
    store: &Store,
    into: &Path,
    backup: &Backup,
    wanted: &Wanted,
) -> Result<PathBuf, RestoreError> {
    let asked = wanted
        .paths
        .first()
        .map(|path| path.display().to_string())
        .unwrap_or_default();
    let key = backup
        .keys()
        .iter()
        .find(|candidate| **candidate == asked)
        .ok_or_else(|| ResolveError::NotFound(format!("{asked} is not a captured repository")))?;

    let leaf = key.rsplit('/').next().unwrap_or(key);
    let bare = into.join(format!("{leaf}.git"));
    std::fs::create_dir_all(&bare).map_err(io(format!("creating {}", bare.display())))?;

    let git = Git::at(&bare);
    git.checked(&["init", "--bare", "-q"], Timeout::QUICK)?;
    let store_path = store.repo().path().as_path();
    fetch_from(&bare, store_path, &repo_refspecs(key))?;
    let _ = fetch_from(&bare, store_path, &[stash_refspec(key)]);

    // A bundle whose HEAD dangles clones to an empty working tree, which is the same
    // trap `store.md` names for the store's own HEAD.
    if let Some(head) = read_recorded(store, backup, key)?.head {
        let target = format!("refs/heads/{head}");
        let _ = git.run(&["symbolic-ref", "HEAD", &target], Timeout::QUICK);
    }

    let out = into.join(format!("{leaf}.bundle"));
    Repo::at_unchecked(
        &crate::primitives::path::AbsPath::from_absolute(&bare).map_err(|error| {
            ResolveError::NotStorable {
                path: bare.display().to_string(),
                reason: error.to_string(),
            }
        })?,
    )
    .bundle(&out, &["--all".to_owned()])?;
    Ok(out)
}

fn tree_bytes(root: &Path) -> Result<u64, RestoreError> {
    let mut total = 0;
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let listing = std::fs::read_dir(&dir).map_err(io(format!("reading {}", dir.display())))?;
        for entry in listing.flatten() {
            let Ok(meta) = entry.path().symlink_metadata() else {
                continue;
            };
            if meta.is_dir() {
                stack.push(entry.path());
            } else {
                total += meta.len();
            }
        }
    }
    Ok(total)
}
