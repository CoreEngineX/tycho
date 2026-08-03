//! The `RECOVERY.md` written beside the repositories in a remote folder.
//!
//! It describes the **folder**, not the profile that wrote it: the writer scans for
//! `*.git` directories and documents every one it finds, so a folder holding two
//! profiles gets one file covering both.
//!
//! Two details make last-writer-wins safe rather than merely likely. The content
//! carries **no timestamp**, so it is a pure function of the folder's contents and
//! two writers converge by construction. And the scan happens immediately before the
//! write, so a sibling repository created earlier in the same run is seen.

use crate::sys::fs::write_atomic;
use crate::sys::process::{Git, Timeout};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

pub const FILE_NAME: &str = "RECOVERY.md";

/// One recoverable profile in the folder.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Source {
    pub repo: String,
    /// The captured repository keys, read from `.tycho/repos/<key>/REPO.txt` in the
    /// latest commit.
    ///
    /// Derived from tree paths, never from ref names: `refs/tycho/<key>/...` also
    /// holds `remotes/...` and `stashes/...`, so stripping `/heads/` and `/tags/`
    /// emits several wrong keys for every real one.
    pub keys: Vec<String>,
}

/// Reads what is in the folder right now.
#[must_use]
pub fn scan(folder: &Path) -> Vec<Source> {
    let mut repos: Vec<PathBuf> = Vec::new();
    let Ok(listing) = std::fs::read_dir(folder) else {
        return Vec::new();
    };
    for entry in listing.flatten() {
        let path = entry.path();
        if path.is_dir() && path.extension().is_some_and(|ext| ext == "git") {
            repos.push(path);
        }
    }
    repos.sort();

    repos
        .iter()
        .filter_map(|repo| {
            let name = repo.file_name()?.to_string_lossy().into_owned();
            Some(Source {
                repo: name,
                keys: keys_in(repo),
            })
        })
        .collect()
}

fn keys_in(repo: &Path) -> Vec<String> {
    let Ok(out) = Git::at(repo).run(
        &["ls-tree", "-r", "--name-only", "-z", "HEAD"],
        Timeout::WORK,
    ) else {
        return Vec::new();
    };
    if !out.status.success() {
        return Vec::new();
    }
    let mut keys: Vec<String> = String::from_utf8_lossy(&out.stdout)
        .split('\0')
        .filter_map(|path| {
            path.strip_prefix(".tycho/repos/")?
                .strip_suffix("/REPO.txt")
                .map(str::to_owned)
        })
        .collect();
    keys.sort();
    keys.dedup();
    keys
}

/// Writes the file, scanning immediately beforehand.
///
/// # Errors
///
/// If the file cannot be written.
pub fn write(folder: &Path) -> std::io::Result<()> {
    let sources = scan(folder);
    write_atomic(&folder.join(FILE_NAME), render(folder, &sources).as_bytes())
}

