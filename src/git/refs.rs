//! Refs, and moving them between repositories.

use crate::git::repo::{Repo, RepoError};
use crate::primitives::names::RefName;
use crate::primitives::oid::Oid;
use crate::sys::process::Timeout;
use std::collections::BTreeMap;
use std::fmt;
use std::path::Path;

/// `force` is a field rather than a `+` inside a string, because which refspecs may
/// be forced is the whole of D14: forced on `refs/tycho/*`, never on `refs/heads/*`.
/// A leading `+` on the wrong one destroys another machine's backup history at
/// exit 0.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Refspec {
    force: bool,
    source: String,
    destination: String,
}

impl Refspec {
    #[must_use]
    pub fn fast_forward(source: &str, destination: &str) -> Self {
        Self {
            force: false,
            source: source.to_owned(),
            destination: destination.to_owned(),
        }
    }

    #[must_use]
    pub fn forced(source: &str, destination: &str) -> Self {
        Self {
            force: true,
            source: source.to_owned(),
            destination: destination.to_owned(),
        }
    }

    #[must_use]
    pub const fn is_forced(&self) -> bool {
        self.force
    }
}

impl fmt::Display for Refspec {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let plus = if self.force { "+" } else { "" };
        write!(f, "{plus}{}:{}", self.source, self.destination)
    }
}

/// Whether the remote took the push. A refusal is an answer rather than an error:
/// a non-fast-forward on `refs/heads/*` means a second machine is pushing this
/// profile name, which the caller classifies.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PushOutcome {
    Accepted,
    Refused { detail: String },
}

impl Repo {
    /// Every ref under `prefix`, keyed by name.
    ///
    /// # Errors
    ///
    /// If git fails or prints a line this cannot parse.
    pub fn for_each_ref(&self, prefix: &str) -> Result<BTreeMap<RefName, Oid>, RepoError> {
        let out = self.git().checked(
            &["for-each-ref", "--format=%(objectname)\t%(refname)", prefix],
            Timeout::QUICK,
        )?;
        parse_ref_lines(&out.stdout)
    }

    /// # Errors
    ///
    /// If git fails.
    pub fn update_ref(&self, name: &RefName, to: Oid) -> Result<(), RepoError> {
        self.git().checked(
            &["update-ref", name.as_str(), &to.to_string()],
            Timeout::QUICK,
        )?;
        Ok(())
    }

    /// Fetches a source repository's refs into this one.
    ///
    /// `--no-tags` is load-bearing rather than tidy: without it git auto-follows
    /// tags into this repository's *own* `refs/tags/`, so two captured repositories
    /// that both tag `v1.0` fight over one ref, last fetch winning.
    ///
    /// There is deliberately no prune option. Captured history is reachable only
    /// through these refs, so a pruned ref is garbage rather than "still reachable
    /// from an older backup commit" - D13.
    ///
    /// # Errors
    ///
    /// If git fails.
    pub fn fetch_refs(&self, from: &Path, spec: &Refspec) -> Result<(), RepoError> {
        self.git().checked(
            &[
                "fetch",
                "--no-tags",
                "--quiet",
                &from.display().to_string(),
                &spec.to_string(),
            ],
            Timeout::WORK,
        )?;
        Ok(())
    }

    /// Pushes to a bare repository in a folder.
    ///
    /// `--atomic` has no parameter because it is not a choice: without it a
    /// rejection on one ref family still lets the other land, leaving the remote
    /// half-clobbered.
    ///
    /// # Errors
    ///
    /// If git cannot run at all. A refusal by the remote is [`PushOutcome::Refused`],
    /// not an error.
    pub fn push(&self, to: &Path, specs: &[Refspec]) -> Result<PushOutcome, RepoError> {
        let rendered: Vec<String> = specs.iter().map(ToString::to_string).collect();
        let mut args = vec!["push", "--atomic", "--porcelain"];
        let destination = to.display().to_string();
        args.push(&destination);
        args.extend(rendered.iter().map(String::as_str));

        let out = self.git().run(&args, Timeout::REMOTE)?;
        if out.status.success() {
            return Ok(PushOutcome::Accepted);
        }
        let mut detail = String::from_utf8_lossy(&out.stderr).trim().to_owned();
        if detail.is_empty() {
            detail = String::from_utf8_lossy(&out.stdout).trim().to_owned();
        }
        Ok(PushOutcome::Refused { detail })
    }

    /// The refs a remote currently holds, for the verification that compares the
    /// full ref set rather than just heads.
    ///
    /// # Errors
    ///
    /// If git fails or prints a line this cannot parse.
    pub fn ls_remote(&self, to: &Path) -> Result<BTreeMap<RefName, Oid>, RepoError> {
        let out = self
            .git()
            .checked(&["ls-remote", &to.display().to_string()], Timeout::REMOTE)?;
        parse_ref_lines(&out.stdout)
    }
}

/// Every ref in a repository Tycho does not own.
///
/// A free function rather than a method, because [`Repo::open`] refuses anything not
/// mode `0700` - right for a store that holds gitignored content, wrong for a source
/// repository that is simply somebody's working copy.
///
/// # Errors
///
/// If git fails or prints a line this cannot parse.
pub fn list_refs(repo: &Path) -> Result<BTreeMap<RefName, Oid>, RepoError> {
    let out = crate::sys::process::Git::at(repo).checked(
        &["for-each-ref", "--format=%(objectname)\t%(refname)"],
        Timeout::QUICK,
    )?;
    parse_ref_lines(&out.stdout)
}

/// Both `for-each-ref` and `ls-remote` print `<oid><tab><refname>`. `HEAD` and the
/// `^{}` peel lines are skipped: neither is a ref that is ever pushed, and neither
/// is a name `RefName` accepts.
fn parse_ref_lines(stdout: &[u8]) -> Result<BTreeMap<RefName, Oid>, RepoError> {
    let text = String::from_utf8_lossy(stdout);
    let mut refs = BTreeMap::new();
    for line in text.lines() {
        let Some((oid, name)) = line.split_once('\t') else {
            return Err(RepoError::Unparsable(line.to_owned()));
        };
        if !name.starts_with("refs/") || name.ends_with("^{}") {
            continue;
        }
        let name = RefName::parse(name)
            .map_err(|error| RepoError::Unparsable(format!("{name}: {error}")))?;
        refs.insert(name, Oid::parse(oid)?);
    }
    Ok(refs)
}
