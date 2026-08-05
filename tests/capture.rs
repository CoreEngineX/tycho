//! Repository capture: history, the overlay, and provenance.

use std::fs;
use std::path::Path;
use std::process::Command;
use tempfile::TempDir;
use tycho::config::Profile;
use tycho::primitives::path::AbsPath;
use tycho::state::State;
use tycho::store::{Store, run};

fn abs(path: &Path) -> AbsPath {
    AbsPath::from_absolute(path).expect("temp paths are absolute")
}

fn write(path: &Path, text: &str) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).expect("mkdir");
    }
    fs::write(path, text).expect("write");
}

fn git(repo: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .current_dir(repo)
        .args(args)
        .output()
        .expect("git runs");
    assert!(out.status.success(), "git {args:?}: {out:?}");
    String::from_utf8_lossy(&out.stdout).trim().to_owned()
}

/// A repository with one commit, identity configured so commits work anywhere.
fn init_repo(path: &Path) {
    fs::create_dir_all(path).expect("mkdir");
    git(path, &["init", "--quiet", "-b", "main", "."]);
    git(path, &["config", "user.name", "t"]);
    git(path, &["config", "user.email", "t@t"]);
}

fn commit(repo: &Path, message: &str) {
    git(repo, &["add", "-A"]);
    git(repo, &["commit", "--quiet", "-m", message]);
}

struct Fixture {
    dir: TempDir,
    store: Store,
    profile: Profile,
    state: State,
}

/// A path inside a TOML basic string needs every `\` escaped, and on Windows every
/// separator is one. A person writing a config by hand would use a literal string;
/// a fixture that interpolates a real path has to escape instead.
fn toml_path(path: &Path) -> String {
    path.display().to_string().replace('\\', "\\\\")
}

