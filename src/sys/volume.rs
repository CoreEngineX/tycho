//! Whether a directory is really private to the person who owns it.
//!
//! Layer 1, not layer 2, because the question is about the filesystem rather than
//! about git - and because answering it on Windows means SDDL, SIDs and `icacls`,
//! which is platform knowledge that has no business in a module that shells out to
//! `git`.
//!
//! The store holds gitignored content: `.env` files, keys, whatever the working tree
//! excluded and the backup deliberately keeps. `store.md` promises it is readable by
//! nobody but its owner, and this is the whole of that promise.

use std::path::Path;

#[derive(Debug, thiserror::Error)]
pub enum VolumeError {
    #[error("{context}: {source}")]
    Io {
        context: String,
        #[source]
        source: std::io::Error,
    },
    #[error("{0}")]
    Unreadable(String),
}

fn io(context: String) -> impl FnOnce(std::io::Error) -> VolumeError {
    move |source| VolumeError::Io { context, source }
}

/// Why `path` can be read by someone other than its owner, or `None` if it cannot.
///
/// Phrased as a reason rather than a bool so the refusal can say which of the several
/// quite different things went wrong.
///
/// # Errors
///
/// If the path cannot be read, or the platform's own answer cannot be parsed.
#[cfg(unix)]
pub fn exposure(path: &Path) -> Result<Option<String>, VolumeError> {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};

    let meta = std::fs::metadata(path).map_err(io(format!("reading {}", path.display())))?;
    let mode = meta.permissions().mode() & 0o777;
    if mode & 0o077 != 0 {
        return Ok(Some(format!("is mode {mode:04o}")));
    }

    // **The mode above is a lie on a volume that records no ownership**, and it is the
    // most reassuring lie available: a store on exFAT reads back as exactly `0700`
    // whatever you set, because `chmod` there succeeds and changes nothing. Measured -
    // `chmod 777` then `chmod 000` on a real exFAT volume both leave `drwx------`. The
    // kernel synthesises owner and mode from the mount, so every account on the
    // machine sees itself as the owner and the check passes for all of them.
    //
    // This is the same case the Windows arm refuses as `NO_ACCESS_CONTROL`, and it is
    // reachable here in one more way besides exFAT and FAT32: `noowners` is what
    // Disk Utility's "Ignore ownership on this volume" sets, and it is the default for
    // removable volumes whatever the filesystem - so an HFS+ or APFS external drive
    // reaches it too.
    let root = std::fs::metadata("/").map_err(io("reading /".to_owned()))?;
    if meta.dev() == root.dev() {
        // The boot volume enforces ownership, so there is nothing to ask and no
        // subprocess to pay for on the ordinary path.
        return Ok(None);
    }
    if ignores_ownership(path)? {
        return Ok(Some(
            "is on a volume mounted `noowners`, such as exFAT, FAT32, or any external \
             drive with \"Ignore ownership\" set, where the mode above is synthesised \
             per-reader and every account on the machine can read it"
                .to_owned(),
        ));
    }
    Ok(None)
}

/// Whether the volume holding `path` records ownership at all.
///
/// Public because a **remote** holds the same gitignored content as the store and gets
/// no exposure check: refusing one would ban the cross-platform external drive
/// entirely, since exFAT is the only filesystem both macOS and Windows can write and
/// it records nothing. `doctor` warns instead, which leaves the choice where it
/// belongs.
///
/// # Errors
///
/// If `mount` cannot be run or fails.
#[cfg(unix)]
pub fn records_ownership(path: &Path) -> Result<bool, VolumeError> {
    let root = std::fs::metadata("/").map_err(io("reading /".to_owned()))?;
    let here = std::fs::metadata(path).map_err(io(format!("reading {}", path.display())))?;
    use std::os::unix::fs::MetadataExt as _;
    if here.dev() == root.dev() {
        return Ok(true);
    }
    ignores_ownership(path).map(|ignores| !ignores)
}

