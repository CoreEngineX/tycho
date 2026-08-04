//! Scanning a remote for sync-client wreckage.
//!
//! **The damaging artifacts do not sit next to the bare repo, they land inside it.**
//! A duplicated packfile or a conflicted copy of a ref file is the case that silently
//! corrupts a remote, and git will not always complain - so the scan goes into each
//! `<profile>.git` rather than around it.

use std::path::{Path, PathBuf};

use crate::remote::recovery::FILE_NAME;

/// One file that should not be there, and what it is.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Artifact {
    pub path: PathBuf,
    pub kind: Kind,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// In `objects/pack/` but not named `pack-<hex>.(pack|idx|rev|bitmap|mtimes)`.
    /// This is the one that corrupts silently.
    StrayPack,
    /// Under `refs/` with a name that is not a legal ref component.
    StrayRef,
    /// The shapes sync clients leave anywhere: `... (1).ext`, `...conflicted copy...`.
    Conflicted,
    /// `._name`, which macOS writes beside every file on a volume that cannot hold an
    /// extended attribute - which is every exFAT and FAT32 drive, and `remotes.md`
    /// expects an external backup to be one.
    ///
    /// Not cosmetic. Git globs `objects/pack/*.idx`, so `._pack-<hex>.idx` is opened
    /// as a pack index and answered with `non-monotonic index`; every `._` under
    /// `refs/` is a `badRefName`. Measured on a real drive: one push produced 49 of
    /// them and `git fsck` reported errors on both. Given its own kind because the
    /// remedy is specific and nothing else here shares it.
    AppleDouble,
}

impl std::fmt::Display for Kind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.pad(match self {
            Self::AppleDouble => "macOS sidecar; run dot_clean -m on the folder",
            Self::StrayPack => "not a packfile name",
            Self::StrayRef => "not a ref name",
            Self::Conflicted => "a sync conflict copy",
        })
    }
}

/// Tycho's own in-flight write, which `doctor` must not report as wreckage.
fn is_ours(name: &str) -> bool {
    name == FILE_NAME || name.starts_with(&format!("{FILE_NAME}.tmp."))
}

/// Whether a name in `objects/pack/` is one git wrote.
fn is_packfile(name: &str) -> bool {
    let Some((stem, extension)) = name.rsplit_once('.') else {
        return false;
    };
    if !matches!(extension, "pack" | "idx" | "rev" | "bitmap" | "mtimes") {
        return false;
    }
    stem.strip_prefix("pack-")
        .is_some_and(|hex| !hex.is_empty() && hex.chars().all(|c| c.is_ascii_hexdigit()))
}

/// Whether a name under `refs/` is a legal ref component.
///
/// Deliberately narrower than `git check-ref-format`: a name with a space or a
/// parenthesis in it is not something git wrote, whatever the rules technically
/// permit, and this scan exists to find what a sync client left behind.
fn is_ref_component(name: &str) -> bool {
    !name.is_empty()
        && !name.starts_with('.')
        && !name.ends_with(".lock")
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '+' | '%' | '@'))
}

fn looks_conflicted(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    lower.contains("conflict")
        || lower.contains(" copy")
        // `notes (1).md`, the shape Drive and OneDrive both produce.
        || name
            .rsplit_once(" (")
            .is_some_and(|(_, rest)| {
                rest.split_once(')')
                    .is_some_and(|(digits, _)| !digits.is_empty() && digits.chars().all(|c| c.is_ascii_digit()))
            })
}

/// Walks a remote folder, including inside each bare repository.
#[must_use]
pub fn scan(folder: &Path) -> Vec<Artifact> {
    let mut found = Vec::new();
    walk(folder, folder, &mut found);
    found.sort_by(|a, b| a.path.cmp(&b.path));
    found
}

