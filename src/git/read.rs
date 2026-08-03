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
}
