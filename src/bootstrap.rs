//! Layer 5. The hidden `__bootstrap` command: install Tycho and wire up shell
//! completions.
//!
//! Mirrors `cex __bootstrap` and `a sibling tool __bootstrap`, so the three tools are
//! learned once. It resolves its source checkout from `TYCHO_SRC` rather than from a
//! persisted store, and writes that variable back into the shell rc file so the next
//! run skips discovery.
//!
//! Resolution order: `TYCHO_SRC` if it points at a Tycho checkout, then the config
//! file's `source` key if it points at one, then a bounded scan of the usual checkout
//! roots, then the current directory.
//!
//! Colour goes through `cli::render` rather than the `colored` crate the sibling tools
//! use. Not a dependency argument - `colored` has no transitive ones - but this crate
//! already decides colour once at startup from `--no-color`, `NO_COLOR` and whether
//! stdout is a terminal, and having two answers to that question in one binary is
//! worse than the ergonomics are good.

use crate::cli::Cli;
use crate::cli::render::{CYAN, DIM, GREEN, paint};
use crate::cli::report::{at_file, report};
use clap::{Args, CommandFactory};
use clap_complete::{Shell as CompShell, generate};
use serde::Deserialize;
use std::io::Write as _;
use std::path::{Path, PathBuf};

const SRC_ENV: &str = "TYCHO_SRC";

/// Directories a scan will not descend into, because none of them holds a checkout and
/// `target` alone can hold tens of thousands of files.
const SKIP_DIRS: [&str; 5] = ["node_modules", "target", "build", "dist", ".git"];

#[derive(Clone, Debug, Args)]
pub struct BootstrapArgs {
    /// Write or refresh shell completions without reinstalling the binary
    #[arg(long)]
    pub only_completions: bool,

    /// Build with cargo's debug profile: faster to link, slower to run
    #[arg(long, short = 'd')]
    pub debug: bool,
}

/// # Errors
///
/// If no checkout can be found, or cargo, the completion write or the rc patch fails -
/// each already prints its own diagnostic, so the message itself is not meant to be
/// shown again.
pub fn run(args: &BootstrapArgs) -> Result<(), String> {
    println!("{}", paint("tycho __bootstrap", "1"));
    println!();

    if args.only_completions {
        write_completions()?;
    } else {
        let source = resolve_source()?;
        row("source", &source.display().to_string(), CYAN);
        install_binary(&source, args.debug)?;
        write_completions()?;
        persist_source_env(&source);
        starter_config();
    }

    println!();
    println!("{}", paint("done", GREEN));
    println!(
        "  Run {} to load completions now.",
        paint(Shell::detect().reload_hint(), CYAN)
    );
    Ok(())
}

/// Writes a starter config if there is none, so the first command after installing
/// finds a file rather than an `os error 2`.
///
/// Never overwrites: an existing config is the user's, and `config init` already
/// refuses one without `--force`. Silent when it declines, because "you already have
/// a config" is not news at install time.
fn starter_config() {
    let Ok(path) = crate::platform::config_path() else {
        return;
    };
    let path = path.as_path();
    if path.exists() {
        row(
            "config",
            &format!("{} (already there)", path.display()),
            DIM,
        );
        return;
    }
    let Some(home) = std::env::home_dir() else {
        return;
    };
    let starter = crate::config_edit::starter(&home.to_string_lossy());
    if path
        .parent()
        .is_some_and(|dir| std::fs::create_dir_all(dir).is_ok())
        && crate::sys::fs::write_atomic(path, starter.as_bytes()).is_ok()
    {
        row("config", &path.display().to_string(), CYAN);
    }
}

fn row(label: &str, value: &str, colour: &str) {
    println!(
        "{}  {}",
        paint(&format!("{label:<14}"), DIM),
        paint(value, colour)
    );
}

fn resolve_source() -> Result<PathBuf, String> {
    let config = crate::platform::config_path()
        .ok()
        .and_then(|path| config_source(path.as_path()));
    resolve_source_with(
        std::env::var_os(SRC_ENV).map(|value| value.to_string_lossy().into_owned()),
        config,
        discover,
    )
}

