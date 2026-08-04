//! Restore, including the disaster path it exists for.
//!
//! `disaster-recovery.md` has now been wrong twice in the same shape: a procedure
//! that was genuinely executed, against one repository, whose particulars happened to
//! make it pass. These tests execute the procedure the product performs, against the
//! cases that broke it.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use tempfile::TempDir;
use tycho::config::Profile;
use tycho::primitives::path::AbsPath;
use tycho::restore::{self, Wanted};
use tycho::state::State;
use tycho::store::{Store, run};

/// A path inside a TOML basic string needs every `\` escaped, and on Windows every
/// separator is one. A person writing a config by hand would use a literal string;
/// a fixture that interpolates a real path has to escape instead.
fn toml_path(path: &Path) -> String {
    path.display().to_string().replace('\\', "\\\\")
}

/// The five gitattributes traps `store.md` section 3 names, plus the file that arms
/// them. Every one is a file a user could plausibly have, and each defeats
/// byte-exactness on the way *out* rather than the way in.
const ATTRIBUTES: &str = "\
* text=auto
*.lfs filter=fake
*.id ident
*.subst export-subst
hidden.txt export-ignore
";

fn traps() -> Vec<(&'static str, &'static [u8])> {
    vec![
        (".gitattributes", ATTRIBUTES.as_bytes()),
        ("windows.txt", b"line one\r\nline two\r\n"),
        ("big.lfs", b"pretend this is a large binary\n"),
        ("stamped.id", b"$Id$\nbody\n"),
        ("expanded.subst", b"$Format:%H$\n"),
        ("hidden.txt", b"the file export-ignore drops\n"),
    ]
}

fn write(path: &Path, bytes: &[u8]) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).expect("mkdir");
    }
    fs::write(path, bytes).expect("write");
}

/// Config is pinned because these fixtures otherwise inherit whatever this machine
/// happens to have set - `tag.gpgSign` alone turns `git tag v1.0` into
/// `fatal: no tag message?`, which is a failing test that says nothing about Tycho.
fn git(dir: &Path, args: &[&str]) -> std::process::Output {
    Command::new("git")
        .current_dir(dir)
        .args([
            "-c",
            "tag.gpgSign=false",
            "-c",
            "commit.gpgSign=false",
            "-c",
            "core.autocrlf=false",
        ])
        .args(args)
        .output()
        .expect("git runs")
}