fn fixture(dir: TempDir) -> Fixture {
    let root = toml_path(dir.path());
    let text = format!(
        "version = 1\n[[profile]]\nname = \"demo\"\nwatch = [\"{root}/A\"]\nlocal_only = true\n"
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
    let store = Store::open_or_init(&abs(&dir.path().join("demo.git"))).expect("init");
    Fixture {
        dir,
        store,
        profile,
        state: State::default(),
    }
}

impl Fixture {
    fn run(&mut self) -> Result<run::Completed, run::RunError> {
        let lock = self.dir.path().join("demo.lock");
        let state_path = self.dir.path().join("state.json");
        let paths = run::Paths {
            lock: &lock,
            state: &state_path,
            config_text: None,
        };
        run::execute(
            &self.profile,
            &self.store,
            &paths,
            &mut self.state,
            false,
            &mut |_| {},
        )
    }

    fn tree(&self) -> Vec<String> {
        git(
            self.store.path_for_test(),
            &["ls-tree", "-r", "--name-only", "HEAD"],
        )
        .lines()
        .map(str::to_owned)
        .collect()
    }

    fn refs(&self) -> Vec<String> {
        git(
            self.store.path_for_test(),
            &["for-each-ref", "--format=%(refname)"],
        )
        .lines()
        .map(str::to_owned)
        .collect()
    }
}

/// The July 2026 incident, as a standing regression test. A sync deleted files under
/// a repository and git restored every one except the gitignored `CLAUDE.md` - the
/// only unprotected file was the one git could not resurrect. D9.
#[test]
fn a_gitignored_file_is_captured_and_gitignored_junk_is_not() {
    let dir = tempfile::tempdir().expect("temp dir");
    let repo = dir.path().join("A/docs");
    init_repo(&repo);
    write(&repo.join(".gitignore"), "node_modules/\nCLAUDE.md\n");
    write(&repo.join("README.md"), "tracked\n");
    commit(&repo, "one");

    write(&repo.join("CLAUDE.md"), "the file git could not restore\n");
    write(
        &repo.join("node_modules/pkg/index.js"),
        "tens of gigabytes\n",
    );

    let mut fixture = fixture(dir);
    fixture.run().expect("run");

    let tree = fixture.tree();
    assert!(
        tree.iter().any(|path| path.ends_with("overlay/CLAUDE.md")),
        "the gitignored file is the whole reason the overlay exists: {tree:?}"
    );
    assert!(
        !tree.iter().any(|path| path.contains("node_modules")),
        "the overlay swallowed the junk it is filtered to exclude: {tree:?}"
    );
    assert!(
        !tree.iter().any(|path| path.contains("README.md")),
        "a tracked file belongs to history, not the overlay: {tree:?}"
    );
}

/// Git reports an untracked nested repository as one collapsed entry. Copying it
/// wholesale would put its `.git` into the store tree as loose files.
#[test]
fn an_untracked_nested_repository_is_not_swallowed_by_its_parent() {
    let dir = tempfile::tempdir().expect("temp dir");
    let outer = dir.path().join("A/outer");
    init_repo(&outer);
    write(&outer.join("tracked.md"), "t\n");
    commit(&outer, "one");

    let inner = outer.join("vendor/inner");
    init_repo(&inner);
    write(&inner.join("inner.md"), "i\n");
    commit(&inner, "one");

    let mut fixture = fixture(dir);
    fixture.run().expect("run");

    let tree = fixture.tree();
    for path in &tree {
        assert!(
            !path.contains(".git/"),
            "a .git path reached the tree: {path}"
        );
    }
    assert!(
        !tree
            .iter()
            .any(|path| path.contains("outer/overlay/vendor/inner/inner.md")),
        "the nested repository was swept into its parent's overlay: {tree:?}"
    );

    let refs = fixture.refs();
    for key in ["A/outer", "A/outer/vendor/inner"] {
        assert!(
            refs.iter()
                .any(|name| name.starts_with(&format!("refs/tycho/{key}/heads/"))),
            "{key} was not captured in its own right: {refs:?}"
        );
    }
}

/// Every platform repository on this machine is a submodule, so its `.git` is a file
/// holding a gitdir pointer rather than a directory.
#[test]
fn a_submodule_is_captured_in_its_own_right() {
    let dir = tempfile::tempdir().expect("temp dir");
    let parent = dir.path().join("A/org");
    init_repo(&parent);
    write(&parent.join("tracked.md"), "t\n");
    commit(&parent, "one");

    // The submodule's real git directory lives under the parent, and its worktree
    // carries a .git *file* pointing at it - the shape git actually produces.
    let child = parent.join("handbook");
    let gitdir = parent.join(".git/modules/handbook");
    fs::create_dir_all(&child).expect("mkdir");
    fs::create_dir_all(gitdir.parent().expect("a parent")).expect("mkdir");
    git(
        &child,
        &[
            "init",
            "--quiet",
            "-b",
            "main",
            "--separate-git-dir",
            &gitdir.display().to_string(),
            ".",
        ],
    );
    git(&child, &["config", "user.name", "t"]);
    git(&child, &["config", "user.email", "t@t"]);
    write(&child.join("doc.md"), "d\n");
    commit(&child, "one");
    assert!(
        child.join(".git").is_file(),
        "the fixture must use the submodule form, where .git is a file"
    );

    let mut fixture = fixture(dir);
    fixture.run().expect("run");

    let refs = fixture.refs();
    assert!(
        refs.iter()
            .any(|name| name.starts_with("refs/tycho/A/org/handbook/heads/")),
        "the submodule was reduced to a gitlink: {refs:?}"
    );
}

/// Invariant 1. Plain `git status` rewrites `.git/index` as a cache update, so
/// without `--no-optional-locks` every run would modify every repository it backs up.
/// Nothing proved this until now.
#[test]
fn a_run_does_not_write_to_the_repositories_it_reads() {
    let dir = tempfile::tempdir().expect("temp dir");
    let repo = dir.path().join("A/docs");
    init_repo(&repo);
    write(&repo.join("README.md"), "tracked\n");
    commit(&repo, "one");
    write(&repo.join("untracked.md"), "u\n");

    let index = repo.join(".git/index");
    let before = fs::read(&index).expect("read index");

    let mut fixture = fixture(dir);
    fixture.run().expect("run");

    let after = fs::read(&index).expect("read index");
    assert_eq!(
        before, after,
        "the run rewrote a source repository's index; --no-optional-locks is not doing its job"
    );
}

/// Both entries of a stash stack survive, and the top one does not collide with the
/// deeper ones. `refs/*` maps `refs/stash` onto a leaf ref, so the stack cannot live
/// under that same name.
#[test]
fn the_whole_stash_stack_is_captured_without_a_name_collision() {
    let dir = tempfile::tempdir().expect("temp dir");
    let repo = dir.path().join("A/docs");
    init_repo(&repo);
    write(&repo.join("a.txt"), "a\n");
    commit(&repo, "one");
    for round in 0..3 {
        write(&repo.join("a.txt"), &format!("change {round}\n"));
        git(&repo, &["stash", "--quiet"]);
    }

    let mut fixture = fixture(dir);
    fixture.run().expect("run");

    let refs = fixture.refs();
    assert!(
        refs.iter().any(|name| name == "refs/tycho/A/docs/stash"),
        "the top entry rides the main refspec: {refs:?}"
    );
    for index in 0..3 {
        assert!(
            refs.iter()
                .any(|name| name == &format!("refs/tycho/A/docs/stashes/{index}")),
            "stash@{{{index}}} was lost: {refs:?}"
        );
    }
}

/// `Feature` and `feature` map to one file on a case-insensitive volume. Git errors
/// on first exposure and the *next* fetch is silent, clobbering the captured tip.
#[test]
fn colliding_refnames_fail_loudly_rather_than_clobbering() {
    let dir = tempfile::tempdir().expect("temp dir");
    let repo = dir.path().join("A/docs");
    init_repo(&repo);
    write(&repo.join("a.txt"), "a\n");
    commit(&repo, "one");

    // Written straight into packed-refs, because the filesystem under the test may
    // itself refuse to hold both as loose files.
    let head = git(&repo, &["rev-parse", "HEAD"]);
    write(
        &repo.join(".git/packed-refs"),
        &format!(
            "# pack-refs with: peeled fully-peeled sorted \n{head} refs/heads/Feature\n{head} refs/heads/feature\n"
        ),
    );

    let mut fixture = fixture(dir);
    let error = fixture
        .run()
        .expect_err("a collision must fail the run rather than clobber a tip");
    assert!(
        error.to_string().contains("collide"),
        "the failure should name the collision: {error}"
    );
}

#[test]
fn repo_txt_records_what_was_there() {
    let dir = tempfile::tempdir().expect("temp dir");
    let repo = dir.path().join("A/docs");
    init_repo(&repo);
    write(&repo.join("a.txt"), "a\n");
    commit(&repo, "one");
    git(&repo, &["tag", "-a", "v1.0", "-m", "v1.0"]);
    git(&repo, &["branch", "dev"]);
    git(
        &repo,
        &["remote", "add", "origin", "git@github.com:example/docs.git"],
    );

    let mut fixture = fixture(dir);
    fixture.run().expect("run");

    let text = git(
        fixture.store.path_for_test(),
        &["show", "HEAD:.tycho/repos/A/docs/REPO.txt"],
    );
    for expected in [
        "origin    git@github.com:example/docs.git",
        "head      main @",
        "state     clean",
        "tags      v1.0",
    ] {
        assert!(text.contains(expected), "missing {expected:?} in:\n{text}");
    }
    assert!(
        text.contains("branches  dev, main") || text.contains("branches  main, dev"),
        "the branch list is what records liveness, since nothing is pruned:\n{text}"
    );
}

/// Two repositories both tagging `v1.0` must not fight over one ref in the store.
#[test]
fn two_repositories_tagging_the_same_version_stay_separate() {
    let dir = tempfile::tempdir().expect("temp dir");
    for name in ["one", "two"] {
        let repo = dir.path().join("A").join(name);
        init_repo(&repo);
        write(&repo.join("a.txt"), name);
        commit(&repo, "one");
        git(&repo, &["tag", "-a", "v1.0", "-m", "v1.0"]);
    }

    let mut fixture = fixture(dir);
    fixture.run().expect("run");

    let refs = fixture.refs();
    for name in ["one", "two"] {
        assert!(
            refs.iter()
                .any(|item| item == &format!("refs/tycho/A/{name}/tags/v1.0")),
            "{name}'s tag is missing: {refs:?}"
        );
    }
    assert!(
        !refs.iter().any(|item| item.starts_with("refs/tags/")),
        "a tag leaked into the store's own namespace: {refs:?}"
    );
}

/// `REPO.txt` records nothing that changes on its own.
///
/// It stamped the capture time, so every one of them rewrote on any run that crossed
/// a minute boundary: a no-op backup reported one changed file per repository and
/// committed a diff, which is the churn `metadata.rs` refuses an mtime to avoid. It
/// was never read back either - `parse_repo_txt` matches known fields and ignores the
/// rest - and the commit's own timestamp already says when a capture happened.
///
/// Asserted on the field names rather than by running twice, because the stamp was
/// minute-granular: two runs inside one minute produce identical text, so a
/// behavioural test passes with the bug present and fails only sometimes. This fails
/// the moment any field is added whose value the repository did not supply.
#[test]
fn repo_txt_records_only_facts_about_the_repository() {
    let dir = tempfile::tempdir().expect("temp dir");
    let repo = dir.path().join("A/docs");
    init_repo(&repo);
    write(&repo.join("README.md"), "tracked\n");
    commit(&repo, "one");

    let mut fixture = fixture(dir);
    fixture.run().expect("run");

    let path = fixture
        .tree()
        .into_iter()
        .find(|entry| entry.ends_with("REPO.txt"))
        .expect("a captured repository writes one");
    let text = git(
        fixture.store.path_for_test(),
        &["show", &format!("HEAD:{path}")],
    );

    let fields: Vec<String> = text
        .lines()
        .filter_map(|line| line.split_whitespace().next().map(str::to_owned))
        .collect();
    assert_eq!(
        fields,
        [
            "origin", "path", "head", "state", "branches", "tags", "stash"
        ],
        "{text}"
    );
}

/// A second run over an unchanged repository writes nothing. Weaker than the check
/// above - it only fails when the runs straddle whatever granularity a reintroduced
/// stamp used - but it is the property that actually matters.
#[test]
fn a_second_run_over_an_unchanged_repository_captures_nothing() {
    let dir = tempfile::tempdir().expect("temp dir");
    let repo = dir.path().join("A/docs");
    init_repo(&repo);
    write(&repo.join("README.md"), "tracked\n");
    commit(&repo, "one");

    let mut fixture = fixture(dir);
    fixture.run().expect("first run");
    let second = fixture.run().expect("second run");

    assert!(
        second.summary.is_empty(),
        "nothing changed, so nothing may be written: {:?}",
        second.summary
    );
}
