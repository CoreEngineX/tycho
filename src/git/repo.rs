//! Repository lifecycle and object writing.

use crate::primitives::encode::{FileMode, index_info_line, stdin_paths_line};
use crate::primitives::oid::{Oid, OidError};
use crate::primitives::path::{AbsPath, TreePath};
use crate::sys::process::{Git, RunError, Timeout};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

/// Neutralises every attribute that could alter bytes on the way out. It is the
/// highest-precedence attributes source, and it does not survive a mirror clone -
/// restore re-establishes it.
pub const NEUTRAL_ATTRIBUTES: &str = "* -text -diff -filter -ident -export-subst -export-ignore\n";

#[derive(Debug, thiserror::Error)]
pub enum RepoError {
    #[error(transparent)]
    Run(#[from] RunError),
    #[error("{context}: {source}")]
    Io {
        context: String,
        #[source]
        source: io::Error,
    },
    #[error("git printed an object id that could not be read: {0}")]
    Oid(#[from] OidError),
    #[error("git printed output this command cannot parse: {0}")]
    Unparsable(String),
    #[error(
        "the store at {path} {detail}; it holds gitignored content and must not be readable by others"
    )]
    Exposed { path: String, detail: String },
    #[error("hashing stopped with {remaining} paths left and no path to blame: {stderr}")]
    Stalled { remaining: usize, stderr: String },
    #[error("git refused to index a path and did not fail doing it: {detail}")]
    PathRefused { detail: String },
}

/// One outcome per planned path, so the caller cannot be handed a short list.
/// `store.md`: a run either hashes every planned path or names the ones it did not.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Hashed {
    Object(Oid),
    Unreadable { reason: String },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IndexEntry {
    pub mode: FileMode,
    pub oid: Oid,
    pub path: TreePath,
}

/// A bare repository Tycho owns.
#[derive(Clone, Debug)]
pub struct Repo {
    path: AbsPath,
}

impl Repo {
    /// Creates a store with every setting `store.md` section 2 requires. None of
    /// them may be inherited: a store created without them is silently wrong rather
    /// than broken, which is worse.
    ///
    /// # Errors
    ///
    /// If git fails, or the attributes file cannot be written.
    pub fn init_bare(path: &AbsPath) -> Result<Self, RepoError> {
        let target = path.as_path();
        Git::at(Path::new("."))
            .checked(
                &[
                    "init",
                    "--bare",
                    "--object-format=sha1",
                    "--shared=0600",
                    "-b",
                    "main",
                    &target.display().to_string(),
                ],
                Timeout::QUICK,
            )
            .map_err(RepoError::Run)?;

        let repo = Self { path: path.clone() };
        let git = repo.git();
        // Without the explicit HEAD, a store whose init.defaultBranch differs clones
        // to an empty repository.
        git.checked(&["symbolic-ref", "HEAD", "refs/heads/main"], Timeout::QUICK)?;
        for (key, value) in [
            // Plain `true` logs refs/heads/* only, leaving refs/tycho/* - the refs
            // whose movement you would need to diagnose - with no reflog at all.
            ("core.logAllRefUpdates", "always"),
            ("gc.pruneExpire", "never"),
            ("gc.reflogExpire", "never"),
            ("gc.reflogExpireUnreachable", "never"),
        ] {
            repo.set_config(key, value)?;
        }

        repo.write_attributes()?;
        Ok(repo)
    }

    /// Opens an existing store, refusing one others can read.
    ///
    /// # Errors
    ///
    /// If the path is not a repository, or its mode grants group or other access.
    pub fn open(path: &AbsPath) -> Result<Self, RepoError> {
        let repo = Self { path: path.clone() };
        refuse_if_exposed(path)?;
        repo.git()
            .checked(&["rev-parse", "--git-dir"], Timeout::QUICK)?;
        Ok(repo)
    }

    /// Names a repository without opening it, for a caller that has already decided
    /// what it is looking at - a restore source it does not own, where [`Repo::open`]'s
    /// mode check would refuse a perfectly good mirror clone.
    #[must_use]
    pub fn at_unchecked(path: &AbsPath) -> Self {
        Self { path: path.clone() }
    }

    #[must_use]
    pub fn path(&self) -> &AbsPath {
        &self.path
    }