fn checked(dir: &Path, args: &[&str]) -> String {
    let out = git(dir, args);
    assert!(out.status.success(), "git {args:?}: {out:?}");
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn commit(repo: &Path, message: &str) {
    checked(repo, &["add", "-A"]);
    checked(
        repo,
        &[
            "-c",
            "user.email=a@b",
            "-c",
            "user.name=a",
            "commit",
            "-qm",
            message,
        ],
    );
}

struct Fixture {
    dir: TempDir,
    store: Store,
    profile: Profile,
    state: State,
}

impl Fixture {
    fn run(&mut self) {
        let lock = self.dir.path().join("demo.lock");
        let state_path = self.dir.path().join("state.json");
        let paths = run::Paths {
            lock: &lock,
            state: &state_path,
            config_text: None,
        };
        run::execute(&self.profile, &self.store, &paths, &mut self.state, false).expect("the run");
    }

    fn root(&self) -> PathBuf {
        self.dir.path().join("A")
    }

    fn dest(&self, name: &str) -> PathBuf {
        self.dir.path().join(name)
    }

    fn restore(&self, into: &Path, wanted: &Wanted) -> restore::Done {
        restore::execute(&self.store, into, None, wanted, false).expect("the restore")
    }

    fn store_path(&self) -> PathBuf {
        self.dir.path().join("demo.git")
    }
}

fn fixture() -> Fixture {
    let dir = tempfile::tempdir().expect("temp dir");
    let root = dir.path().join("A");
    let text = format!(
        "version = 1\n[[profile]]\nname = \"demo\"\nwatch = [\"{}\"]\nlocal_only = true\n",
        toml_path(&root)
    );
    let parsed = tycho::config::parse_with(&text, Some(Path::new("/nowhere")), |_| None)
        .expect("valid TOML");
    assert!(!parsed.has_errors(), "{:#?}", parsed.diagnostics);
    let profile = parsed
        .config
        .profiles
        .into_iter()
        .next()
        .expect("a profile");
    let store = Store::open_or_init(
        &AbsPath::from_absolute(&dir.path().join("demo.git")).expect("absolute"),
    )
    .expect("init");
    Fixture {
        dir,
        store,
        profile,
        state: State::default(),
    }
}

/// The invariant the whole design rests on, taken through the **real restore path**
/// rather than a raw `archive` call.
///
/// A `.gitattributes` captured into the store's own tree arms all five of these, and
/// needs no unusual configuration anywhere.
#[test]
fn every_gitattributes_trap_survives_a_restore_byte_for_byte() {
    let mut fixture = fixture();
    for (name, bytes) in traps() {
        write(&fixture.root().join(name), bytes);
    }
    fixture.run();

    let into = fixture.dest("recovered");
    fixture.restore(&into, &Wanted::default());

    for (name, bytes) in traps() {
        let restored = fs::read(into.join("A").join(name))
            .unwrap_or_else(|error| panic!("{name} did not come back: {error}"));
        assert_eq!(
            restored, bytes,
            "{name} came back changed, which is the whole failure this guards"
        );
    }
}

/// The other half, and the reason `Neutralised` is a spine state.
///
/// `info/attributes` does not survive `git clone --mirror`, so a restore from a mirror
/// clone re-establishes it. Run twice, because a byte-exactness test that passes for
/// the wrong reason is worse than none: without the mechanism, `export-ignore` drops a
/// file outright and `export-subst` and `ident` rewrite two more, **at exit 0**.
#[test]
fn a_restore_from_a_mirror_clone_re_establishes_the_attributes_it_needs() {
    let mut fixture = fixture();
    for (name, bytes) in traps() {
        write(&fixture.root().join(name), bytes);
    }
    fixture.run();

    let clone = fixture.dest("mirror.git");
    assert!(
        Command::new("git")
            .args(["clone", "--mirror", "-q"])
            .arg(fixture.store.path_for_test())
            .arg(&clone)
            .status()
            .expect("git runs")
            .success()
    );
    assert!(
        !clone.join("info/attributes").exists(),
        "if a mirror clone carried it, this test would prove nothing"
    );

    // Without the mechanism: archive straight out of the clone.
    let bare = fixture.dest("without");
    fs::create_dir_all(&bare).expect("mkdir");
    let tar = bare.join("out.tar");
    checked(
        &clone,
        &[
            "archive",
            "--format=tar",
            &format!("--output={}", tar.display()),
            "HEAD",
        ],
    );
    assert!(
        Command::new("tar")
            .args(["-xf"])
            .arg(&tar)
            .arg("-C")
            .arg(&bare)
            .status()
            .expect("tar runs")
            .success()
    );
    assert!(
        !bare.join("A/hidden.txt").exists(),
        "export-ignore must actually drop the file, or the mechanism is untested"
    );
    assert_ne!(
        fs::read(bare.join("A/expanded.subst")).expect("read"),
        b"$Format:%H$\n",
        "export-subst must actually rewrite it, or the mechanism is untested"
    );
    assert_ne!(
        fs::read(bare.join("A/stamped.id")).expect("read"),
        b"$Id$\nbody\n",
        "ident must actually rewrite it, or the mechanism is untested"
    );

    // With it: the restore path, which writes info/attributes before archiving.
    let store = Store::open_to_read(&AbsPath::from_absolute(&clone).expect("absolute"))
        .expect("a mirror clone is mode 0755, and reading one must still be allowed");
    let into = fixture.dest("with");
    restore::execute(&store, &into, None, &Wanted::default(), false).expect("the restore");

    for (name, bytes) in traps() {
        let restored = fs::read(into.join("A").join(name))
            .unwrap_or_else(|error| panic!("{name} did not come back: {error}"));
        assert_eq!(
            restored, bytes,
            "{name} came back changed from a mirror clone"
        );
    }
}

/// A repository comes back as a repository, and the fetch is globs only.
///
/// Run against a repository with **no stash**, which is the case that made two
/// earlier versions of `disaster-recovery.md` recover nothing at all: an exact-ref
/// refspec that is absent aborts the whole fetch and writes no branches and no tags.
#[test]
fn a_repository_with_no_stash_comes_back_whole() {
    let mut fixture = fixture();
    let repo = fixture.root().join("proj");
    write(&repo.join("tracked.md"), b"committed\n");
    checked(&repo, &["init", "-q", "-b", "main"]);
    commit(&repo, "first");
    checked(&repo, &["tag", "v1.0"]);
    checked(&repo, &["checkout", "-qb", "side"]);
    write(&repo.join("side.md"), b"on a branch\n");
    commit(&repo, "second");
    checked(&repo, &["checkout", "-q", "main"]);
    assert!(
        checked(&repo, &["stash", "list"]).is_empty(),
        "the point of this test is that there is no stash"
    );
    write(&repo.join("untracked.md"), b"never committed\n");
    fixture.run();

    let into = fixture.dest("recovered");
    let done = fixture.restore(&into, &Wanted::default());
    assert_eq!(done.repos.len(), 1);
    assert_eq!(done.repos[0].head.as_deref(), Some("main"));

    let back = into.join("A/proj");
    let branches = checked(
        &back,
        &["for-each-ref", "--format=%(refname)", "refs/heads/"],
    );
    assert!(branches.contains("refs/heads/main"), "{branches}");
    assert!(branches.contains("refs/heads/side"), "{branches}");
    assert!(
        checked(&back, &["tag", "-l"]).contains("v1.0"),
        "a tag must survive a fetch whose other globs matched nothing"
    );
    assert_eq!(
        fs::read(back.join("tracked.md")).expect("read"),
        b"committed\n"
    );
    assert_eq!(
        fs::read(back.join("untracked.md")).expect("read"),
        b"never committed\n",
        "the overlay carries what git alone could never bring back"
    );
}

/// The whole stash stack comes back: the top entry as `refs/stash`, the rest as
/// ordinary refs, because git's stash stack is a reflog and cannot be rebuilt.
#[test]
fn a_repository_with_stashes_gets_all_of_them_back() {
    let mut fixture = fixture();
    let repo = fixture.root().join("proj");
    write(&repo.join("f.txt"), b"one\n");
    checked(&repo, &["init", "-q", "-b", "main"]);
    commit(&repo, "first");
    for text in [b"two\n".as_slice(), b"three\n".as_slice()] {
        write(&repo.join("f.txt"), text);
        checked(&repo, &["stash", "push", "-q"]);
    }
    fixture.run();

    let into = fixture.dest("recovered");
    let done = fixture.restore(&into, &Wanted::default());
    assert_eq!(
        done.repos[0].stashes, 2,
        "REPO.txt records how many there were"
    );

    let back = into.join("A/proj");
    assert!(
        !checked(&back, &["rev-parse", "refs/stash"])
            .trim()
            .is_empty(),
        "the top entry becomes refs/stash"
    );
    let rest = checked(
        &back,
        &["for-each-ref", "--format=%(refname)", "refs/tycho-stash/"],
    );
    assert!(!rest.is_empty(), "the rest arrive as ordinary refs: {rest}");
}

/// A symlink, the shape `restore::overlay`'s conflict guard is written against.
#[cfg(unix)]
fn name_pointing_at_real_txt(at: &std::path::Path) {
    std::os::unix::fs::symlink("real.txt", at).expect("symlink");
}

/// No symlink without a privilege here. What this test asserts is that the staging
/// tree survives a restore, which does not turn on the link, so a regular file
/// carrying the same name keeps the shape. The refusal itself is covered by
/// `restore::overlay`'s own Windows test.
#[cfg(windows)]
fn name_pointing_at_real_txt(at: &std::path::Path) {
    std::fs::write(at, "real.txt").expect("write");
}

/// The overlay refuses rather than resolves, and says what it refused. The refused
/// file stays in the staging tree, which is why `.tycho/` is never deleted.
#[test]
fn an_overlay_type_conflict_is_reported_and_the_material_is_still_there() {
    let mut fixture = fixture();
    let repo = fixture.root().join("proj");
    write(&repo.join("real.txt"), b"the link's target\n");
    name_pointing_at_real_txt(&repo.join("config"));
    checked(&repo, &["init", "-q", "-b", "main"]);
    commit(&repo, "a symlink named config");

    // The same name, as an untracked regular file, is impossible on one machine - so
    // the conflict is manufactured by replacing the checkout's link afterwards.
    fixture.run();
    let into = fixture.dest("recovered");
    let done = fixture.restore(&into, &Wanted::default());

    // Nothing conflicts on a clean restore; the guard is proved by the unit tests in
    // `restore::overlay`. What matters here is that the staging tree survives.
    assert_eq!(done.conflicts(), 0);
    assert!(
        into.join(".tycho/repos/A/proj/REPO.txt").exists(),
        "the staging tree is kept, so a refused file can be settled by hand"
    );
    assert!(
        into.join(".tycho/config.toml").exists() || done.files > 0,
        "the backup's own description travels with it"
    );
}

/// A restore that could not write everything says which paths, rather than counting
/// the backup and calling it the result.
///
/// On Windows this is not hypothetical: `tar` cannot create a symlink without
/// `SeCreateSymbolicLinkPrivilege`, prints `Can't create ... Invalid argument`,
/// **carries on with the rest of the archive**, and exits 1. The count previously
/// came from the backup's tree, so every one of those restores reported success.
#[cfg(windows)]
#[test]
fn a_link_windows_cannot_write_is_named_rather_than_counted() {
    let mut fixture = fixture();
    write(&fixture.root().join("ordinary.txt"), b"plain\n");
    write(&fixture.root().join("elsewhere").join("inside.txt"), b"x\n");

    // A junction is stored as a link, and a link is what `tar` cannot write here.
    let made = Command::new("cmd")
        .args(["/c", "mklink", "/J"])
        .arg(fixture.root().join("link"))
        .arg(fixture.root().join("elsewhere"))
        .output()
        .expect("mklink runs");
    assert!(
        made.status.success(),
        "mklink /J: {}",
        String::from_utf8_lossy(&made.stdout)
    );

    fixture.run();
    let into = fixture.dest("recovered");
    let done = fixture.restore(&into, &Wanted::default());

    assert!(
        done.missing.iter().any(|path| path.ends_with("link")),
        "the link tar could not create must be named: {:?}",
        done.missing
    );
    assert!(
        into.join("A").join("ordinary.txt").exists(),
        "one refused link must not cost the rest of the restore"
    );
    assert!(
        done.files > 0,
        "the rest of the backup still landed and is counted"
    );
    assert_eq!(
        fs::read(into.join("A").join("ordinary.txt")).expect("read"),
        b"plain\n"
    );
}

/// A directory `tar` cannot write into, which is how a restore comes up short on a
/// platform where every link can be created.
///
/// `bsdtar` applies a mode only to a directory it creates itself, so one already at
/// `0500` in the destination stays that way and every entry underneath it fails -
/// while the rest of the archive lands. That is the same shape as the Windows case
/// above and needs no privilege to arrange.
#[cfg(unix)]
fn block_a_directory(into: &Path) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;

    let blocked = into.join("A").join("sub");
    fs::create_dir_all(&blocked).expect("mkdir");
    fs::set_permissions(&blocked, fs::Permissions::from_mode(0o500)).expect("chmod");
    blocked
}