/// The decision itself, with every source of a candidate passed in rather than read
/// live - which is what lets a bad or absent one be exercised without a real
/// `$TYCHO_SRC`, config file or home directory.
fn resolve_source_with(
    env: Option<String>,
    config: Option<PathBuf>,
    scan: impl FnOnce() -> Option<PathBuf>,
) -> Result<PathBuf, String> {
    if let Some(raw) = env {
        let path = expand_home(&raw);
        if is_tycho_checkout(&path) {
            return Ok(path);
        }
        let shown = path.display();
        let _ = report! {
            warning: "${SRC_ENV} = '{shown}' is not a tycho checkout",
            at: at_file(&path),
        };
    }
    if let Some(path) = config {
        if is_tycho_checkout(&path) {
            return Ok(path);
        }
        let shown = path.display();
        let _ = report! {
            warning: "the config's source = '{shown}' is not a tycho checkout",
            at: at_file(&path),
        };
    }
    if let Some(found) = scan() {
        return Ok(found);
    }
    let cwd = std::env::current_dir().map_err(|error| {
        let _ = report! { error: "cannot read the current directory: {error}" };
        String::new()
    })?;
    if is_tycho_checkout(&cwd) {
        return Ok(cwd);
    }

    let example = match Shell::detect() {
        Shell::Pwsh => format!("$env:{SRC_ENV} = \"$HOME\\Developer\\tycho\""),
        _ => format!("export {SRC_ENV}=~/Developer/tycho"),
    };
    let _ = report! {
        error: "no tycho checkout found",
        note: "${SRC_ENV}, the config's `source` key and a scan of the usual checkout \
               roots all came up empty",
        recovery: { "{example}" => "point it at the repo" },
    };
    Err(String::new())
}

/// The config file's own `source` key alone: top-level, a sibling of `version`, and
/// read leniently rather than through `config::parse`, which validates backup
/// semantics this key is not part of and would refuse a file this build cannot fully
/// understand - exactly the file `__bootstrap` is most likely to be run against.
#[derive(Debug, Default, Deserialize)]
struct SourceProbe {
    source: Option<String>,
}

fn config_source(path: &Path) -> Option<PathBuf> {
    let text = std::fs::read_to_string(path).ok()?;
    let probe: SourceProbe = toml::from_str(&text).ok()?;
    probe.source.map(|raw| expand_home(&raw))
}

/// Where a checkout plausibly lives, per platform.
fn checkout_roots(home: &Path) -> Vec<PathBuf> {
    let mut roots = vec![
        home.join("Developer"),
        home.join("Projects"),
        home.join("src"),
        home.join("code"),
    ];
    if cfg!(windows) {
        roots.push(home.join("source").join("repos"));
    }
    roots
}

fn discover() -> Option<PathBuf> {
    let home = std::env::home_dir()?;
    let mut hits = Vec::new();
    for root in checkout_roots(&home) {
        if root.is_dir() {
            scan(&root, 0, &mut hits);
        }
    }
    hits.sort();
    hits.into_iter().next()
}

/// Bounded at four levels, because an unbounded walk of a home directory is how a
/// convenience command becomes a thing people avoid running.
fn scan(dir: &Path, depth: u32, hits: &mut Vec<PathBuf>) {
    if depth > 4 {
        return;
    }
    if is_tycho_checkout(dir) {
        hits.push(dir.canonicalize().unwrap_or_else(|_| dir.to_owned()));
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name.starts_with('.') || SKIP_DIRS.contains(&name.as_ref()) {
            continue;
        }
        scan(&path, depth + 1, hits);
    }
}

/// The manifest's own name, not the directory's: a folder called `tycho` holding
/// something else is not a checkout, and a checkout cloned under another name is.
fn is_tycho_checkout(path: &Path) -> bool {
    std::fs::read_to_string(path.join("Cargo.toml"))
        .is_ok_and(|text| text.contains("name = \"tycho\""))
}