/// NTFS records ownership; exFAT and FAT32 are caught by the DACL check instead, which
/// sees them as `NO_ACCESS_CONTROL`.
///
/// # Errors
///
/// Never.
#[cfg(windows)]
pub fn records_ownership(path: &Path) -> Result<bool, VolumeError> {
    Ok(dacl_records_ownership(&read_dacl(path)?))
}

/// Whether a security descriptor comes from a filesystem that records ownership.
///
/// **A narrower question than [`exposure`]'s, and they were the same call.**
/// `exposure` answers "can somebody other than the owner read this", which is right
/// for the store and wrong here: it also says `Some` when the DACL simply grants a
/// foreign trustee, which is what an ordinary NTFS folder with inherited ACLs looks
/// like. Routing this through it refused every such folder as "a volume that records
/// no ownership" and demanded `--trust-ownership` for a volume that records it fine.
///
/// The unix arm asks only whether the mount ignores ownership, so this is what makes
/// one name mean one thing on both platforms.
///
/// Compiled off Windows too, which is what lets it be tested anywhere rather than
/// only on the machine that already agrees with it.
#[cfg(any(windows, test))]
fn dacl_records_ownership(dacl: &str) -> bool {
    !dacl.contains("NO_ACCESS_CONTROL")
}

#[cfg(test)]
mod records_ownership_tests {
    use super::dacl_records_ownership;

    /// What the question is actually for: exFAT and FAT32 carry no ACLs, so Windows
    /// reports the whole descriptor this way.
    #[test]
    fn a_volume_with_no_access_control_records_no_ownership() {
        assert!(!dacl_records_ownership("name\nD:NO_ACCESS_CONTROL"));
    }

    /// The regression. A folder granting a foreign trustee is an ordinary NTFS folder
    /// with inherited ACLs, and routing this through `exposure` called it a volume
    /// that records no ownership - refusing it until `--trust-ownership` was passed
    /// for a volume that records ownership perfectly well.
    #[test]
    fn a_folder_readable_by_others_still_records_ownership() {
        let dacl = "name\nD:AI(A;OICIID;FA;;;S-1-5-32-544)(A;OICIID;FA;;;S-1-5-18)";
        assert!(dacl_records_ownership(dacl), "{dacl}");
    }
}

/// Whether the volume holding `path` is mounted `noowners`.
///
/// Asked of `mount` because there is no way to reach `statfs(2)`'s `f_flags` from
/// `std`, and `unsafe_code = "forbid"` rules out reaching for it directly. One
/// subprocess at about 5ms, paid only when the store is not on the boot volume -
/// cheaper than the `icacls` the Windows arm pays on every command.
#[cfg(unix)]
fn ignores_ownership(path: &Path) -> Result<bool, VolumeError> {
    Ok(mounted_noowners(&mount_listing()?, path))
}