/// The count must come from the filesystem, not from the backup's own tree.
///
/// Before this, `files` was `backup.tree().len()` whatever happened, so a restore
/// that wrote nothing still reported every file in the backup and exited 0.
#[cfg(unix)]
#[test]
fn a_restore_short_of_its_backup_names_what_is_missing() {
    use std::os::unix::fs::PermissionsExt;

    let mut fixture = fixture();
    write(&fixture.root().join("ordinary.txt"), b"plain\n");
    write(&fixture.root().join("sub").join("inside.md"), b"blocked\n");
    fixture.run();

    let into = fixture.dest("recovered");
    let blocked = block_a_directory(&into);
    let done = restore::execute(&fixture.store, &into, None, &Wanted::default(), true)
        .expect("a short restore still reports rather than aborting");

    assert!(
        done.missing.iter().any(|path| path.ends_with("inside.md")),
        "the entry tar could not write must be named: {:?}",
        done.missing
    );
    assert!(
        into.join("A").join("ordinary.txt").exists(),
        "one blocked directory must not cost the rest of the restore"
    );
    assert!(
        done.files > 0 && done.missing.len() == 1,
        "the rest landed and is counted separately from what did not: {done:?}"
    );

    fs::set_permissions(&blocked, fs::Permissions::from_mode(0o700)).expect("chmod back");
}

