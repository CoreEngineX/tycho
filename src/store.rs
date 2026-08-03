//! Layer 4. The backup store: the commit pipeline, history, and restore.

pub mod message;
pub mod pipeline;
pub mod run;

use crate::git::read::Change;
use crate::git::{Hashed as HashOutcome, IndexEntry, Repo, RepoError};
use crate::plan::{Entry, Plan};
use crate::primitives::names::RefName;
use crate::primitives::oid::Oid;
use crate::primitives::path::AbsPath;
use crate::sys::process::Timeout;
use message::Summary;
use std::path::PathBuf;

/// The one ref a backup lives on.
pub const BACKUP_REF: &str = "refs/heads/main";

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error(transparent)]
    Repo(#[from] RepoError),
    #[error(transparent)]
    Run(#[from] crate::sys::process::RunError),
    #[error("{context}: {source}")]
    Io {
        context: String,
        #[source]
        source: std::io::Error,
    },
    #[error("'{0}' is not a refname")]
    BadRef(String),
    /// The check `store.md` calls the one that catches the short-batch case, the
    /// silently-discarded-path case, and future variants at once.
    #[error(
        "the tree holds {found} entries but the run planned {planned}; refusing to publish a backup that is short of its plan"
    )]
    Short { planned: usize, found: usize },
}

/// A profile's store, plus where its scratch index lives.
#[derive(Debug)]
pub struct Store {
    repo: Repo,
    index: PathBuf,
}

impl Store {
    /// Opens the store, creating it with every setting `store.md` section 2 requires
    /// if it is not there yet.
    ///
    /// # Errors
    ///
    /// If the directory exists but is not a usable store, or git fails.
    pub fn open_or_init(path: &AbsPath) -> Result<Self, StoreError> {
        let repo = if path.as_path().join("HEAD").exists() {
            Repo::open(path)?
        } else {
            if let Some(parent) = path.as_path().parent() {
                std::fs::create_dir_all(parent).map_err(|source| StoreError::Io {
                    context: format!("creating {}", parent.display()),
                    source,
                })?;
            }
            Repo::init_bare(path)?
        };
        let index = path.as_path().join("tycho-index");
        Ok(Self { repo, index })
    }

    #[must_use]
    pub const fn repo(&self) -> &Repo {
        &self.repo
    }

    /// The store's directory, for tests that drive plain git against it - the whole
    /// point being that nothing about the store is Tycho-specific.
    #[must_use]
    pub fn path_for_test(&self) -> &std::path::Path {
        self.repo.path().as_path()
    }

    /// The commit the last backup landed on, or `None` before the first run.
    ///
    /// # Errors
    ///
    /// If git fails.
    pub fn head(&self) -> Result<Option<Oid>, StoreError> {
        let name = backup_ref()?;
        Ok(self.repo.for_each_ref(BACKUP_REF)?.get(&name).copied())
    }

    /// Hashes every planned file, returning index entries and the paths that could
    /// not be read.
    ///
    /// # Errors
    ///
    /// If git fails.
    pub fn hash(&self, plan: &Plan) -> Result<(Vec<IndexEntry>, Vec<String>), StoreError> {
        let files: Vec<&crate::plan::PlainFile> = plan
            .roots
            .iter()
            .flat_map(|root| &root.entries)
            .filter_map(|entry| match entry {
                Entry::Plain(file) => Some(file),
                Entry::Repo(_) => None,
            })
            .collect();

        let paths: Vec<AbsPath> = files.iter().map(|file| file.source.clone()).collect();
        let outcomes = self.repo.hash_object_batch(&paths)?;

        let mut entries = Vec::with_capacity(files.len());
        let mut unreadable = Vec::new();
        for (file, outcome) in files.iter().zip(outcomes) {
            match outcome {
                HashOutcome::Object(oid) => entries.push(IndexEntry {
                    mode: file.mode,
                    oid,
                    path: file.stored.clone(),
                }),
                HashOutcome::Unreadable { reason } => unreadable.push(reason),
            }
        }
        Ok((entries, unreadable))
    }

    /// Builds the index from nothing and returns how many entries it holds.
    ///
    /// # Errors
    ///
    /// If git fails.
    pub fn index(&self, entries: &[IndexEntry]) -> Result<usize, StoreError> {
        let index = self.repo.scratch_index(self.index.clone())?;
        index.update(entries)?;
        Ok(index.len()?)
    }

    /// Writes the index built by [`Store::index`] as a tree. Separate because the
    /// spine treats "the index holds the entries" and "a tree exists" as different
    /// states, and only the second may be reconciled.
    ///
    /// # Errors
    ///
    /// If git fails.
    pub fn tree(&self) -> Result<Oid, StoreError> {
        Ok(self.repo.open_index(self.index.clone()).write_tree()?)
    }