fn install_binary(source: &Path, debug: bool) -> Result<(), String> {
    row(
        "binary",
        if debug {
            "installing (debug)..."
        } else {
            "installing (release)..."
        },
        DIM,
    );

    let mut args: Vec<&std::ffi::OsStr> = vec![
        "install".as_ref(),
        "--path".as_ref(),
        source.as_os_str(),
        "--force".as_ref(),
    ];
    if debug {
        args.push("--debug".as_ref());
    }
    // Through `sys::process::interactive`, not `command`: the build takes a minute,
    // prints progress worth watching, and any deadline short enough to protect a
    // backup would kill it.
    let status = crate::sys::process::interactive("cargo", &args).map_err(|error| {
        let _ = report! { error: "failed to spawn cargo: {error}" };
        String::new()
    })?;
    if !status.success() {
        let code = status.code();
        let shown = source.display();
        let _ = report! {
            error: "cargo install failed (exit {code:?})",
            recovery: { "cargo install --path {shown}" => "run it by hand to see why" },
        };
        return Err(String::new());
    }
    row("binary", "installed", GREEN);
    Ok(())
}

enum Shell {
    Zsh,
    Bash,
    Fish,
    Pwsh,
}

impl Shell {
    fn detect() -> Self {
        // Windows sets no `$SHELL`, and PowerShell is the shell there with completions
        // worth generating - `cmd.exe` has none at all.
        if cfg!(windows) {
            return Self::Pwsh;
        }
        let shell = std::env::var("SHELL").unwrap_or_default();
        if shell.contains("zsh") {
            Self::Zsh
        } else if shell.contains("fish") {
            Self::Fish
        } else {
            Self::Bash
        }
    }

    const fn reload_hint(&self) -> &'static str {
        match self {
            Self::Pwsh => ". $PROFILE",
            _ => "exec $SHELL",
        }
    }
}

fn write_completions() -> Result<(), String> {
    let home = home().map_err(|error| {
        let _ = report! { error: "{error}" };
        String::new()
    })?;
    let mut cmd = Cli::command();

    match Shell::detect() {
        Shell::Zsh => {
            write_completion_file(
                &home.join(".zsh/completions"),
                "_tycho",
                CompShell::Zsh,
                &mut cmd,
            )?;
            patch_rc(
                &home.join(".zshrc"),
                "# tycho completions",
                "# tycho completions\nfpath=(~/.zsh/completions $fpath)\nautoload -U compinit && compinit\n",
            )?;
        }
        Shell::Bash => {
            write_completion_file(
                &home.join(".bash_completion.d"),
                "tycho",
                CompShell::Bash,
                &mut cmd,
            )?;
            let rc = if home.join(".bash_profile").exists() {
                home.join(".bash_profile")
            } else {
                home.join(".bashrc")
            };
            patch_rc(
                &rc,
                "# tycho completions",
                "# tycho completions\n[ -f ~/.bash_completion.d/tycho ] && source ~/.bash_completion.d/tycho\n",
            )?;
        }
        Shell::Fish => {
            // fish auto-loads this directory, so there is no rc file to touch.
            write_completion_file(
                &home.join(".config/fish/completions"),
                "tycho.fish",
                CompShell::Fish,
                &mut cmd,
            )?;
        }
        Shell::Pwsh => {
            let profile = powershell_profile(&home);
            let dir = profile
                .parent()
                .map_or_else(|| home.clone(), Path::to_path_buf)
                .join("Completions");
            write_completion_file(&dir, "tycho.ps1", CompShell::PowerShell, &mut cmd)?;
            patch_rc(
                &profile,
                "# tycho completions",
                &format!(
                    "# tycho completions\n. \"{}\"\n",
                    dir.join("tycho.ps1").display()
                ),
            )?;
        }
    }
    Ok(())
}

/// Asked of PowerShell rather than assumed, because OneDrive folder redirection moves
/// `Documents` and the default path is then wrong.
#[cfg(windows)]
fn powershell_profile(home: &Path) -> PathBuf {
    for exe in ["pwsh", "powershell"] {
        let out = crate::sys::process::command(
            exe,
            &["-NoProfile", "-NonInteractive", "-Command", "$PROFILE"],
            crate::sys::process::Timeout::QUICK,
        );
        if let Ok(out) = out
            && out.status.success()
        {
            let path = String::from_utf8_lossy(&out.stdout).trim().to_owned();
            if !path.is_empty() {
                return PathBuf::from(path);
            }
        }
    }
    home.join("Documents")
        .join("WindowsPowerShell")
        .join("Microsoft.PowerShell_profile.ps1")
}

