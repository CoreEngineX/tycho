//! Reading back what was stored.

use crate::git::repo::{Repo, RepoError};
use crate::primitives::oid::Oid;
use crate::primitives::path::TreePath;
use crate::sys::process::Timeout;
use std::ffi::OsStr;
use std::os::unix::ffi::OsStrExt;
use std::path::Path;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChangeStatus {
    Added,
    Modified,
    Deleted,
    TypeChanged,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Change {
    pub status: ChangeStatus,
    pub path: TreePath,
}

impl Repo {
    /// The bytes of a blob. Buffered in memory, which is right for one file and is
    /// why restore extracts whole trees with `archive` instead.
    ///
    /// # Errors
    ///
    /// If git fails.
    pub fn cat_file_blob(&self, oid: Oid) -> Result<Vec<u8>, RepoError> {
        let out = self
            .git()
            .checked(&["cat-file", "blob", &oid.to_string()], Timeout::WORK)?;
        Ok(out.stdout)
    }

    /// Writes a tar of `commit`, optionally scoped to `paths`.
    ///
    /// `--output` rather than stdout, because the runner buffers stdout and a
    /// restore can be gigabytes.
    ///
    /// # Errors
    ///
    /// If git fails.
    pub fn archive(&self, commit: Oid, paths: &[TreePath], out: &Path) -> Result<(), RepoError> {
        let output = format!("--output={}", out.display());
        let commit = commit.to_string();
        let mut args = vec!["archive", "--format=tar", &output, &commit];
        let rendered: Vec<String> = paths
            .iter()
            .map(|path| path.as_path().display().to_string())
            .collect();
        args.extend(rendered.iter().map(String::as_str));
        self.git().checked(&args, Timeout::WORK)?;
        Ok(())
    }

    /// What changed between two trees, one entry per file.
    ///
    /// `-r` is required or the output is directory-level and the per-file list is
    /// wrong. `from` is `None` on a first run, which compares against a computed
    /// empty tree so everything lists as added.
    ///
    /// # Errors
    ///
    /// If git fails or prints a record this cannot parse.
    pub fn diff_tree(&self, from: Option<Oid>, to: Oid) -> Result<Vec<Change>, RepoError> {
        let from = match from {
            Some(oid) => oid,
            None => self.empty_tree()?,
        };
        let out = self.git().checked(
            &[
                "diff-tree",
                "-r",
                "-z",
                "--no-commit-id",
                "--name-status",
                &from.to_string(),
                &to.to_string(),
            ],
            Timeout::WORK,
        )?;

        // -z alternates a status record and a path record, both NUL-terminated.
        let mut fields = out
            .stdout
            .split(|&byte| byte == 0)
            .filter(|field| !field.is_empty());
        let mut changes = Vec::new();
        while let Some(status) = fields.next() {
            let Some(path) = fields.next() else {
                return Err(RepoError::Unparsable(
                    "diff-tree ended with a status and no path".to_owned(),
                ));
            };
            let status = match status.first() {
                Some(b'A') => ChangeStatus::Added,
                Some(b'M') => ChangeStatus::Modified,
                Some(b'D') => ChangeStatus::Deleted,
                Some(b'T') => ChangeStatus::TypeChanged,
                _ => {
                    return Err(RepoError::Unparsable(format!(
                        "diff-tree status {}",
                        String::from_utf8_lossy(status)
                    )));
                }
            };
            let path = TreePath::parse(Path::new(OsStr::from_bytes(path)))
                .map_err(|error| RepoError::Unparsable(error.to_string()))?;
            changes.push(Change { status, path });
        }
        Ok(changes)
    }

    /// Every path in a commit's tree.
    ///
    /// `-z`, because a path is bytes: without it git C-quotes anything non-ASCII and
    /// the caller gets a name that does not exist.
    ///
    /// # Errors
    ///
    /// If git fails or prints a path this cannot parse.
    pub fn ls_tree(&self, commit: Oid) -> Result<Vec<TreePath>, RepoError> {
        let out = self.git().run(
            &[
                "ls-tree",
                "-r",
                "-z",
                "--name-only",
                "--full-tree",
                &commit.to_string(),
            ],
            Timeout::WORK,
        )?;
        if !out.status.success() {
            return Ok(Vec::new());
        }
        out.stdout
            .split(|&byte| byte == 0)
            .filter(|field| !field.is_empty())
            .map(|field| {
                TreePath::parse(Path::new(OsStr::from_bytes(field)))
                    .map_err(|error| RepoError::Unparsable(error.to_string()))
            })
            .collect()
    }

    /// One file's bytes out of a commit, by path rather than by object id.
    ///
    /// # Errors
    ///
    /// If the path is not in that commit, or git fails.
    pub fn blob_at(&self, commit: Oid, path: &Path) -> Result<Vec<u8>, RepoError> {
        let mut spec = std::ffi::OsString::from(commit.to_string());
        spec.push(":");
        spec.push(path.as_os_str());
        let spec = spec.to_string_lossy().into_owned();
        let out = self
            .git()
            .checked(&["cat-file", "blob", &spec], Timeout::WORK)?;
        Ok(out.stdout)
    }

    /// The commits in `scope` that touched `path`, newest first.
    ///
    /// This is how a **tracked and clean** file is found: it has no path in the store
    /// tree at all, because its content lives in a captured repository's own history
    /// under `refs/tycho/<key>/*`. That is the normal case, not an edge case.
    ///
    /// # Errors
    ///
    /// If git fails.
    pub fn commits_touching(
        &self,
        scope: &Scope<'_>,
        path: &Path,
        limit: usize,
    ) -> Result<Vec<Commit>, RepoError> {
        let count = limit.to_string();
        let path = path.to_string_lossy().into_owned();
        let scope = scope.render();
        let out = self.git().run(
            &[
                "log",
                "-n",
                &count,
                "--format=%H%x1f%cI%x1f%s%x1e",
                &scope,
                "--",
                &path,
            ],
            Timeout::WORK,
        )?;
        if !out.status.success() {
            return Ok(Vec::new());
        }

        let text = String::from_utf8_lossy(&out.stdout);
        let mut commits = Vec::new();
        for record in text.split('\u{1e}').filter(|item| !item.trim().is_empty()) {
            let mut fields = record.trim_start().splitn(3, '\u{1f}');
            let (Some(oid), Some(when), Some(subject)) =
                (fields.next(), fields.next(), fields.next())
            else {
                continue;
            };
            commits.push(Commit {
                oid: Oid::parse(oid.trim())?,
                when: when.trim().to_owned(),
                subject: subject.trim().to_owned(),
            });
        }
        Ok(commits)
    }

    /// Writes a bundle of `refs`, for handing history to someone without the store.
    ///
    /// # Errors
    ///
    /// If git fails, including when no ref matches - a bundle of nothing is an error
    /// in git and stays one here.
    pub fn bundle(&self, out: &Path, refs: &[String]) -> Result<(), RepoError> {
        let destination = out.display().to_string();
        let mut args = vec!["bundle", "create", &destination];
        args.extend(refs.iter().map(String::as_str));
        self.git().checked(&args, Timeout::WORK)?;
        Ok(())
    }
}

/// Which refs a log covers.
///
/// A sum type because git spells the two cases differently and confusing them is
/// silent: `--glob` appends `/*` to a pattern containing no wildcard, so passing one
/// exact ref through it yields `refs/heads/main/*`, which matches nothing and returns
/// an empty history rather than an error.
#[derive(Clone, Copy, Debug)]
pub enum Scope<'a> {
    Ref(&'a str),
    Glob(&'a str),
}

impl Scope<'_> {
    fn render(&self) -> String {
        match self {
            Self::Ref(name) => (*name).to_owned(),
            Self::Glob(pattern) => format!("--glob={pattern}"),
        }
    }
}

/// One commit of a captured repository's own history, for `history --path`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Commit {
    pub oid: Oid,
    /// RFC 3339, as git prints it with `%cI`.
    pub when: String,
    pub subject: String,
}