    /// Counts what the tree actually holds and refuses a shortfall.
    ///
    /// The index count is one step too early: it would miss anything `write-tree`
    /// itself dropped, and this single check is meant to catch that class too.
    ///
    /// # Errors
    ///
    /// [`StoreError::Short`] if the tree holds fewer entries than the run planned.
    pub fn reconcile(&self, tree: Oid, planned: usize) -> Result<(), StoreError> {
        let out = self.repo.git().checked(
            &["ls-tree", "-r", "-z", "--name-only", &tree.to_string()],
            Timeout::WORK,
        )?;
        let found = out
            .stdout
            .split(|&byte| byte == 0)
            .filter(|entry| !entry.is_empty())
            .count();
        if found < planned {
            return Err(StoreError::Short { planned, found });
        }
        Ok(())
    }

    /// # Errors
    ///
    /// If git fails.
    pub fn commit(&self, tree: Oid, parent: Option<Oid>, message: &str) -> Result<Oid, StoreError> {
        Ok(self.repo.commit_tree(tree, parent, message)?)
    }

    /// Moves `refs/heads/main`. The last mutation a run makes to the store, which is
    /// what leaves an interrupted run on the previous commit rather than a partial
    /// state.
    ///
    /// # Errors
    ///
    /// If git fails.
    pub fn publish(&self, commit: Oid) -> Result<(), StoreError> {
        self.repo.update_ref(&backup_ref()?, commit)?;
        Ok(())
    }

    /// Compacts. Runs after the commit lands rather than after a push, because the
    /// commit is what created the loose objects and tying compaction to a successful
    /// push means a store whose remote is offline never compacts at all.
    ///
    /// # Errors
    ///
    /// If git fails.
    pub fn gc(&self) -> Result<(), StoreError> {
        self.repo
            .git()
            .checked(&["gc", "--auto", "--quiet"], Timeout::WORK)?;
        Ok(())
    }

    /// What changed between the last backup's tree and this one.
    ///
    /// # Errors
    ///
    /// If git fails.
    pub fn changes(&self, parent: Option<Oid>, tree: Oid) -> Result<Vec<Change>, StoreError> {
        let from = match parent {
            Some(commit) => Some(self.tree_of(commit)?),
            None => None,
        };
        Ok(self.repo.diff_tree(from, tree)?)
    }

    fn tree_of(&self, commit: Oid) -> Result<Oid, StoreError> {
        let out = self.repo.git().checked(
            &["rev-parse", &format!("{commit}^{{tree}}")],
            Timeout::QUICK,
        )?;
        Ok(Oid::parse(String::from_utf8_lossy(&out.stdout).trim()).map_err(RepoError::Oid)?)
    }

    /// The store's own size on disk, recorded per run so `status` need not walk the
    /// object database on a command people run casually.
    ///
    /// # Errors
    ///
    /// If git fails.
    pub fn size(&self) -> Result<u64, StoreError> {
        let out = self
            .repo
            .git()
            .checked(&["count-objects", "-v"], Timeout::QUICK)?;
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

    /// Every backup, newest first, with what each run recorded about itself.
    ///
    /// Read out of the store's own commits rather than the state file, so this works
    /// from a bare clone on a replacement machine - which is the disaster path.
    ///
    /// # Errors
    ///
    /// If git fails.
    pub fn history(&self, limit: usize) -> Result<Vec<Backup>, StoreError> {
        let count = limit.to_string();
        let out = self.repo.git().run(
            &[
                "log",
                "-n",
                &count,
                "--format=%H%x1f%cI%x1f%B%x1e",
                BACKUP_REF,
            ],
            Timeout::WORK,
        )?;
        if !out.status.success() {
            // No commits yet is not a failure; it is a store nobody has run against.
            return Ok(Vec::new());
        }

        let text = String::from_utf8_lossy(&out.stdout);
        let mut backups = Vec::new();
        for record in text.split('\u{1e}').filter(|item| !item.trim().is_empty()) {
            let mut fields = record.trim_start().splitn(3, '\u{1f}');
            let (Some(commit), Some(when), Some(body)) =
                (fields.next(), fields.next(), fields.next())
            else {
                continue;
            };
            let Ok(commit) = Oid::parse(commit.trim()) else {
                continue;
            };
            backups.push(Backup {
                commit,
                when: when.trim().to_owned(),
                summary: message::parse(body),
            });
        }
        Ok(backups)
    }
}

/// One row of `history`.
#[derive(Clone, Debug)]
pub struct Backup {
    pub commit: Oid,
    /// RFC 3339, as git prints it with `%cI`.
    pub when: String,
    /// `None` for a commit Tycho did not write, so a hand-made one degrades rather
    /// than being reported as something it is not.
    pub summary: Option<Summary>,
}

fn backup_ref() -> Result<RefName, StoreError> {
    RefName::parse(BACKUP_REF).map_err(|_| StoreError::BadRef(BACKUP_REF.to_owned()))
}