    pub(crate) fn git(&self) -> Git<'_> {
        Git::at(self.path.as_path())
    }

    /// Writes the neutralising attributes file. Restore calls this again on a clone,
    /// because `info/attributes` lives in the directory rather than the object
    /// database and no clone carries it.
    ///
    /// # Errors
    ///
    /// If the file cannot be written.
    pub fn write_attributes(&self) -> Result<(), RepoError> {
        let info = self.path.as_path().join("info");
        fs::create_dir_all(&info).map_err(|source| RepoError::Io {
            context: format!("creating {}", info.display()),
            source,
        })?;
        let file = info.join("attributes");
        fs::write(&file, NEUTRAL_ATTRIBUTES).map_err(|source| RepoError::Io {
            context: format!("writing {}", file.display()),
            source,
        })
    }

    /// # Errors
    ///
    /// If git fails.
    pub fn set_config(&self, key: &str, value: &str) -> Result<(), RepoError> {
        self.git()
            .checked(&["config", key, value], Timeout::QUICK)?;
        Ok(())
    }

    /// # Errors
    ///
    /// If git fails or prints something unreadable.
    pub fn config(&self, key: &str) -> Result<Option<String>, RepoError> {
        let out = self.git().run(&["config", "--get", key], Timeout::QUICK)?;
        if !out.status.success() {
            return Ok(None);
        }
        Ok(Some(String::from_utf8_lossy(&out.stdout).trim().to_owned()))
    }

    /// Hashes every path into the object database, reading each file once.
    ///
    /// `--no-filters` is not optional: without it `core.autocrlf` stores a CRLF file
    /// LF-normalized and a `clean` filter replaces content wholesale.
    ///
    /// A single unreadable file makes `hash-object` stop there, so a naive caller
    /// pairs N paths against fewer than N hashes and builds a short tree that looks
    /// healthy. The batch restarts past each failure until the list is exhausted,
    /// and returns exactly one outcome per input path.
    ///
    /// # Errors
    ///
    /// If git cannot run, prints an unreadable id, or stops with paths remaining and
    /// no path to attribute the failure to.
    pub fn hash_object_batch(&self, paths: &[AbsPath]) -> Result<Vec<Hashed>, RepoError> {
        let mut outcomes = Vec::with_capacity(paths.len());
        let mut start = 0;
        while start < paths.len() {
            let out = self.git().stream(
                &["hash-object", "-w", "--no-filters", "--stdin-paths"],
                paths[start..]
                    .iter()
                    .map(|path| stdin_paths_line(path.as_path())),
                Timeout::WORK,
            )?;
            let text = String::from_utf8_lossy(&out.stdout);
            let mut hashed = 0;
            for line in text.lines() {
                outcomes.push(Hashed::Object(Oid::parse(line)?));
                hashed += 1;
            }
            let next = start + hashed;
            if out.status.success() {
                if next != paths.len() {
                    return Err(RepoError::Unparsable(format!(
                        "hash-object exited 0 after {hashed} of {} paths",
                        paths.len() - start
                    )));
                }
                break;
            }
            let stderr = String::from_utf8_lossy(&out.stderr).trim().to_owned();
            if next >= paths.len() {
                return Err(RepoError::Stalled {
                    remaining: 0,
                    stderr,
                });
            }
            outcomes.push(Hashed::Unreadable {
                reason: stderr.lines().last().unwrap_or_default().to_owned(),
            });
            start = next + 1;
        }
        Ok(outcomes)
    }

    /// Writes a blob from memory, for the small objects Tycho generates itself.
    ///
    /// # Errors
    ///
    /// If git fails or prints an unreadable id.
    pub fn hash_blob(&self, bytes: &[u8]) -> Result<Oid, RepoError> {
        let out = self.git().stream(
            &["hash-object", "-w", "-t", "blob", "--no-filters", "--stdin"],
            std::iter::once(bytes.to_vec()),
            Timeout::WORK,
        )?;
        parse_one(&out.stdout, &out.stderr, out.status.success())
    }

    /// The empty tree, computed rather than hardcoded - a literal `4b825dc6...` is
    /// correct only for SHA-1 and only until it is not.
    ///
    /// # Errors
    ///
    /// If git fails.
    pub fn empty_tree(&self) -> Result<Oid, RepoError> {
        let out = self.git().stream(
            &["hash-object", "-w", "-t", "tree", "--stdin"],
            std::iter::empty(),
            Timeout::QUICK,
        )?;
        parse_one(&out.stdout, &out.stderr, out.status.success())
    }

    /// # Errors
    ///
    /// If git fails or prints an unreadable id.
    pub fn commit_tree(
        &self,
        tree: Oid,
        parent: Option<Oid>,
        message: &str,
    ) -> Result<Oid, RepoError> {
        let tree = tree.to_string();
        let parent = parent.map(|oid| oid.to_string());
        let mut args = vec!["commit-tree", tree.as_str()];
        if let Some(parent) = parent.as_deref() {
            args.push("-p");
            args.push(parent);
        }
        args.push("-m");
        args.push(message);
        let out = self.git().run(&args, Timeout::WORK)?;
        parse_one(&out.stdout, &out.stderr, out.status.success())
    }

    /// Opens a scratch index, deleting any file already at that path.
    ///
    /// `write_tree` is a method on the returned handle rather than on `Repo`,
    /// because without `GIT_INDEX_FILE` it would silently use the store's own index
    /// and a stale entry could survive into the tree.
    ///
    /// # Errors
    ///
    /// If an existing file cannot be removed.
    pub fn scratch_index(&self, path: PathBuf) -> Result<Index<'_>, RepoError> {
        match fs::remove_file(&path) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(source) => {
                return Err(RepoError::Io {
                    context: format!("clearing {}", path.display()),
                    source,
                });
            }
        }
        Ok(Index { repo: self, path })
    }

    /// The same index without clearing it, for a caller that built it with
    /// [`Repo::scratch_index`] and is now reading it back.
    #[must_use]
    pub const fn open_index(&self, path: PathBuf) -> Index<'_> {
        Index { repo: self, path }
    }
}