/// The file's whole content, as a pure function of what the scan found.
#[must_use]
#[allow(clippy::too_many_lines)]
pub fn render(folder: &Path, sources: &[Source]) -> String {
    let here = folder.display();
    let mut out = String::new();

    out.push_str(
        "# Recovering from this folder\n\
         \n\
         Every `*.git` directory here is a **complete, self-sufficient backup**: a bare\n\
         git repository holding the entire object database and every ref. There is no\n\
         separate index, manifest or catalogue anywhere else, and Tycho is not needed to\n\
         read it - every command below is plain git.\n\n",
    );

    if sources.is_empty() {
        out.push_str("This folder holds no repositories yet.\n");
        return out;
    }

    out.push_str("## What is here\n\n```text\n");
    let _ = writeln!(out, "{here}/");
    for source in sources {
        let _ = writeln!(out, "  {}/", source.repo);
        for key in &source.keys {
            let _ = writeln!(out, "      {key}");
        }
    }
    out.push_str("```\n\nThe indented names are the captured repositories inside each backup.\n\n");

    let first = &sources[0];
    let profile = first.repo.strip_suffix(".git").unwrap_or(&first.repo);
    let source_path = format!("{here}/{}", first.repo);

    let _ = write!(
        out,
        "## 1. Get a complete copy\n\
         \n\
         If this folder is a cloud folder, **force full materialisation first**. On\n\
         macOS, files under `~/Library/CloudStorage` are dataless placeholders until\n\
         read, so a copy taken mid-download produces a store with a missing packfile\n\
         and no obvious symptom:\n\
         \n\
         ```text\n\
         find \"{here}\" -type f -exec cat {{}} + > /dev/null\n\
         ```\n\
         \n\
         Then mirror-clone. **Use `--mirror`, never a plain `git clone`**: a plain clone\n\
         fetches `refs/heads/*` and `refs/tags/*` only, and every captured repository\n\
         lives under `refs/tycho/*`, so it silently leaves all of that behind and hands\n\
         you a repository that looks fine.\n\
         \n\
         ```text\n\
         git clone --mirror \"{source_path}\" ~/{profile}.git\n\
         git -C ~/{profile}.git fsck\n\
         ```\n\
         \n\
         `git log` succeeding is **not** sufficient - it exits 0 on a store whose\n\
         objects are missing. `fsck` is what catches that. If it reports anything, the\n\
         source was not fully materialised: re-copy, or use another folder.\n\n"
    );

    let _ = write!(
        out,
        "## 2. Neutralise attributes before extracting\n\
         \n\
         ```text\n\
         printf '* -text -diff -filter -ident -export-subst -export-ignore\\n' \\\n\
         \x20 > ~/{profile}.git/info/attributes\n\
         ```\n\
         \n\
         `info/attributes` is not in the object database, so a mirror clone does not\n\
         carry it. Without this, a `.gitattributes` file that was itself backed up can\n\
         make the extraction below silently drop files marked `export-ignore` and\n\
         rewrite line endings - at exit 0, while `git ls-tree` still lists the file that\n\
         was never written.\n\n"
    );

    let _ = write!(
        out,
        "## 3. Recover the plain files and overlays\n\
         \n\
         ```text\n\
         mkdir ~/recovered\n\
         git -C ~/{profile}.git archive HEAD > ~/store.tar && tar -xf ~/store.tar -C ~/recovered\n\
         ```\n\
         \n\
         **Do not pipe `git archive` straight into `tar`.** In a pipeline the shell\n\
         reports only the last command's status, so a store with a missing object prints\n\
         an error, extracts zero files, and still exits 0.\n\
         \n\
         Use an older commit in place of `HEAD` for an earlier backup; `git -C\n\
         ~/{profile}.git log --oneline` lists every run.\n\n"
    );

    out.push_str("## 4. Rebuild a captured repository with its history\n\n");
    let example = first
        .keys
        .first()
        .cloned()
        .unwrap_or_else(|| "<key>".to_owned());
    let leaf = example.rsplit('/').next().unwrap_or(&example).to_owned();
    let _ = write!(
        out,
        "```text\n\
         git init ~/recovered-repos/{leaf}\n\
         git -C ~/recovered-repos/{leaf} symbolic-ref HEAD refs/heads/__tycho_restore\n\
         git -C ~/recovered-repos/{leaf} fetch ~/{profile}.git \\\n\
         \x20 \"+refs/tycho/{example}/heads/*:refs/heads/*\" \\\n\
         \x20 \"+refs/tycho/{example}/tags/*:refs/tags/*\" \\\n\
         \x20 \"+refs/tycho/{example}/remotes/*:refs/remotes/*\" \\\n\
         \x20 \"+refs/tycho/{example}/stash:refs/stash\" \\\n\
         \x20 \"+refs/tycho/{example}/stashes/*:refs/tycho-stash/*\"\n\
         git -C ~/recovered-repos/{leaf} checkout main\n\
         ```\n\
         \n\
         The `symbolic-ref` line is not optional and is the step people trip on. `git\n\
         init` leaves HEAD on an unborn `refs/heads/main`, and git refuses to fetch into\n\
         a branch that is checked out. Parking HEAD on a name that will never exist lets\n\
         the fetch write every branch as a real local branch.\n\
         \n\
         `git stash list` will be empty afterwards and **the stash is not lost**: that\n\
         command reads the reflog of `refs/stash`, and a fetch does not carry reflogs.\n\
         Apply them by name - `git stash apply refs/stash`, and\n\
         `git show refs/tycho-stash/1` for the rest.\n\n"
    );

    let _ = write!(
        out,
        "## 5. Put the overlay back on top\n\
         \n\
         The checkout gives you committed history. It does not give you what was never\n\
         committed - uncommitted edits, untracked files, and the gitignored files git\n\
         alone could never bring back:\n\
         \n\
         ```text\n\
         cp -R ~/recovered/.tycho/repos/{example}/overlay/. ~/recovered-repos/{leaf}/\n\
         ```\n\
         \n\
         Inspect the result. Where the checkout holds a **symlink** and the overlay a\n\
         regular file of the same name, `cp` writes through the link to its target,\n\
         creating a file that never existed on the source machine; and where one side is\n\
         a file and the other a directory, `cp` fails partway having already copied part\n\
         of the tree. `tycho restore` refuses both.\n\
         \n\
         `.tycho/repos/<key>/REPO.txt` records which branch or commit was checked out,\n\
         and how many stashes there were.\n\
         \n\
         ## What is not restored\n\
         \n\
         File contents only. Permissions beyond the execute bit, ownership, timestamps\n\
         and extended attributes are lost, so anything secret-bearing comes back\n\
         world-readable and should be re-secured immediately.\n"
    );

    out
}