fn walk(root: &Path, dir: &Path, found: &mut Vec<Artifact>) {
    let Ok(listing) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in listing.flatten() {
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().into_owned();
        if path.is_dir() {
            walk(root, &path, found);
            continue;
        }
        if is_ours(&name) {
            continue;
        }

        let relative = path.strip_prefix(root).unwrap_or(&path);
        let inside = |segment: &str| {
            relative
                .components()
                .any(|part| part.as_os_str() == segment)
        };

        // Before the others, because an AppleDouble in `objects/pack/` is also a
        // stray packfile name and the sidecar is the useful half of the answer.
        let kind = if name.starts_with("._") {
            Some(Kind::AppleDouble)
        } else if inside("pack") && inside("objects") && !is_packfile(&name) {
            Some(Kind::StrayPack)
        } else if inside("refs") && !is_ref_component(&name) {
            Some(Kind::StrayRef)
        } else if looks_conflicted(&name) {
            Some(Kind::Conflicted)
        } else {
            None
        };

        if let Some(kind) = kind {
            found.push(Artifact { path, kind });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Kind, is_packfile, is_ref_component, looks_conflicted, scan};
    use std::fs;

    #[test]
    fn a_real_packfile_name_is_not_an_artifact() {
        for name in [
            "pack-1a2b3c4d5e6f.pack",
            "pack-1a2b3c4d5e6f.idx",
            "pack-1a2b3c4d5e6f.rev",
            "pack-1a2b3c4d5e6f.bitmap",
            "pack-1a2b3c4d5e6f.mtimes",
        ] {
            assert!(is_packfile(name), "{name}");
        }
    }

    /// The case that silently corrupts a remote: a duplicated packfile that git will
    /// not always complain about.
    #[test]
    fn a_duplicated_packfile_is_an_artifact() {
        for name in [
            "pack-1a2b3c4d5e6f (1).pack",
            "pack-1a2b3c4d5e6f.pack.conflicted",
            "pack-notactuallyhex.pack",
            "something.pack",
        ] {
            assert!(!is_packfile(name), "{name}");
        }
    }

    #[test]
    fn a_conflicted_copy_is_recognised_in_every_shape_the_clients_produce() {
        assert!(looks_conflicted("main (1)"));
        assert!(looks_conflicted("notes (2).md"));
        assert!(looks_conflicted("HEAD (alex's conflicted copy)"));
        assert!(looks_conflicted("packed-refs copy"));
        assert!(!looks_conflicted("packed-refs"));
        assert!(!looks_conflicted("main"));
    }

    #[test]
    fn a_legal_ref_component_is_left_alone() {
        assert!(is_ref_component("main"));
        assert!(is_ref_component("v1.0"));
        assert!(is_ref_component("feature-x"));
        assert!(is_ref_component("CoreEngineX%2Forg"));
        assert!(!is_ref_component("main (1)"));
        assert!(!is_ref_component("main.lock"));
        assert!(!is_ref_component(".DS_Store"));
    }

    /// The scan goes **inside** each bare repository, because that is where the
    /// damaging artifacts land.
    #[test]
    fn the_scan_reaches_inside_the_bare_repository() {
        let dir = tempfile::tempdir().expect("temp dir");
        let repo = dir.path().join("demo.git");
        fs::create_dir_all(repo.join("objects/pack")).expect("mkdir");
        fs::create_dir_all(repo.join("refs/heads")).expect("mkdir");

        fs::write(repo.join("objects/pack/pack-abc123.pack"), "").expect("a real pack");
        fs::write(repo.join("objects/pack/pack-abc123 (1).pack"), "").expect("a duplicate");
        fs::write(repo.join("refs/heads/main"), "").expect("a real ref");
        fs::write(repo.join("refs/heads/main (1)"), "").expect("a conflicted ref");
        // What macOS writes beside every file on exFAT. Measured on a real drive:
        // one push produced 49 of them, and git read `._pack-<hex>.idx` as a pack
        // index and answered `non-monotonic index`.
        fs::write(repo.join("objects/pack/._pack-abc123.idx"), "").expect("an AppleDouble sidecar");
        fs::write(dir.path().join("RECOVERY.md"), "").expect("ours");

        let found = scan(dir.path());
        let names: Vec<String> = found
            .iter()
            .map(|item| {
                item.path
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .into_owned()
            })
            .collect();

        assert_eq!(found.len(), 3, "{names:?}");
        let kind_of = |name: &str| {
            found
                .iter()
                .find(|item| item.path.ends_with(name))
                .map(|item| item.kind)
        };
        assert_eq!(kind_of("pack-abc123 (1).pack"), Some(Kind::StrayPack));
        // Measured on a real exFAT drive: git reads `._pack-<hex>.idx` as a pack index
        // and reports `non-monotonic index`, so this is wreckage rather than litter.
        assert_eq!(kind_of("._pack-abc123.idx"), Some(Kind::AppleDouble));
        assert_eq!(kind_of("main (1)"), Some(Kind::StrayRef));
    }

    /// Tycho's own in-flight `RECOVERY.md` write is expected, not wreckage.
    #[test]
    fn tychos_own_temp_file_is_not_reported() {
        let dir = tempfile::tempdir().expect("temp dir");
        fs::write(dir.path().join("RECOVERY.md"), "").expect("write");
        fs::write(dir.path().join("RECOVERY.md.tmp.4812"), "").expect("write");
        assert!(scan(dir.path()).is_empty());
    }
}