/// A scratch index, built from nothing.
#[derive(Debug)]
pub struct Index<'a> {
    repo: &'a Repo,
    path: PathBuf,
}

impl Index<'_> {
    /// Adds entries in one batch. `-z` is what keeps a path starting with `"` from
    /// being dequoted and a path with a `.git` component from being discarded.
    ///
    /// # Errors
    ///
    /// If git fails.
    pub fn update(&self, entries: &[IndexEntry]) -> Result<(), RepoError> {
        if entries.is_empty() {
            return Ok(());
        }
        let out = self.repo.git().with_index(&self.path).stream(
            &["update-index", "-z", "--index-info"],
            entries
                .iter()
                .map(|entry| index_info_line(entry.mode, entry.oid, entry.path.as_path())),
            Timeout::WORK,
        )?;
        let stderr = String::from_utf8_lossy(&out.stderr);
        if !out.status.success() {
            return Err(RepoError::Unparsable(stderr.trim().to_owned()));
        }
        // `update-index` answers a path it will not hold by printing `Ignoring path`
        // and **exiting 0**. Every character NTFS reserves is in that set, `\`
        // included, so on Windows a run that trusted the status would write a tree
        // short of what it planned - or the empty tree - and report success.
        if stderr.contains("Ignoring path") {
            return Err(RepoError::PathRefused {
                detail: stderr.trim().to_owned(),
            });
        }
        Ok(())
    }

    /// # Errors
    ///
    /// If git fails or prints an unreadable id.
    pub fn write_tree(&self) -> Result<Oid, RepoError> {
        let out = self
            .repo
            .git()
            .with_index(&self.path)
            .run(&["write-tree"], Timeout::WORK)?;
        parse_one(&out.stdout, &out.stderr, out.status.success())
    }

    /// The number of entries the index holds, for the reconciliation that catches a
    /// short batch and a silently discarded path together.
    ///
    /// # Errors
    ///
    /// If git fails.
    pub fn len(&self) -> Result<usize, RepoError> {
        let out = self
            .repo
            .git()
            .with_index(&self.path)
            .checked(&["ls-files", "-z"], Timeout::WORK)?;
        Ok(out
            .stdout
            .split(|&byte| byte == 0)
            .filter(|entry| !entry.is_empty())
            .count())
    }

    /// # Errors
    ///
    /// As [`Index::len`].
    pub fn is_empty(&self) -> Result<bool, RepoError> {
        self.len().map(|count| count == 0)
    }
}

/// Refuses a store others can read, because it holds gitignored content.
#[cfg(unix)]
fn refuse_if_exposed(path: &AbsPath) -> Result<(), RepoError> {
    use std::os::unix::fs::PermissionsExt;

    let mode = fs::metadata(path.as_path())
        .map_err(|source| RepoError::Io {
            context: format!("reading {path}"),
            source,
        })?
        .permissions()
        .mode()
        & 0o777;
    if mode & 0o077 != 0 {
        return Err(RepoError::Exposed {
            path: path.to_string(),
            detail: format!("is mode {mode:04o}"),
        });
    }
    Ok(())
}