#[cfg(test)]
mod tests {
    use super::{Source, render, scan, write};
    use std::path::Path;

    fn sources() -> Vec<Source> {
        vec![
            Source {
                repo: "coreenginex.git".to_owned(),
                keys: vec!["CoreEngineX/org/handbook".to_owned()],
            },
            Source {
                repo: "personal.git".to_owned(),
                keys: Vec::new(),
            },
        ]
    }

    /// The whole reason concurrent writers converge. A date in the content would make
    /// two writers disagree for no reason at all.
    #[test]
    fn the_content_carries_no_timestamp() {
        let text = render(Path::new("/Volumes/T7/tycho"), &sources());
        let first = render(Path::new("/Volumes/T7/tycho"), &sources());
        assert_eq!(text, first);

        for year in ["2025", "2026", "2027"] {
            assert!(!text.contains(year), "the content must not date itself");
        }
    }

    #[test]
    fn it_describes_every_repository_in_the_folder() {
        let text = render(Path::new("/Volumes/T7/tycho"), &sources());
        assert!(text.contains("coreenginex.git"));
        assert!(text.contains("personal.git"));
        assert!(text.contains("CoreEngineX/org/handbook"));
    }

    /// The instructions people get wrong, spelled out in the file itself.
    #[test]
    fn it_carries_the_four_traps() {
        let text = render(Path::new("/Volumes/T7/tycho"), &sources());
        assert!(
            text.contains("--mirror"),
            "a plain clone loses refs/tycho/*"
        );
        assert!(text.contains("symbolic-ref HEAD refs/heads/__tycho_restore"));
        assert!(text.contains("info/attributes"));
        assert!(
            text.contains("> ~/store.tar &&"),
            "piping archive into tar hides a failure"
        );
    }

    #[test]
    fn an_empty_folder_says_so_rather_than_printing_commands() {
        let text = render(Path::new("/tmp/empty"), &[]);
        assert!(text.contains("no repositories yet"));
        assert!(!text.contains("git clone"));
    }

    #[test]
    fn the_scan_finds_bare_repositories_and_ignores_everything_else() {
        let dir = tempfile::tempdir().expect("temp dir");
        for name in ["b.git", "a.git"] {
            std::fs::create_dir(dir.path().join(name)).expect("repo dir");
        }
        std::fs::create_dir(dir.path().join("notes")).expect("plain dir");
        std::fs::write(dir.path().join("RECOVERY.md"), "old").expect("file");

        let found = scan(dir.path());
        let names: Vec<&str> = found.iter().map(|s| s.repo.as_str()).collect();
        assert_eq!(names, ["a.git", "b.git"], "sorted, so the output is stable");
    }

    #[test]
    fn two_writers_over_the_same_folder_produce_identical_bytes() {
        let dir = tempfile::tempdir().expect("temp dir");
        std::fs::create_dir(dir.path().join("one.git")).expect("repo dir");

        write(dir.path()).expect("first write");
        let first = std::fs::read(dir.path().join("RECOVERY.md")).expect("read");
        write(dir.path()).expect("second write");
        let second = std::fs::read(dir.path().join("RECOVERY.md")).expect("read");

        assert_eq!(first, second);
        let leftovers: Vec<String> = std::fs::read_dir(dir.path())
            .expect("listing")
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|name| name.contains(".tmp"))
            .collect();
        assert!(
            leftovers.is_empty(),
            "temp files left behind: {leftovers:?}"
        );
    }
}
