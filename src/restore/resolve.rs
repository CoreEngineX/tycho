//! Working out what a path given to `restore` or `history --path` actually names.
//!
//! It can be three different things and the rule is mechanical, from `cli.md`
//! section 7. Getting it wrong is not a crash: it is a restore that quietly answers
//! from the wrong half of the store.

use crate::git::read::{Commit, Scope};
use crate::primitives::oid::Oid;
use crate::primitives::path::TreePath;
use crate::store::{REPOS_PREFIX, Store, StoreError};
use std::path::{Path, PathBuf};

/// Where a path's content lives.
///
/// A sum type rather than a struct with three optionals, because each source needs
/// different things and only one of them is ever the answer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Resolved {
    /// A plain watched file, or the store's own `.tycho/` content. It is a path in
    /// the backup tree and comes straight out of it.
    StoreFile { path: TreePath },

    /// Inside a captured repository, and **uncommitted, untracked or gitignored** -
    /// so it was copied into the overlay. This is what was on disk at backup time.
    Overlay {
        key: String,
        /// The path relative to the repository root.
        rest: PathBuf,
        /// Where it sits in the backup tree.
        path: TreePath,
    },

    /// Inside a captured repository, and tracked and clean - so it has **no path in
    /// the backup tree at all**. Its content is in the object database under
    /// `refs/tycho/<key>/*`.
    ///
    /// This is the normal case, not an edge case: on a machine whose watched roots
    /// are mostly repositories, nearly every path lands here.
    Tracked {
        key: String,
        rest: PathBuf,
        /// The newest commit in that repository's captured history that touched it.
        commit: Oid,
        subject: String,
    },
}

impl Resolved {
    /// The one-line explanation `restore` and `history --path` print. "From the
    /// overlay" and "from repository history" mean different things about how current
    /// the content is, so the answer is never silent about which it was.
    #[must_use]
    pub fn source(&self) -> String {
        match self {
            Self::StoreFile { .. } => "a plain file, from the backup tree".to_owned(),
            Self::Overlay { key, .. } => {
                format!("repository {key}, uncommitted, from the overlay")
            }
            Self::Tracked { key, commit, .. } => {
                format!("repository {key}, tracked file, from {}", commit.short())
            }
        }
    }

    #[must_use]
    pub fn key(&self) -> Option<&str> {
        match self {
            Self::StoreFile { .. } => None,
            Self::Overlay { key, .. } | Self::Tracked { key, .. } => Some(key),
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ResolveError {
    #[error(transparent)]
    Store(#[from] StoreError),
    #[error("'{0}' is not in this backup")]
    NotFound(String),
    #[error("'{path}' cannot be a path in a backup: {reason}")]
    NotStorable { path: String, reason: String },
}

/// What one backup holds, read once so resolving many paths costs one `ls-tree`.
#[derive(Debug)]
pub struct Backup {
    commit: Oid,
    tree: Vec<TreePath>,
    keys: Vec<String>,
}

impl Backup {
    /// # Errors
    ///
    /// If git fails.
    pub fn read(store: &Store, commit: Oid) -> Result<Self, StoreError> {
        Ok(Self {
            commit,
            tree: store.repo().ls_tree(commit)?,
            keys: store.keys(commit)?,
        })
    }

    #[must_use]
    pub const fn commit(&self) -> Oid {
        self.commit
    }

    #[must_use]
    pub fn keys(&self) -> &[String] {
        &self.keys
    }

    #[must_use]
    pub fn tree(&self) -> &[TreePath] {
        &self.tree
    }

    fn holds(&self, path: &Path) -> bool {
        self.tree.iter().any(|entry| entry.as_path() == path)
    }

    /// The longest key that is a prefix of `path`.
    ///
    /// Longest, not first: a machine with both `CoreEngineX/org` and
    /// `CoreEngineX/org/handbook` captured must resolve a path inside the latter
    /// against the latter, or every file in a nested repository would be looked for
    /// in its parent's history and not found.
    fn repository_for(&self, path: &Path) -> Option<&str> {
        self.keys
            .iter()
            .filter(|key| path.starts_with(Path::new(key)))
            .max_by_key(|key| key.len())
            .map(String::as_str)
    }
}

/// The three-way rule.
///
/// # Errors
///
/// If the path is not in the backup at all, or git fails.
pub fn resolve(store: &Store, backup: &Backup, path: &Path) -> Result<Resolved, ResolveError> {
    let named = || path.display().to_string();

    // 1. No captured repository owns this path, so it is a plain file - and if the
    //    tree does not hold it, nothing does.
    let Some(key) = backup.repository_for(path) else {
        if backup.holds(path) {
            return Ok(Resolved::StoreFile {
                path: tree_path(path)?,
            });
        }
        return Err(ResolveError::NotFound(named()));
    };

    let rest = path
        .strip_prefix(Path::new(key))
        .unwrap_or(path)
        .to_path_buf();
    if rest.as_os_str().is_empty() {
        // The path names the repository itself, not a file in it.
        return Err(ResolveError::NotFound(named()));
    }

    // 2. The overlay, which holds what was on disk rather than what was committed.
    let overlay = Path::new(REPOS_PREFIX)
        .join(key)
        .join("overlay")
        .join(&rest);
    if backup.holds(&overlay) {
        return Ok(Resolved::Overlay {
            key: key.to_owned(),
            rest,
            path: tree_path(&overlay)?,
        });
    }

    // 3. Tracked and clean, so it is in that repository's own history.
    let commits = store
        .repo()
        .commits_touching(&Scope::Glob(&format!("refs/tycho/{key}/*")), &rest, 1)
        .map_err(StoreError::from)?;
    let Some(newest) = commits.into_iter().next() else {
        return Err(ResolveError::NotFound(named()));
    };
    let Commit { oid, subject, .. } = newest;
    Ok(Resolved::Tracked {
        key: key.to_owned(),
        rest,
        commit: oid,
        subject,
    })
}

/// Every commit that touched the path, for `history --path`.
///
/// A plain file or an overlay entry answers from the **store's** commits - the backup
/// runs. A tracked file answers from its own repository's commits, which is what you
/// want, because "the version before I broke it" is a commit in your repo, not a
/// Sunday.
///
/// # Errors
///
/// If git fails.
pub fn history(
    store: &Store,
    resolved: &Resolved,
    limit: usize,
) -> Result<Vec<Commit>, ResolveError> {
    let glob;
    let (scope, path) = match resolved {
        Resolved::StoreFile { path } => (Scope::Ref(crate::store::BACKUP_REF), path.as_path()),
        Resolved::Overlay { path, .. } => (Scope::Ref(crate::store::BACKUP_REF), path.as_path()),
        Resolved::Tracked { key, rest, .. } => {
            glob = format!("refs/tycho/{key}/*");
            (Scope::Glob(&glob), rest.as_path())
        }
    };
    Ok(store
        .repo()
        .commits_touching(&scope, path, limit)
        .map_err(StoreError::from)?)
}

fn tree_path(path: &Path) -> Result<TreePath, ResolveError> {
    TreePath::parse(path).map_err(|error| ResolveError::NotStorable {
        path: path.display().to_string(),
        reason: error.to_string(),
    })
}