/// The NTFS equivalent: refuse a store whose DACL names any trustee other than the
/// owner, `SYSTEM` and `Administrators`.
///
/// This is a real equivalent rather than a weakened one, and the measurement that
/// says so is that a directory under the user profile carries exactly `SY`, `BA` and
/// the owner - the same reach as `0700` - while one under `C:\Users\Public` adds
/// `IU`, `SU` and `BA`-batch with modify rights, which is what `0700` exists to
/// refuse. `store.md` section 2 records both.
///
/// SDDL rather than `icacls`'s own listing because the listing prints localised
/// account names - `BUILTIN\Administrators` is `VORDEFINIERT\Administratoren` on a
/// German install - while SDDL is fixed abbreviations and raw SIDs.
#[cfg(windows)]
fn refuse_if_exposed(path: &AbsPath) -> Result<(), RepoError> {
    let owner = current_user_sid()?;
    let dacl = read_dacl(path)?;
    if let Some(detail) = exposure(&dacl, &owner) {
        return Err(RepoError::Exposed {
            path: path.to_string(),
            detail,
        });
    }
    Ok(())
}

/// Why the store is readable by someone else, or `None` if it is not.
///
/// **A volume with no access control is the case that matters most and looks like
/// the safest.** exFAT and FAT32 carry no ACLs, so Windows reports the whole
/// descriptor as `D:NO_ACCESS_CONTROL` with owner `WD` - Everyone - and a check that
/// only walks ACEs finds none to object to and passes. Measured on a removable exFAT
/// volume, which is exactly where `remotes.md` expects an external drive to be, and
/// where someone would reasonably point `store_path`.
///
/// An absent DACL is treated the same way. This function refuses anything it cannot
/// positively show is owner-only, because the alternative is proving a secret-bearing
/// store safe by failing to find evidence against it.
#[cfg(windows)]
fn exposure(dacl: &str, owner: &str) -> Option<String> {
    if dacl.contains("NO_ACCESS_CONTROL") {
        return Some(
            "is on a volume that carries no access control at all, such as exFAT or FAT32, \
             where every file is readable by anyone with the disk"
                .to_owned(),
        );
    }
    let Some(entries) = dacl.split("D:").nth(1) else {
        return Some("has no discretionary access control list to check".to_owned());
    };
    foreign_trustee(entries, owner).map(|trustee| format!("grants access to {trustee}"))
}

#[cfg(windows)]
fn current_user_sid() -> Result<String, RepoError> {
    let out =
        crate::sys::process::command("whoami", &["/user", "/fo", "csv", "/nh"], Timeout::QUICK)
            .map_err(RepoError::Run)?;
    let text = String::from_utf8_lossy(&out.stdout);
    text.rsplit(',')
        .next()
        .map(|field| field.trim().trim_matches('"').to_owned())
        .filter(|sid| sid.starts_with("S-1-"))
        .ok_or_else(|| RepoError::Unparsable(format!("whoami /user printed {}", text.trim())))
}