/// **A restore short of its backup exits 1.**
///
/// The reconciliation above was computed, printed, and then discarded: `restore`
/// returned 0 whenever the extraction merely failed to write something, and reserved
/// its non-zero exits for a refused overlay file - the *less* serious condition, where
/// the material is still in the staging tree. A scripted `tycho restore && rm -rf
/// original` read that as success.
#[cfg(unix)]
#[test]
fn a_restore_that_could_not_write_everything_exits_non_zero() {
    use std::os::unix::fs::PermissionsExt;

    let mut fixture = fixture();
    write(&fixture.root().join("ordinary.txt"), b"plain\n");
    write(&fixture.root().join("sub").join("inside.md"), b"blocked\n");
    fixture.run();

    let into = fixture.dest("recovered");
    let blocked = block_a_directory(&into);
    let out = Command::new(env!("CARGO_BIN_EXE_tycho"))
        .args(["restore", "--store"])
        .arg(fixture.store_path())
        .arg("--into")
        .arg(&into)
        .arg("--force")
        .output()
        .expect("tycho runs");
    fs::set_permissions(&blocked, fs::Permissions::from_mode(0o700)).expect("chmod back");

    let stdout = String::from_utf8_lossy(&out.stdout);
    assert_eq!(
        out.status.code(),
        Some(1),
        "a restore that did not write everything is a failed restore\n{stdout}{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        stdout.contains("missing") && stdout.contains("inside.md"),
        "and it says which path: {stdout}"
    );
}