#[cfg(not(windows))]
fn powershell_profile(home: &Path) -> PathBuf {
    home.join(".config")
        .join("powershell")
        .join("Microsoft.PowerShell_profile.ps1")
}

fn write_completion_file(
    dir: &Path,
    file: &str,
    shell: CompShell,
    cmd: &mut clap::Command,
) -> Result<(), String> {
    std::fs::create_dir_all(dir).map_err(|error| {
        let shown = dir.display();
        let _ = report! {
            error: "cannot create {shown}",
            at: at_file(dir),
            note: "{error}",
        };
        String::new()
    })?;
    let path = dir.join(file);
    let mut buf = Vec::new();
    generate(shell, cmd, "tycho", &mut buf);
    std::fs::write(&path, &buf).map_err(|error| {
        let shown = path.display();
        let _ = report! {
            error: "cannot write {shown}",
            at: at_file(&path),
            note: "{error}",
        };
        String::new()
    })?;
    row("completions", &path.display().to_string(), CYAN);
    Ok(())
}

/// Appends the block once, keyed on a marker, so running this repeatedly does not
/// grow the file.
fn patch_rc(rc: &Path, marker: &str, block: &str) -> Result<(), String> {
    if std::fs::read_to_string(rc)
        .unwrap_or_default()
        .contains(marker)
    {
        row(
            "rc file",
            &format!("{} (already configured)", rc.display()),
            DIM,
        );
        return Ok(());
    }
    append(rc, &format!("\n{block}"))?;
    row("rc file", &rc.display().to_string(), CYAN);
    Ok(())
}

fn persist_source_env(source: &Path) {
    let Ok(home) = home() else {
        return;
    };
    let shell = Shell::detect();
    let rc = match shell {
        Shell::Pwsh => powershell_profile(&home),
        Shell::Fish => home.join(".config/fish/config.fish"),
        _ if home.join(".zshrc").exists() => home.join(".zshrc"),
        _ => home.join(".bashrc"),
    };
    let marker = match shell {
        Shell::Pwsh => format!("$env:{SRC_ENV} ="),
        _ => format!("{SRC_ENV}="),
    };
    if std::fs::read_to_string(&rc)
        .unwrap_or_default()
        .contains(&marker)
    {
        return;
    }
    let line = match shell {
        Shell::Pwsh => format!("\n$env:{SRC_ENV} = \"{}\"\n", source.display()),
        Shell::Fish => format!("\nset -gx {SRC_ENV} \"{}\"\n", source.display()),
        _ => format!("\nexport {SRC_ENV}=\"{}\"\n", source.display()),
    };
    if append(&rc, &line).is_ok() {
        row("env", &format!("{SRC_ENV}={}", source.display()), DIM);
    }
}

fn append(path: &Path, text: &str) -> Result<(), String> {
    // A PowerShell profile often has no directory yet on a fresh install.
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|error| {
            let shown = parent.display();
            let _ = report! {
                error: "cannot create {shown}",
                at: at_file(parent),
                note: "{error}",
            };
            String::new()
        })?;
    }
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .map_err(|error| {
            let shown = path.display();
            let _ = report! {
                error: "cannot open {shown}",
                at: at_file(path),
                note: "{error}",
            };
            String::new()
        })?;
    file.write_all(text.as_bytes()).map_err(|error| {
        let shown = path.display();
        let _ = report! {
            error: "cannot write {shown}",
            at: at_file(path),
            note: "{error}",
        };
        String::new()
    })
}

fn home() -> Result<PathBuf, String> {
    std::env::home_dir().ok_or_else(|| "there is no home directory".to_owned())
}

/// Expands a leading `~/`, `~\`, `$HOME/` or `${HOME}/` - the same two spellings
/// `config`'s watch paths accept unexpanded, so a value stored either way in
/// `TYCHO_SRC` or the config file resolves the same.
fn expand_home(text: &str) -> PathBuf {
    let rest = text
        .strip_prefix("~/")
        .or_else(|| text.strip_prefix("~\\"))
        .or_else(|| text.strip_prefix("$HOME/"))
        .or_else(|| text.strip_prefix("${HOME}/"));
    match rest {
        Some(rest) => std::env::home_dir().map_or_else(|| PathBuf::from(text), |h| h.join(rest)),
        None => PathBuf::from(text),
    }
}