/// `icacls /save` is the only way to get SDDL out of a tool that starts fast enough
/// to run on every command: `Get-Acl` needs a PowerShell process, measured at 1.6s
/// against 30ms here. It writes to a file rather than stdout, hence the temporary.
#[cfg(windows)]
fn read_dacl(path: &AbsPath) -> Result<String, RepoError> {
    let target = path
        .as_path()
        .to_str()
        .ok_or_else(|| RepoError::Unparsable(format!("{path} is not representable as UTF-8")))?;
    let scratch = std::env::temp_dir().join(format!("tycho-acl-{}.sddl", std::process::id()));
    let scratch_arg = scratch.to_str().ok_or_else(|| {
        RepoError::Unparsable("the temporary directory is not representable as UTF-8".to_owned())
    })?;

    let out = crate::sys::process::command(
        "icacls",
        &[target, "/save", scratch_arg, "/q"],
        Timeout::QUICK,
    )
    .map_err(RepoError::Run)?;
    let read = fs::read(&scratch);
    let _ = fs::remove_file(&scratch);

    if !out.status.success() {
        return Err(RepoError::Unparsable(format!(
            "icacls /save: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        )));
    }
    let bytes = read.map_err(|source| RepoError::Io {
        context: format!("reading the saved ACL for {path}"),
        source,
    })?;
    Ok(decode_utf16le(&bytes))
}

/// `icacls /save` writes UTF-16LE with a BOM. An odd trailing byte is dropped rather
/// than refused: the DACL line is what matters and a truncated tail cannot be part of
/// it.
#[cfg(windows)]
fn decode_utf16le(bytes: &[u8]) -> String {
    let body = bytes.strip_prefix(&[0xff, 0xfe]).unwrap_or(bytes);
    let units: Vec<u16> = body
        .chunks_exact(2)
        .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
        .collect();
    String::from_utf16_lossy(&units)
}

/// The first trustee in the DACL that is neither the owner, `SYSTEM` nor
/// `Administrators`.
///
/// Only allow ACEs count. A deny ACE removes access rather than granting it, and an
/// inherit-only ACE - `IO` in the flags - applies to children this object does not
/// have yet, so neither can expose what is already there.
#[cfg(windows)]
fn foreign_trustee(dacl: &str, owner: &str) -> Option<String> {
    for ace in dacl.split('(').skip(1) {
        let ace = ace.split(')').next().unwrap_or_default();
        let fields: Vec<&str> = ace.split(';').collect();
        let [kind, flags, .., trustee] = fields.as_slice() else {
            continue;
        };
        if !kind.starts_with('A') || flags.contains("IO") {
            continue;
        }
        if !matches!(*trustee, "SY" | "BA") && !trustee.eq_ignore_ascii_case(owner) {
            return Some((*trustee).to_owned());
        }
    }
    None
}

fn parse_one(stdout: &[u8], stderr: &[u8], success: bool) -> Result<Oid, RepoError> {
    if !success {
        return Err(RepoError::Unparsable(
            String::from_utf8_lossy(stderr).trim().to_owned(),
        ));
    }
    Oid::parse(String::from_utf8_lossy(stdout).trim()).map_err(RepoError::Oid)
}

#[cfg(all(test, windows))]
mod windows_tests {
    use super::exposure;

    const OWNER: &str = "S-1-5-21-1111111111-2222222222-3333333333-1001";

    /// The shape a directory under the user profile has: SYSTEM, Administrators and
    /// the owner, which is the same reach `0700` grants.
    #[test]
    fn a_profile_directorys_dacl_is_not_an_exposure() {
        let dacl = format!("name\nD:(A;OICIID;FA;;;SY)(A;OICIID;FA;;;BA)(A;OICIID;FA;;;{OWNER})");
        assert_eq!(exposure(&dacl, OWNER), None, "{dacl}");
    }

    /// `C:\Users\Public` adds Interactive, Service and Batch with modify rights,
    /// which is precisely what the check exists to refuse.
    #[test]
    fn a_publicly_writable_dacl_names_the_trustee() {
        let dacl = format!(
            "name\nD:AI(A;OICIID;FA;;;BA)(A;ID;FA;;;{OWNER})(A;OICIID;FA;;;SY)\
             (A;OICIID;0x1301ff;;;IU)(A;OICIID;0x1301ff;;;SU)"
        );
        let detail = exposure(&dacl, OWNER).expect("an exposure");
        assert!(detail.contains("IU"), "{detail}");
    }

    /// The one that looks safest and is the least safe. exFAT and FAT32 have no ACLs,
    /// so there are no entries to object to - and a check that only walks entries
    /// passes a store every user of the machine can read. Measured on a real removable
    /// exFAT volume, which is where an external-drive store would live.
    #[test]
    fn a_volume_with_no_access_control_is_an_exposure_rather_than_a_clean_bill() {
        let detail = exposure("acl-probe\nD:NO_ACCESS_CONTROL", OWNER)
            .expect("a volume with no ACLs must be refused");
        assert!(detail.contains("exFAT"), "{detail}");
    }

    /// A descriptor with no DACL at all is unprovable, not proven safe.
    #[test]
    fn an_absent_dacl_is_refused_rather_than_assumed_owner_only() {
        assert!(exposure("acl-probe\n", OWNER).is_some());
    }

    /// A deny entry restricts rather than grants, and an inherit-only one applies to
    /// children this object does not have.
    #[test]
    fn deny_and_inherit_only_entries_do_not_count_as_grants() {
        let dacl = format!("name\nD:(D;;FA;;;WD)(A;OICIIOID;FA;;;CO)(A;OICIID;FA;;;{OWNER})");
        assert_eq!(exposure(&dacl, OWNER), None, "{dacl}");
    }
}