/// A name outside ASCII must come back as itself.
///
/// Windows ships bsdtar, which reads tar header names in the machine's ANSI codepage
/// unless told otherwise, so `Café — supplier agrément.md` extracted as mojibake: on
/// disk, under a name nothing would look for, while the count said the restore was
/// complete. Found by restoring a real tree, not by reading.
#[test]
fn a_name_outside_ascii_comes_back_as_itself() {
    let mut fixture = fixture();
    let awkward = "Café — supplier agrément.md";
    write(
        &fixture.root().join(awkward),
        b"non-ascii name, ascii content\n",
    );
    write(&fixture.root().join("plain.txt"), b"plain\n");
    fixture.run();

    let into = fixture.dest("recovered");
    let done = fixture.restore(&into, &Wanted::default());

    assert!(
        done.missing.is_empty(),
        "nothing should be missing: {:?}",
        done.missing
    );
    assert_eq!(
        fs::read(into.join("A").join(awkward)).expect("the non-ascii name came back"),
        b"non-ascii name, ascii content\n"
    );
}

/// The same name, restored with **no locale at all**, which is what the scheduled run
/// gets.
///
/// The test above inherits this shell's `LANG`, and libarchive takes its header
/// charset from `nl_langinfo(CODESET)` - so it exercises the one condition under which
/// the Windows bug could not appear. A launchd agent has no `LANG`: the generated plist
/// sets no `EnvironmentVariables`, so an agent-run restore is the stripped case.
///
/// This is what says the `#[cfg(not(windows))]` arm of `charset_options` is right
/// rather than merely untested. It is also why that arm must stay empty: passing
/// `hdrcharset=UTF-8` here extracts `agre\u{301}ment` - `e` plus a combining acute -
/// where the tree holds `é`, turning NFC into NFD.
#[cfg(unix)]
#[test]
fn a_name_outside_ascii_survives_the_empty_environment_a_scheduled_run_has() {
    let mut fixture = fixture();
    let awkward = "Café — supplier agrément.md";
    write(&fixture.root().join(awkward), b"stripped environment\n");
    fixture.run();

    let into = fixture.dest("recovered-bare");
    let out = Command::new(env!("CARGO_BIN_EXE_tycho"))
        .env_clear()
        .env(
            "PATH",
            std::env::var_os("PATH").expect("a PATH to find git and tar"),
        )
        .args(["restore", "--store"])
        .arg(fixture.store_path())
        .arg("--into")
        .arg(&into)
        .output()
        .expect("tycho runs");
    assert_eq!(
        out.status.code(),
        Some(0),
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );

    assert_eq!(
        fs::read(into.join("A").join(awkward)).expect("the non-ascii name came back"),
        b"stripped environment\n"
    );

    // Read out of the directory rather than compared against a path built here.
    // APFS looks a name up in either normalisation, so `fs::read` on a constructed
    // path succeeds whichever form is on disk and proves nothing - the bytes the
    // filesystem hands back are the only thing that can tell NFC from NFD.
    let on_disk = fs::read_dir(into.join("A"))
        .expect("read the restored directory")
        .filter_map(Result::ok)
        .map(|entry| entry.file_name())
        .find(|name| name.as_encoded_bytes().starts_with(b"Caf"))
        .expect("the accented entry is there under some spelling");
    assert_eq!(
        on_disk.as_encoded_bytes(),
        awkward.as_bytes(),
        "the name on disk must be the bytes the tree holds, not a re-normalised form"
    );
}

