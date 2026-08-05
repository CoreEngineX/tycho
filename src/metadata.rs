//! Layer 4. What a git tree cannot hold: permissions, ownership and extended
//! attributes.
//!
//! **Git records one bit per file.** A tree entry is `100644`, `100755`, `120000` or
//! `040000`, so a file stored at `0600` comes back `0644` - readable by everyone on
//! the restoring machine. That is not a detail: the store deliberately keeps
//! gitignored content, which is where `.env` files and keys live, and `store.md` goes
//! to some trouble to keep the store itself private. Restoring those secrets
//! world-readable undoes the whole of it.
//!
//! `cp`, `cp -p` and `rsync -a` all preserve the mode, so the gap is Tycho's storage
//! format rather than something inherent to backing up. The answer is the same one
//! `etckeeper` reaches for: a manifest inside the tree, written at capture and
//! replayed at restore.
//!
//! ## What is deliberately not recorded
//!
//! **Modification times.** Recording one means the manifest changes whenever a file is
//! touched, so a backup that found no content change still commits a diff - and the
//! history stops being a record of what changed. `store.md` counts mtime as metadata
//! and the restore leaves it as the extraction time.
//!
//! **`com.apple.provenance`**, which macOS attaches to essentially every file, and
//! **`com.apple.quarantine`**, which marks a file as downloaded from the internet.
//! Both are the system's to manage. Restoring quarantine would make Gatekeeper treat a
//! recovered file as untrusted, which is a worse outcome than losing the attribute.

use crate::primitives::encode::{percent_component, percent_decode};
use crate::primitives::path::{AbsPath, TreePath};
use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::Path;

/// Where the manifest sits in the store's tree.
pub const MANIFEST: &str = ".tycho/metadata.tsv";

/// The system's own attributes, which belong to the machine rather than to the file.
#[cfg(unix)]
const SYSTEM_XATTRS: [&str; 2] = ["com.apple.provenance", "com.apple.quarantine"];

/// What one path carried that the tree entry could not.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Record {
    /// The full permission bits, not just the execute bit git keeps.
    pub mode: u32,
    /// By name rather than by number: uid 501 is a different person on a different
    /// machine, and a recovery onto a new machine is the case this exists for.
    pub owner: String,
    pub group: String,
    pub xattrs: Vec<(String, Vec<u8>)>,
}

/// Every record, keyed by tree path so the rendering is ordered and a diff is minimal.
pub type Manifest = BTreeMap<String, Record>;

/// What replaying the manifest managed, and what it did not.
///
/// Named rather than counted, for the same reason `restore::Done::missing` is: a
/// count reads as a complete restore of a smaller thing.
#[derive(Debug, Default)]
pub struct Applied {
    pub modes: usize,
    pub owners: usize,
    pub xattrs: usize,
    /// Paths whose owner could not be restored, which needs privilege this process
    /// does not have unless it is root.
    pub owner_refused: Vec<String>,
    pub failed: Vec<String>,
}

/// One line per fact rather than one per path.
///
/// A path with four attributes is five lines, and changing one of them changes one
/// line. Packing them onto a single line would rewrite the whole entry for any change,
/// which is a worse diff in a file that is committed on every run.
///
/// Paths are percent-encoded through the encoder refnames already use, because a
/// filename may contain a tab or a newline and a manifest that a filename can corrupt
/// is not a manifest.
#[must_use]
pub fn render(manifest: &Manifest) -> String {
    let mut out = String::new();
    for (path, record) in manifest {
        let encoded = percent_component(path.as_bytes());
        let _ = writeln!(
            out,
            "f\t{:04o}\t{}\t{}\t{encoded}",
            record.mode, record.owner, record.group
        );
        for (name, value) in &record.xattrs {
            let _ = writeln!(out, "x\t{name}\t{}\t{encoded}", hex(value));
        }
    }
    out
}

/// # Errors
///
/// Never: an unparsable line is skipped rather than failing a restore. A manifest
/// written by a newer version must not be able to stop an older one recovering the
/// files, which are the thing that matters.
#[must_use]
pub fn parse(text: &str) -> Manifest {
    let mut manifest = Manifest::new();
    for line in text.lines() {
        let fields: Vec<&str> = line.split('\t').collect();
        match fields.as_slice() {
            ["f", mode, owner, group, encoded] => {
                let Some(path) = decode(encoded) else {
                    continue;
                };
                let Ok(mode) = u32::from_str_radix(mode, 8) else {
                    continue;
                };
                let record = manifest.entry(path).or_default();
                record.mode = mode;
                record.owner = (*owner).to_owned();
                record.group = (*group).to_owned();
            }
            ["x", name, value, encoded] => {
                let Some(path) = decode(encoded) else {
                    continue;
                };
                let Some(bytes) = unhex(value) else { continue };
                manifest
                    .entry(path)
                    .or_default()
                    .xattrs
                    .push(((*name).to_owned(), bytes));
            }
            _ => {}
        }
    }
    manifest
}

