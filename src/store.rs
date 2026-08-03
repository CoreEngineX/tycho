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

/// A file to hash: where it is, where it is stored, and what it is.
pub type Item = (
    AbsPath,
    crate::primitives::path::TreePath,
    crate::primitives::encode::FileMode,
);

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
        let items: Vec<Item> = plan
            .roots
            .iter()
            .flat_map(|root| &root.entries)
            .filter_map(|entry| match entry {
                Entry::Plain(file) => Some((file.source.clone(), file.stored.clone(), file.mode)),
                Entry::Repo(_) => None,
            })
            .collect();
        self.hash_paths(&items)
    }

    /// Hashes files that are not part of the plan's plain-file set - the overlay.
    ///
    /// # Errors
    ///
    /// If git fails.
    pub fn hash_files(
        &self,
        files: &[(AbsPath, crate::primitives::path::TreePath)],
    ) -> Result<(Vec<IndexEntry>, Vec<String>), StoreError> {
        let items: Vec<Item> = files
            .iter()
            .filter_map(|(source, stored)| {
                let mode = crate::sys::fs::classify_path(source.as_path())
                    .ok()
                    .and_then(crate::sys::fs::FileKind::mode)?;
                Some((source.clone(), stored.clone(), mode))
            })
            .collect();
        self.hash_paths(&items)
    }

    /// The one hashing path.
    ///
    /// Symlinks are hashed from their target *string* rather than by path, because
    /// `hash-object` follows a link and stores what it points at. Storing that under
    /// mode `120000` would fabricate a file on restore that never existed on the
    /// source machine, and a dangling link would fail the batch outright.
    ///
    /// # Errors
    ///
    /// If git fails.
    pub fn hash_paths(&self, items: &[Item]) -> Result<(Vec<IndexEntry>, Vec<String>), StoreError> {
        let mut entries = Vec::with_capacity(items.len());
        let mut unreadable = Vec::new();
        let mut batched = Vec::new();

        for (source, stored, mode) in items {
            if *mode != crate::primitives::encode::FileMode::Symlink {
                batched.push((source.clone(), stored.clone(), *mode));
                continue;
            }
            match std::fs::read_link(source.as_path()) {
                Ok(target) => {
                    let oid = self.repo.hash_blob(target.as_os_str().as_encoded_bytes())?;
                    entries.push(IndexEntry {
                        mode: *mode,
                        oid,
                        path: stored.clone(),
                    });
                }
                Err(error) => unreadable.push(format!("{source}: {error}")),
            }
        }

        let paths: Vec<AbsPath> = batched.iter().map(|(source, ..)| source.clone()).collect();
        for ((_, stored, mode), outcome) in batched.iter().zip(self.repo.hash_object_batch(&paths)?)
        {
            match outcome {
                HashOutcome::Object(oid) => entries.push(IndexEntry {
                    mode: *mode,
                    oid,
                    path: stored.clone(),
                }),
                HashOutcome::Unreadable { reason } => unreadable.push(reason),
            }
        }
        Ok((entries, unreadable))
    }

    /// Hashes content Tycho generates rather than reads: `REPO.txt`, the config.
    ///
    /// # Errors
    ///
    /// If git fails.
    pub fn hash_generated(
        &self,
        items: &[(crate::primitives::path::TreePath, Vec<u8>)],
    ) -> Result<Vec<IndexEntry>, StoreError> {
        let mut entries = Vec::with_capacity(items.len());
        for (path, bytes) in items {
            entries.push(IndexEntry {
                mode: crate::primitives::encode::FileMode::Regular,
                oid: self.repo.hash_blob(bytes)?,
                path: path.clone(),
            });
        }
        Ok(entries)
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

    /// How many backups there are and when the first and last landed.
    ///
    /// Two cheap git calls rather than reading the whole log, because `status` is run
    /// casually and this is its subtitle, not its subject.
    ///
    /// # Errors
    ///
    /// If git fails. A store with no commits is `None`, not an error.
    pub fn span(&self) -> Result<Option<Span>, StoreError> {
        let out = self
            .repo
            .git()
            .run(&["rev-list", "--count", BACKUP_REF], Timeout::QUICK)?;
        if !out.status.success() {
            return Ok(None);
        }
        let backups: usize = String::from_utf8_lossy(&out.stdout)
            .trim()
            .parse()
            .unwrap_or_default();
        if backups == 0 {
            return Ok(None);
        }

        let out = self.repo.git().checked(
            &["log", "--format=%cI", "--max-parents=0", BACKUP_REF],
            Timeout::WORK,
        )?;
        let oldest = String::from_utf8_lossy(&out.stdout)
            .lines()
            .next_back()
            .unwrap_or_default()
            .trim()
            .to_owned();

        let out = self.repo.git().checked(
            &["log", "-n", "1", "--format=%cI", BACKUP_REF],
            Timeout::QUICK,
        )?;
        let newest = String::from_utf8_lossy(&out.stdout).trim().to_owned();

        Ok(Some(Span {
            backups,
            oldest,
            newest,
        }))
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

/// What `status` says about a store above its remote lines.
#[derive(Clone, Debug)]
pub struct Span {
    pub backups: usize,
    /// RFC 3339, as git prints it with `%cI`.
    pub oldest: String,
    pub newest: String,
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