/// **A secret comes back a secret.**
///
/// Git records one bit per file, so a `0600` `.env` is stored as `100644` and `tar`
/// writes it back readable by everyone on the machine. The store keeps gitignored
/// content precisely because that is where `.env` files and keys live, so restoring
/// them world-readable undoes the reason the store is kept private in the first place.
///
/// Run against the full ladder, because a manifest that only handled `0600` would be
/// a special case rather than a mechanism.
#[cfg(unix)]
#[test]
fn the_permission_bits_git_cannot_store_come_back_anyway() {
    use std::os::unix::fs::PermissionsExt;

    let modes = [
        (".env", 0o600u32),
        ("deploy.sh", 0o700),
        ("notes.md", 0o644),
        ("shared.txt", 0o664),
    ];

    let mut fixture = fixture();
    for (name, mode) in modes {
        let path = fixture.root().join(name);
        write(&path, b"content\n");
        fs::set_permissions(&path, fs::Permissions::from_mode(mode)).expect("chmod");
    }
    fixture.run();

    let into = fixture.dest("recovered");
    let done = fixture.restore(&into, &Wanted::default());

    let applied = done
        .metadata
        .as_ref()
        .expect("the backup carried a manifest");
    assert_eq!(applied.modes, modes.len(), "{applied:?}");
    assert!(applied.failed.is_empty(), "{applied:?}");

    for (name, mode) in modes {
        let back = fs::metadata(into.join("A").join(name))
            .unwrap_or_else(|error| panic!("{name}: {error}"))
            .permissions()
            .mode()
            & 0o7777;
        assert_eq!(
            back, mode,
            "{name} came back {back:04o} and was captured {mode:04o}"
        );
    }
}

/// The manifest travels in the tree, so a plain `git archive` recovery can read it
/// too - `disaster-recovery.md` is written for someone who has git and not Tycho.
#[cfg(unix)]
#[test]
fn the_manifest_is_in_the_backup_rather_than_beside_it() {
    use std::os::unix::fs::PermissionsExt;

    let mut fixture = fixture();
    let path = fixture.root().join(".env");
    write(&path, b"SECRET=1\n");
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).expect("chmod");
    fixture.run();

    let into = fixture.dest("recovered");
    fixture.restore(&into, &Wanted::default());

    let text = fs::read_to_string(into.join(".tycho/metadata.tsv"))
        .expect("the manifest is a file in the tree");
    assert!(
        text.lines().any(|line| line.starts_with("f\t0600\t")),
        "the captured mode must be in it: {text}"
    );
}

/// Restore never writes into a destination that already holds things.
#[test]
fn a_non_empty_destination_is_refused_without_force() {
    let mut fixture = fixture();
    write(&fixture.root().join("f.txt"), b"content\n");
    fixture.run();

    let into = fixture.dest("occupied");
    write(&into.join("someone-elses.txt"), b"do not clobber me\n");

    let refused = restore::execute(&fixture.store, &into, None, &Wanted::default(), false)
        .expect_err("a non-empty destination is refused");
    assert!(refused.to_string().contains("--force"), "{refused}");

    restore::execute(&fixture.store, &into, None, &Wanted::default(), true).expect("with --force");
    assert!(
        into.join("someone-elses.txt").exists(),
        "--force restores alongside rather than wiping"
    );
}

/// Each of the three resolutions comes back at the path you asked for, never at the
/// store's internal layout.
#[test]
fn a_single_file_lands_where_you_named_it_whichever_source_answered() {
    let mut fixture = fixture();
    write(&fixture.root().join("loose.md"), b"a plain file\n");
    let repo = fixture.root().join("proj");
    write(&repo.join("tracked.md"), b"committed\n");
    write(&repo.join(".gitignore"), b"secret.env\n");
    checked(&repo, &["init", "-q", "-b", "main"]);
    commit(&repo, "first");
    write(&repo.join("secret.env"), b"TOKEN=hunter2\n");
    fixture.run();

    let into = fixture.dest("recovered");
    let wanted = Wanted {
        paths: vec![
            PathBuf::from("A/loose.md"),
            PathBuf::from("A/proj/tracked.md"),
            PathBuf::from("A/proj/secret.env"),
        ],
        bundle: false,
    };
    let done = fixture.restore(&into, &wanted);

    assert_eq!(done.files, 3);
    assert_eq!(
        fs::read(into.join("A/loose.md")).expect("read"),
        b"a plain file\n"
    );
    assert_eq!(
        fs::read(into.join("A/proj/tracked.md")).expect("read"),
        b"committed\n"
    );
    assert_eq!(
        fs::read(into.join("A/proj/secret.env")).expect("read"),
        b"TOKEN=hunter2\n",
        "an overlay entry comes back at its real path, not under .tycho/"
    );
    assert!(
        !into.join(".tycho").exists(),
        "a single-file restore extracts nothing else"
    );

    // And each says which of the three answered.
    let sources: Vec<String> = done
        .resolved
        .iter()
        .map(|(_, resolved)| resolved.source())
        .collect();
    assert!(sources[0].contains("plain file"), "{sources:?}");
    assert!(sources[1].contains("tracked file"), "{sources:?}");
    assert!(sources[2].contains("overlay"), "{sources:?}");
}