#[cfg(unix)]
fn mount_listing() -> Result<String, VolumeError> {
    let out = crate::sys::process::command("mount", &[], crate::sys::process::Timeout::QUICK)
        .map_err(|error| VolumeError::Unreadable(error.to_string()))?;
    if !out.status.success() {
        return Err(VolumeError::Unreadable(format!(
            "mount: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        )));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// The longest mount point containing `path`, with its options.
///
/// Longest rather than first, because `/` contains everything and would answer for a
/// volume mounted under it. Compared by component so `/Volumes/Backup` does not match
/// `/Volumes/BackupOld`.
#[cfg(unix)]
fn deepest_mount<'a>(listing: &'a str, path: &Path) -> Option<(&'a Path, &'a str)> {
    let mut best: Option<(usize, &Path, &str)> = None;
    for line in listing.lines() {
        let Some((_, rest)) = line.split_once(" on ") else {
            continue;
        };
        let Some((point, options)) = rest.rsplit_once(" (") else {
            continue;
        };
        let point = Path::new(point);
        if !path.starts_with(point) {
            continue;
        }
        let depth = point.components().count();
        if best.is_none_or(|(deepest, ..)| depth > deepest) {
            // The closing paren belongs to the line, not to the last option, and an
            // option is often last: without this `noowners` reads as `noowners)`.
            best = Some((depth, point, options.trim_end_matches(')')));
        }
    }
    best.map(|(_, point, options)| (point, options))
}

#[cfg(unix)]
fn mounted_noowners(listing: &str, path: &Path) -> bool {
    deepest_mount(listing, path)
        .is_some_and(|(_, options)| options.split(", ").any(|item| item == "noowners"))
}

/// Whether the volume holding `path` journals its metadata.
///
/// The distinction that decides whether an interrupted write is recoverable. APFS,
/// HFS+ and NTFS log a metadata change before making it, so a rename cut off halfway
/// is replayed or rolled back at mount. exFAT and FAT32 log nothing, so it stays
/// broken - which is what turns a background writer like Spotlight from wasteful into
/// dangerous.
///
/// # Errors
///
/// If `mount` cannot be run or fails.
#[cfg(unix)]
pub fn is_journaled(path: &Path) -> Result<bool, VolumeError> {
    let listing = mount_listing()?;
    Ok(deepest_mount(&listing, path)
        .is_some_and(|(_, options)| options.split(", ").any(|item| item == "journaled")))
}

/// NTFS journals; the volumes that do not are caught by the DACL check instead.
///
/// # Errors
///
/// Never.
#[cfg(windows)]
pub fn is_journaled(_path: &Path) -> Result<bool, VolumeError> {
    Ok(true)
}

/// Where the volume holding `path` is mounted.
///
/// # Errors
///
/// If `mount` cannot be run or fails.
#[cfg(unix)]
pub fn mount_point(path: &Path) -> Result<Option<std::path::PathBuf>, VolumeError> {
    let listing = mount_listing()?;
    Ok(deepest_mount(&listing, path).map(|(point, _)| point.to_path_buf()))
}

/// Not asked on Windows: `.Spotlight-V100` is Apple's, and the hazard it carries is
/// too.
///
/// # Errors
///
/// Never.
#[cfg(windows)]
pub fn mount_point(_path: &Path) -> Result<Option<std::path::PathBuf>, VolumeError> {
    Ok(None)
}

/// The NTFS equivalent: a store whose DACL names any trustee other than the owner,
/// `SYSTEM` and `Administrators`.
///
/// A real equivalent rather than a weakened one, and the measurement that says so is
/// that a directory under the user profile carries exactly `SY`, `BA` and the owner -
/// the same reach as `0700` - while one under `C:\Users\Public` adds `IU`, `SU` and
/// `BA`-batch with modify rights, which is what `0700` exists to refuse.
///
/// SDDL rather than `icacls`'s own listing because the listing prints localised
/// account names - `BUILTIN\Administrators` is `VORDEFINIERT\Administratoren` on a
/// German install - while SDDL is fixed abbreviations and raw SIDs.
///
/// # Errors
///
/// If `whoami` or `icacls` cannot be run, or their output cannot be parsed.
#[cfg(windows)]
pub fn exposure(path: &Path) -> Result<Option<String>, VolumeError> {
    let owner = current_user_sid()?;
    let dacl = read_dacl(path)?;
    Ok(from_dacl(&dacl, &owner))
}

/// Why the store is readable by someone else, or `None` if it is not.
///
/// **A volume with no access control is the case that matters most and looks like the
/// safest.** exFAT and FAT32 carry no ACLs, so Windows reports the whole descriptor as
/// `D:NO_ACCESS_CONTROL` with owner `WD` - Everyone - and a check that only walks ACEs
/// finds none to object to and passes. Measured on a removable exFAT volume, which is
/// exactly where `remotes.md` expects an external drive to be, and where someone would
/// reasonably point `store_path`.
///
/// An absent DACL is treated the same way. This refuses anything it cannot positively
/// show is owner-only, because the alternative is proving a secret-bearing store safe
/// by failing to find evidence against it.
#[cfg(any(windows, test))]
fn from_dacl(dacl: &str, owner: &str) -> Option<String> {
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
fn current_user_sid() -> Result<String, VolumeError> {
    let out = crate::sys::process::command(
        "whoami",
        &["/user", "/fo", "csv", "/nh"],
        crate::sys::process::Timeout::QUICK,
    )
    .map_err(|error| VolumeError::Unreadable(error.to_string()))?;
    let text = String::from_utf8_lossy(&out.stdout);
    text.rsplit(',')
        .next()
        .map(|field| field.trim().trim_matches('"').to_owned())
        .filter(|sid| sid.starts_with("S-1-"))
        .ok_or_else(|| VolumeError::Unreadable(format!("whoami /user printed {}", text.trim())))
}

/// `icacls /save` is the only way to get SDDL out of a tool that starts fast enough to
/// run on every command: `Get-Acl` needs a PowerShell process, measured at 1.6s against
/// 30ms here. It writes to a file rather than stdout, hence the temporary.
#[cfg(windows)]
fn read_dacl(path: &Path) -> Result<String, VolumeError> {
    let target = path.to_str().ok_or_else(|| {
        VolumeError::Unreadable(format!("{} is not representable as UTF-8", path.display()))
    })?;
    let scratch = std::env::temp_dir().join(format!("tycho-acl-{}.sddl", std::process::id()));
    let scratch_arg = scratch.to_str().ok_or_else(|| {
        VolumeError::Unreadable("the temporary directory is not representable as UTF-8".to_owned())
    })?;

    let out = crate::sys::process::command(
        "icacls",
        &[target, "/save", scratch_arg, "/q"],
        crate::sys::process::Timeout::QUICK,
    )
    .map_err(|error| VolumeError::Unreadable(error.to_string()))?;
    let read = std::fs::read(&scratch);
    let _ = std::fs::remove_file(&scratch);

    if !out.status.success() {
        return Err(VolumeError::Unreadable(format!(
            "icacls /save: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        )));
    }
    let bytes = read.map_err(io(format!("reading the saved ACL for {}", path.display())))?;
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
///
/// **`LA` is allowed for the same reason `BA` is.** SDDL writes well-known accounts as
/// abbreviations rather than as SIDs, so the built-in Administrator arrives as `LA`
/// and never matches an owner string that is a raw SID - which refused a store owned
/// by the very account that created it, whenever that account was the built-in one.
/// It is inside the `Administrators` grant this already allows, and an administrator
/// reads through any DACL regardless.
///
/// Compiled off Windows so its tests run anywhere. They were `cfg(all(test, windows))`
/// and this cost a CI round to find.
#[cfg(any(windows, test))]
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
        if !matches!(*trustee, "SY" | "BA" | "LA") && !trustee.eq_ignore_ascii_case(owner) {
            return Some((*trustee).to_owned());
        }
    }
    None
}

#[cfg(all(test, unix))]
mod unix_tests {
    use super::mounted_noowners;
    use std::path::Path;

    /// Real `mount` output from this machine, with an exFAT disk image attached.
    const LISTING: &str = "\
/dev/disk3s1s1 on / (apfs, sealed, local, read-only, journaled)
/dev/disk3s5 on /System/Volumes/Data (apfs, local, journaled, nobrowse)
/dev/disk17s1 on /Volumes/TYCHOEX (exfat, local, nodev, nosuid, noowners, noatime, fskit)
/dev/disk20s1 on /Volumes/Owned (hfs, local, nodev, nosuid, journaled)
/dev/disk21s1 on /Volumes/OwnedOld (hfs, local, nodev, nosuid, noowners)
";

    #[test]
    fn an_exfat_volume_is_recognised_as_ignoring_ownership() {
        assert!(mounted_noowners(
            LISTING,
            Path::new("/Volumes/TYCHOEX/store.git")
        ));
    }

    #[test]
    fn a_volume_that_records_ownership_is_not() {
        assert!(!mounted_noowners(
            LISTING,
            Path::new("/Volumes/Owned/store.git")
        ));
    }

    /// `/` contains every path, so a first match or a plain prefix test would answer
    /// for the boot volume and never see the mount the store is actually on.
    #[test]
    fn the_deepest_containing_mount_point_answers_rather_than_the_first() {
        assert!(!mounted_noowners(
            LISTING,
            Path::new("/System/Volumes/Data/Users/me/store.git")
        ));
    }

    /// A component-wise comparison, or `/Volumes/Owned` claims `/Volumes/OwnedOld`.
    #[test]
    fn a_mount_point_does_not_claim_a_sibling_that_merely_starts_with_it() {
        assert!(mounted_noowners(
            LISTING,
            Path::new("/Volumes/OwnedOld/store.git")
        ));
    }
}

#[cfg(test)]
mod dacl_tests {
    use super::from_dacl;

    const OWNER: &str = "S-1-5-21-1111111111-2222222222-3333333333-1001";

    /// SDDL writes well-known accounts as abbreviations rather than as SIDs, so the
    /// built-in Administrator arrives as `LA` and never matched an owner string that
    /// is a raw SID. A store created by that account was then refused as exposed to
    /// somebody else - the somebody else being itself. Observed on a CI runner, whose
    /// user is that account.
    #[test]
    fn the_built_in_administrator_is_not_a_foreign_trustee() {
        let dacl = format!("name\nD:(A;OICIID;FA;;;SY)(A;OICIID;FA;;;LA)(A;OICIID;FA;;;{OWNER})");
        assert_eq!(from_dacl(&dacl, OWNER), None, "{dacl}");
    }

    /// The shape a directory under the user profile has: SYSTEM, Administrators and
    /// the owner, which is the same reach `0700` grants.
    #[test]
    fn a_profile_directorys_dacl_is_not_an_exposure() {
        let dacl = format!("name\nD:(A;OICIID;FA;;;SY)(A;OICIID;FA;;;BA)(A;OICIID;FA;;;{OWNER})");
        assert_eq!(from_dacl(&dacl, OWNER), None, "{dacl}");
    }

    /// `C:\Users\Public` adds Interactive, Service and Batch with modify rights,
    /// which is precisely what the check exists to refuse.
    #[test]
    fn a_publicly_writable_dacl_names_the_trustee() {
        let dacl = format!(
            "name\nD:AI(A;OICIID;FA;;;BA)(A;ID;FA;;;{OWNER})(A;OICIID;FA;;;SY)\
             (A;OICIID;0x1301ff;;;IU)(A;OICIID;0x1301ff;;;SU)"
        );
        let detail = from_dacl(&dacl, OWNER).expect("an exposure");
        assert!(detail.contains("IU"), "{detail}");
    }

    /// The one that looks safest and is the least safe. exFAT and FAT32 have no ACLs,
    /// so there are no entries to object to - and a check that only walks entries
    /// passes a store every user of the machine can read. Measured on a real removable
    /// exFAT volume, which is where an external-drive store would live.
    #[test]
    fn a_volume_with_no_access_control_is_an_exposure_rather_than_a_clean_bill() {
        let detail = from_dacl("acl-probe\nD:NO_ACCESS_CONTROL", OWNER)
            .expect("a volume with no ACLs must be refused");
        assert!(detail.contains("exFAT"), "{detail}");
    }

    /// A descriptor with no DACL at all is unprovable, not proven safe.
    #[test]
    fn an_absent_dacl_is_refused_rather_than_assumed_owner_only() {
        assert!(from_dacl("acl-probe\n", OWNER).is_some());
    }

    /// A deny entry restricts rather than grants, and an inherit-only one applies to
    /// children this object does not have.
    #[test]
    fn deny_and_inherit_only_entries_do_not_count_as_grants() {
        let dacl = format!("name\nD:(D;;FA;;;WD)(A;OICIIOID;FA;;;CO)(A;OICIID;FA;;;{OWNER})");
        assert_eq!(from_dacl(&dacl, OWNER), None, "{dacl}");
    }
}