fn decode(encoded: &str) -> Option<String> {
    String::from_utf8(percent_decode(encoded)?).ok()
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn unhex(text: &str) -> Option<Vec<u8>> {
    if !text.len().is_multiple_of(2) {
        return None;
    }
    (0..text.len())
        .step_by(2)
        .map(|at| u8::from_str_radix(text.get(at..at + 2)?, 16).ok())
        .collect()
}

/// Reads what the tree entries will not carry, for every path being captured.
///
/// Ownership comes back as a name, and the lookup is batched: `stat` is asked once for
/// one representative path per distinct uid/gid pair, which is one call for a whole
/// backup in practice rather than one per file. The mode itself is free - it is
/// already in the `stat` every walk performed.
///
/// The second half of the pair is every path whose mode could not be read. A path
/// missing from the manifest restores at git's `0644`, so a `0600` file silently
/// widening to world-readable is the failure this reports rather than swallows.
#[cfg(unix)]
#[must_use]
pub fn capture(items: &[(AbsPath, TreePath)]) -> (Manifest, Vec<String>) {
    use std::{
        os::unix::fs::{MetadataExt, PermissionsExt},
        path::PathBuf,
    };

    let mut manifest = Manifest::new();
    let mut missed: Vec<String> = Vec::new();
    // One representative source path per distinct owner, so the name lookup is one
    // `stat` for a whole backup rather than one per file.
    let mut ids: BTreeMap<(u32, u32), (PathBuf, Vec<String>)> = BTreeMap::new();

    let mut record = |source: &Path,
                      key: String,
                      ids: &mut BTreeMap<_, (_, Vec<String>)>,
                      missed: &mut Vec<String>| {
        let meta = match std::fs::symlink_metadata(source) {
            Ok(meta) => meta,
            Err(error) => {
                missed.push(format!(
                    "{}: permissions could not be read, so it restores 0644: {error}",
                    source.display()
                ));
                return;
            }
        };
        // A symlink's own mode is not meaningful and cannot be set without `lchmod`,
        // which macOS has and Linux does not; the target's mode is the target's
        // business.
        if meta.file_type().is_symlink() {
            return;
        }
        ids.entry((meta.uid(), meta.gid()))
            .or_insert_with(|| (source.to_path_buf(), Vec::new()))
            .1
            .push(key.clone());
        manifest.insert(
            key,
            Record {
                mode: meta.permissions().mode() & 0o7777,
                ..Record::default()
            },
        );
    };

    for (source, stored) in items {
        record(source.as_path(), stored.to_string(), &mut ids, &mut missed);
    }

    // Directories, walked up from each stored path alongside its source. The plan
    // enumerates files, so without this a `0700` directory comes back `0755` and its
    // contents are listable by anyone - the mode on the files inside is only half the
    // guarantee.
    let mut dirs: BTreeMap<String, PathBuf> = BTreeMap::new();
    for (source, stored) in items {
        let (mut src, mut dst) = (source.as_path(), stored.as_path());
        while let (Some(up_src), Some(up_dst)) = (src.parent(), dst.parent()) {
            if up_dst.as_os_str().is_empty() {
                break;
            }
            dirs.insert(up_dst.to_string_lossy().into_owned(), up_src.to_path_buf());
            src = up_src;
            dst = up_dst;
        }
    }
    for (stored, source) in &dirs {
        record(source, stored.clone(), &mut ids, &mut missed);
    }

    for (&(uid, gid), (source, paths)) in &ids {
        let (owner, group) = names(uid, gid, source);
        for path in paths {
            if let Some(entry) = manifest.get_mut(path) {
                entry.owner.clone_from(&owner);
                entry.group.clone_from(&group);
            }
        }
    }

    read_xattrs(items, &mut manifest);
    (manifest, missed)
}

/// Nothing here is representable, so the manifest is empty rather than wrong.
///
/// NTFS has no mode and no uid; its ACLs do not map onto either, and a manifest that
/// invented values would restore them onto a machine that could act on them.
#[cfg(not(unix))]
#[must_use]
pub fn capture(_items: &[(AbsPath, TreePath)]) -> (Manifest, Vec<String>) {
    (Manifest::new(), Vec::new())
}

/// The owner and group names for one id pair, asked of `stat` rather than resolved
/// from `/etc/passwd`, which on macOS is a stub that local accounts are not in.
#[cfg(unix)]
fn names(uid: u32, gid: u32, source: &Path) -> (String, String) {
    use crate::sys::process::Timeout;

    let fallback = (uid.to_string(), gid.to_string());
    let Ok(out) = crate::sys::process::command(
        "stat",
        &["-f", "%Su %Sg", &source.display().to_string()],
        Timeout::QUICK,
    ) else {
        return fallback;
    };
    let text = String::from_utf8_lossy(&out.stdout);
    let mut parts = text.split_whitespace();
    match (parts.next(), parts.next()) {
        (Some(owner), Some(group)) => (owner.to_owned(), group.to_owned()),
        _ => fallback,
    }
}

/// Two passes, because the cheap one answers for almost every file.
///
/// `xattr <path>...` lists names for many paths in one call, and most files carry
/// none worth keeping. Only the few that do pay a second call each for the exact
/// bytes, which `xattr -px` gives as hex - `xattr -l` renders binary values as a
/// hexdump meant for reading rather than for parsing.
#[cfg(unix)]
fn read_xattrs(items: &[(AbsPath, TreePath)], manifest: &mut Manifest) {
    use crate::sys::process::{Timeout, command};

    let sources: BTreeMap<String, String> = items
        .iter()
        .map(|(source, stored)| (source.as_path().display().to_string(), stored.to_string()))
        .collect();
    if sources.is_empty() {
        return;
    }

    let listing: Vec<String> = sources.keys().cloned().collect();
    let args: Vec<&str> = listing.iter().map(String::as_str).collect();
    let Ok(out) = command("xattr", &args, Timeout::WORK) else {
        return;
    };

    for line in String::from_utf8_lossy(&out.stdout).lines() {
        let Some((source, name)) = line.rsplit_once(": ") else {
            continue;
        };
        let name = name.trim();
        if SYSTEM_XATTRS.contains(&name) {
            continue;
        }
        let Some(stored) = sources.get(source) else {
            continue;
        };
        let Ok(value) = command("xattr", &["-px", name, source], Timeout::QUICK) else {
            continue;
        };
        let text = String::from_utf8_lossy(&value.stdout);
        let packed: String = text.split_whitespace().collect();
        if let Some(bytes) = unhex(&packed)
            && let Some(record) = manifest.get_mut(stored)
        {
            record.xattrs.push((name.to_owned(), bytes));
        }
    }
}

/// Replays the manifest over an extracted tree.
///
/// The mode always, because it needs no privilege and is the reason this exists. The
/// owner only when the process can - restoring one needs root, and a recovery run as
/// an ordinary user is the normal case, so a refusal is reported rather than fatal.
#[cfg(unix)]
pub fn apply(into: &Path, manifest: &Manifest) -> Applied {
    use crate::sys::process::Timeout;
    use std::os::unix::fs::PermissionsExt;

    let mut applied = Applied::default();
    let me = whoami();

    // Files first, then directories deepest-first. A directory set to `0500` before
    // its contents were touched would refuse every one of them.
    let (dirs, files): (Vec<_>, Vec<_>) = manifest
        .iter()
        .partition(|(path, _)| into.join(path).is_dir());

    for (path, record) in files.into_iter().chain(dirs.into_iter().rev()) {
        let target = into.join(path);
        if !target
            .symlink_metadata()
            .is_ok_and(|meta| !meta.is_symlink())
        {
            continue;
        }
        match std::fs::set_permissions(&target, std::fs::Permissions::from_mode(record.mode)) {
            Ok(()) => applied.modes += 1,
            Err(error) => applied.failed.push(format!("{path}: {error}")),
        }
        for (name, value) in &record.xattrs {
            let hexed = hex(value);
            let ok = crate::sys::process::command(
                "xattr",
                &["-wx", name, &hexed, &target.display().to_string()],
                Timeout::QUICK,
            )
            .is_ok_and(|out| out.status.success());
            if ok {
                applied.xattrs += 1;
            } else {
                applied.failed.push(format!("{path}: xattr {name}"));
            }
        }
        if record.owner.is_empty() || me.as_deref() == Some(record.owner.as_str()) {
            continue;
        }
        let ok = crate::sys::process::command(
            "chown",
            &[
                "-h",
                &format!("{}:{}", record.owner, record.group),
                &target.display().to_string(),
            ],
            crate::sys::process::Timeout::QUICK,
        )
        .is_ok_and(|out| out.status.success());
        if ok {
            applied.owners += 1;
        } else {
            applied.owner_refused.push(path.clone());
        }
    }
    applied
}

/// Nothing to replay, because nothing was recorded.
#[cfg(not(unix))]
pub fn apply(_into: &Path, _manifest: &Manifest) -> Applied {
    Applied::default()
}

/// Who this process is, so a file already owned by them is not handed to `chown` at
/// all - which is the common case and would otherwise report a refusal on every path.
#[cfg(unix)]
fn whoami() -> Option<String> {
    let out =
        crate::sys::process::command("id", &["-un"], crate::sys::process::Timeout::QUICK).ok()?;
    Some(String::from_utf8_lossy(&out.stdout).trim().to_owned())
}

#[cfg(test)]
mod tests {
    use super::{MANIFEST, Manifest, Record, parse, render};

    fn record(mode: u32) -> Record {
        Record {
            mode,
            owner: "me".to_owned(),
            group: "staff".to_owned(),
            xattrs: Vec::new(),
        }
    }

    /// A path with no manifest record restores at git's `0644`. Dropping the record
    /// silently is how a `0600` file comes back world-readable under a run that
    /// reported success, so the miss has to reach the caller.
    #[cfg(unix)]
    #[test]
    fn a_path_whose_mode_cannot_be_read_is_reported_rather_than_dropped() {
        use crate::primitives::path::{AbsPath, TreePath};
        use std::path::Path;

        let dir = std::env::temp_dir().join(format!("tycho-md-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("dir");
        let gone = dir.join("never-existed");
        let items = vec![(
            AbsPath::from_absolute(&gone).expect("absolute"),
            TreePath::parse(Path::new("A/never-existed")).expect("stored"),
        )];

        let (manifest, missed) = super::capture(&items);

        assert!(!manifest.contains_key("A/never-existed"));
        assert_eq!(missed.len(), 1, "{missed:?}");
        assert!(missed[0].contains("never-existed"), "{missed:?}");
        let _ = std::fs::remove_dir(&dir);
    }

    #[test]
    fn a_manifest_round_trips() {
        let mut manifest = Manifest::new();
        manifest.insert("A/.env".to_owned(), record(0o600));
        manifest.insert(
            "A/bin/deploy.sh".to_owned(),
            Record {
                xattrs: vec![("com.example.tag".to_owned(), vec![0x00, 0x01, 0xff])],
                ..record(0o700)
            },
        );

        assert_eq!(parse(&render(&manifest)), manifest);
    }

    /// A tab or a newline in a filename is legal on every Unix, and a manifest a
    /// filename can corrupt is not a manifest.
    #[test]
    fn a_path_holding_a_tab_or_a_newline_survives() {
        let mut manifest = Manifest::new();
        manifest.insert("A/a\tb\nc.md".to_owned(), record(0o644));

        let text = render(&manifest);
        assert_eq!(
            text.lines().count(),
            1,
            "the encoded path must not break the line: {text:?}"
        );
        assert_eq!(parse(&text), manifest);
    }

    /// The mode is the whole point: git stored `100644` for a file that was `0600`,
    /// so a manifest that lost the distinction would be decoration.
    #[test]
    fn the_full_mode_survives_rather_than_just_the_execute_bit() {
        let mut manifest = Manifest::new();
        manifest.insert("secret".to_owned(), record(0o600));
        manifest.insert("setuid".to_owned(), record(0o4755));

        let back = parse(&render(&manifest));
        assert_eq!(back["secret"].mode, 0o600);
        assert_eq!(back["setuid"].mode, 0o4755);
    }

    /// A newer version's line must not stop an older one restoring the files, which
    /// are the thing that matters.
    #[test]
    fn an_unknown_line_is_skipped_rather_than_failing_the_restore() {
        let text = "f\t0600\tme\tstaff\tA%2F.env\nz\tsomething\tfrom\tthe\tfuture\n";
        let manifest = parse(text);
        assert_eq!(manifest.len(), 1);
        assert_eq!(manifest["A/.env"].mode, 0o600);
    }

    #[test]
    fn the_manifest_lives_under_the_tycho_prefix() {
        assert!(MANIFEST.starts_with(crate::store::REPOS_PREFIX.trim_end_matches("repos/")));
    }
}