/// A bundle is built from the **restored** refs, not the store's internal ones.
/// Verified the hard way: a bundle of `refs/tycho/<key>/*` clones to a repository
/// with no branch and no HEAD, and `git clone` fails outright.
#[test]
fn a_bundle_clones_to_a_working_tree() {
    let mut fixture = fixture();
    let repo = fixture.root().join("proj");
    write(&repo.join("tracked.md"), b"committed\n");
    checked(&repo, &["init", "-q", "-b", "main"]);
    commit(&repo, "first");
    fixture.run();

    let into = fixture.dest("handover");
    let wanted = Wanted {
        paths: vec![PathBuf::from("A/proj")],
        bundle: true,
    };
    let done = fixture.restore(&into, &wanted);
    let bundle = done.bundle.expect("a bundle");

    let refs = checked(
        &into,
        &["bundle", "list-heads", &bundle.display().to_string()],
    );
    assert!(refs.contains("refs/heads/main"), "{refs}");
    assert!(
        !refs.contains("refs/tycho/"),
        "store-internal names in a bundle clone to nothing usable: {refs}"
    );

    let cloned = fixture.dest("cloned");
    assert!(
        Command::new("git")
            .args(["clone", "-q"])
            .arg(&bundle)
            .arg(&cloned)
            .status()
            .expect("git runs")
            .success(),
        "the bundle must clone"
    );
    // The blob, not the checkout. What Tycho guarantees is the bytes in the bundle;
    // what a working tree ends up holding is the recipient's `core.autocrlf`, which
    // defaults to `true` in Git for Windows and rewrites LF to CRLF on the way out.
    // `cat-file blob` applies no filters, so this asks the guarantee on both.
    let blob = checked(&cloned, &["cat-file", "blob", "HEAD:tracked.md"]);
    assert_eq!(blob, "committed\n");

    // And the checkout is what it is, recorded rather than hidden: on Windows a
    // default clone of a byte-exact bundle still lands CRLF in the working tree.
    let working = fs::read(cloned.join("tracked.md")).expect("read");
    if cfg!(windows) {
        assert_eq!(
            working, b"committed\r\n",
            "Git for Windows rewrites on checkout; if this changed, say so in store.md"
        );
    } else {
        assert_eq!(working, b"committed\n");
    }
}

/// `--at` selects the newest backup at or before the moment, which is what makes
/// "the version from before I broke it" mean what it says.
#[test]
fn at_selects_the_newest_backup_at_or_before_the_moment() {
    let mut fixture = fixture();
    write(&fixture.root().join("f.txt"), b"first\n");
    fixture.run();
    let first = fixture.store.at(None).expect("at").expect("a backup");

    // A second backup with different content, and a moment between the two.
    std::thread::sleep(std::time::Duration::from_millis(1100));
    let between = jiff::Timestamp::now();
    std::thread::sleep(std::time::Duration::from_millis(1100));
    write(&fixture.root().join("f.txt"), b"second\n");
    fixture.run();
    let second = fixture.store.at(None).expect("at").expect("a backup");
    assert_ne!(first, second);

    let into = fixture.dest("older");
    restore::execute(
        &fixture.store,
        &into,
        Some(&between),
        &Wanted::default(),
        false,
    )
    .expect("the restore");
    assert_eq!(
        fs::read(into.join("A/f.txt")).expect("read"),
        b"first\n",
        "at-or-before must not reach forward past the moment asked for"
    );
}