#[cfg(test)]
mod tests {
    use super::{SKIP_DIRS, config_source, expand_home, is_tycho_checkout, resolve_source_with};
    use std::path::PathBuf;

    fn checkout() -> tempfile::TempDir {
        let dir = tempfile::tempdir().expect("temp dir");
        std::fs::write(
            dir.path().join("Cargo.toml"),
            "[package]\nname = \"tycho\"\n",
        )
        .expect("write");
        dir
    }

    /// The manifest's `name`, not the directory's: a checkout cloned under another
    /// name is still a checkout, and a folder called `tycho` holding something else
    /// is not.
    #[test]
    fn a_checkout_is_recognised_by_its_manifest_rather_than_its_directory_name() {
        let dir = tempfile::tempdir().expect("temp dir");
        assert!(!is_tycho_checkout(dir.path()));

        std::fs::write(
            dir.path().join("Cargo.toml"),
            "[package]\nname = \"other\"\n",
        )
        .expect("write");
        assert!(!is_tycho_checkout(dir.path()));

        std::fs::write(
            dir.path().join("Cargo.toml"),
            "[package]\nname = \"tycho\"\n",
        )
        .expect("write");
        assert!(is_tycho_checkout(dir.path()));
    }

    /// `target` alone can hold tens of thousands of files, and a scan that walks it is
    /// a command people learn not to run.
    #[test]
    fn the_scan_skips_the_directories_that_would_make_it_slow() {
        assert!(SKIP_DIRS.contains(&"target"));
        assert!(SKIP_DIRS.contains(&"node_modules"));
    }

    #[test]
    fn expand_home_accepts_dollar_home_as_well_as_tilde() {
        let home = std::env::home_dir().expect("a home dir for the test host");
        assert_eq!(
            expand_home("~/Developer/tycho"),
            home.join("Developer/tycho")
        );
        assert_eq!(
            expand_home("$HOME/Developer/tycho"),
            home.join("Developer/tycho")
        );
        assert_eq!(
            expand_home("${HOME}/Developer/tycho"),
            home.join("Developer/tycho")
        );
        assert_eq!(
            expand_home("/already/absolute"),
            PathBuf::from("/already/absolute")
        );
    }

    #[test]
    fn config_source_reads_the_top_level_key() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("tycho.toml");
        std::fs::write(&path, "version = 1\nsource = \"~/Developer/tycho\"\n").expect("write");
        assert_eq!(config_source(&path), Some(expand_home("~/Developer/tycho")));
    }

    #[test]
    fn config_source_is_none_without_the_key_the_file_or_valid_toml() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("tycho.toml");

        std::fs::write(&path, "version = 1\n").expect("write");
        assert_eq!(config_source(&path), None, "no source key");

        std::fs::write(&path, "not valid toml =====").expect("write");
        assert_eq!(config_source(&path), None, "malformed file");

        assert_eq!(
            config_source(&dir.path().join("missing.toml")),
            None,
            "no file at all"
        );
    }

    /// Requirement: the config's source is used when nothing else names one.
    #[test]
    fn config_source_wins_over_the_scan() {
        let config = checkout();
        let scanned = checkout();
        let result = resolve_source_with(None, Some(config.path().to_path_buf()), || {
            Some(scanned.path().to_path_buf())
        });
        assert_eq!(result, Ok(config.path().to_path_buf()));
    }

    /// Requirement: a config `source` that is not a checkout is a note, not a
    /// failure - the scan still runs.
    #[test]
    fn a_bad_config_source_falls_through_to_the_scan() {
        let not_a_checkout = tempfile::tempdir().expect("temp dir");
        let scanned = checkout();
        let result = resolve_source_with(None, Some(not_a_checkout.path().to_path_buf()), || {
            Some(scanned.path().to_path_buf())
        });
        assert_eq!(result, Ok(scanned.path().to_path_buf()));
    }

    /// Requirement: `TYCHO_SRC` outranks the config, which outranks the scan.
    #[test]
    fn tycho_src_still_beats_the_config() {
        let env = checkout();
        let config = checkout();
        let result = resolve_source_with(
            Some(env.path().display().to_string()),
            Some(config.path().to_path_buf()),
            || None,
        );
        assert_eq!(result, Ok(env.path().to_path_buf()));
    }
}
