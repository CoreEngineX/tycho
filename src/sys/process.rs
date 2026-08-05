//! The only door to a child process.
//!
//! Three invariants are enforced by *how* git is spawned rather than by what it is
//! asked to do, and each fails silently if one call site forgets: the config pins
//! keep capture byte-exact, `--no-optional-locks` keeps watched repositories
//! read-only, and the timeout keeps a blocked child from holding the profile lock
//! forever. Routing every invocation through here makes all three structural.

use std::io::{self, Read, Write};
use std::path::Path;
use std::process::{Child, Command, ExitStatus, Output, Stdio};
use std::thread;
use std::time::{Duration, Instant};

/// Pinned onto every git invocation. `core.autocrlf` alone stores a CRLF file
/// LF-normalized, and `commit-tree` inherits the user's identity and hard-fails
/// under `user.useConfigOnly=true`.
const PINNED: [&str; 13] = [
    "--no-optional-locks",
    "-c",
    "core.autocrlf=false",
    "-c",
    "core.eol=lf",
    "-c",
    "core.attributesFile=/dev/null",
    "-c",
    "core.quotePath=false",
    "-c",
    "user.name=tycho",
    "-c",
    "user.email=tycho@localhost",
];

/// Variables that redirect git away from what the arguments say. `-C` only changes
/// directory, so an inherited `GIT_DIR` would write the store's objects into a git
/// hook's repository at exit 0; and the identity variables outrank `-c user.name`,
/// so an inherited one would author a backup as whoever the hook was running for.
const HIJACKING: [&str; 18] = [
    "GIT_CONFIG_GLOBAL",
    "GIT_CONFIG_SYSTEM",
    "GIT_DIR",
    "GIT_WORK_TREE",
    "GIT_INDEX_FILE",
    "GIT_OBJECT_DIRECTORY",
    "GIT_ALTERNATE_OBJECT_DIRECTORIES",
    "GIT_COMMON_DIR",
    "GIT_NAMESPACE",
    "GIT_CEILING_DIRECTORIES",
    "GIT_CONFIG",
    "GIT_CONFIG_COUNT",
    "GIT_AUTHOR_NAME",
    "GIT_AUTHOR_EMAIL",
    "GIT_AUTHOR_DATE",
    "GIT_COMMITTER_NAME",
    "GIT_COMMITTER_EMAIL",
    "GIT_COMMITTER_DATE",
];

/// How long a child may run before it is killed and the run fails loudly.
///
/// A generous total rather than an idle timer: the `st_mode` classification in
/// `sys::fs` already keeps a FIFO or a device out of `hash-object`, so this is the
/// backstop for an unhandled blocking case, not the primary defence.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Timeout(Duration);

impl Timeout {
    /// `rev-parse`, `config`, `symbolic-ref`, `for-each-ref`, `check-ref-format`.
    pub const QUICK: Self = Self(Duration::from_secs(30));
    /// The hash batch, `write-tree`, `fetch`, `gc`.
    pub const WORK: Self = Self(Duration::from_secs(600));
    /// A push into a synced cloud folder, which can be very slow.
    pub const REMOTE: Self = Self(Duration::from_secs(1800));
    /// Starting an interpreter, which is not a git call and does not cost what one
    /// costs. PowerShell loads WinRT before it runs a line, and on a loaded machine
    /// that alone passed 30 seconds twice - so `QUICK`, sized for `rev-parse`, failed
    /// notifications that were about to work. Nothing waits on the result: the caller
    /// discards it, because a banner is a convenience and a backup is not.
    pub const INTERPRETER: Self = Self(Duration::from_secs(120));

    #[must_use]
    pub const fn from_millis(millis: u64) -> Self {
        Self(Duration::from_millis(millis))
    }

    #[must_use]
    pub const fn as_duration(self) -> Duration {
        self.0
    }
}

#[derive(Debug, thiserror::Error)]
pub enum RunError {
    #[error("could not run {program}: {source}")]
    Spawn {
        program: String,
        #[source]
        source: io::Error,
    },
    #[error("{command} timed out after {}s and was killed", .after.as_secs())]
    Timeout { command: String, after: Duration },
    #[error("{command} exited {status}: {stderr}")]
    Failed {
        command: String,
        status: ExitStatus,
        stderr: String,
    },
    #[error("{command}: {source}")]
    Io {
        command: String,
        #[source]
        source: io::Error,
    },
}

/// Runs a child with the terminal attached and no timeout, for a command a person is
/// watching.
///
/// The deliberate opposite of [`command`], and the only other way out of this module.
/// [`command`] captures output and kills the child at a deadline, which is right for
/// everything on the backup path: a hung `git` must not become a silently stalled
/// backup. It is wrong for `cargo install`, which legitimately takes a minute, prints
/// progress worth seeing, and would be killed mid-build by any deadline short enough
/// to be useful elsewhere.
///
/// Nothing here is pinned, because there is no config to pin - the pins in [`command`]
/// exist to stop git reading the ambient environment, and this never runs git.
///
/// # Errors
///
/// If the program cannot be spawned.
pub fn interactive(
    program: &str,
    args: &[&std::ffi::OsStr],
) -> Result<std::process::ExitStatus, RunError> {
    std::process::Command::new(program)
        .args(args)
        .status()
        .map_err(|source| RunError::Spawn {
            program: program.to_owned(),
            source,
        })
}

/// A replacement global config for git, used for exactly one setting.
///
/// `safe.directory` is the only thing Tycho needs that **cannot** be pinned with
/// `-c`: git reads it from the system and global config files and nowhere else,
/// deliberately, so that a repository cannot whitelist itself. Measured - passing
/// `-c safe.directory=<path>` changes nothing, and the push still fails.
///
/// `GIT_CONFIG_GLOBAL` is the one lever left, and it replaces the user's global
/// config wholesale, so the file written here `include.path`s theirs first. That way
/// a trusted remote costs them none of their own settings and leaves no trace on the
/// machine, which `git config --global --add safe.directory` would not.
static TRUST_CONFIG: std::sync::OnceLock<std::path::PathBuf> = std::sync::OnceLock::new();

/// Installs the scoped config for this process. Later calls are ignored, so the
/// composition root decides once.
pub fn trust_config(path: std::path::PathBuf) {
    let _ = TRUST_CONFIG.set(path);
}

/// A git invocation target: a directory, and optionally the scratch index that
/// `store.md` requires be built from nothing each run.
#[derive(Clone, Copy, Debug)]
pub struct Git<'a> {
    cwd: &'a Path,
    index: Option<&'a Path>,
}

impl<'a> Git<'a> {
    #[must_use]
    pub const fn at(cwd: &'a Path) -> Self {
        Self { cwd, index: None }
    }

    #[must_use]
    pub const fn with_index(mut self, index: &'a Path) -> Self {
        self.index = Some(index);
        self
    }

    /// Runs to completion and hands back the output whatever the exit status, since
    /// a non-zero status is the answer for `check-ref-format` and the case
    /// `remotes.md` needs to classify for `push`.
    ///
    /// # Errors
    ///
    /// If git cannot be spawned, times out, or a pipe fails.
    pub fn run(&self, args: &[&str], limit: Timeout) -> Result<Output, RunError> {
        self.stream(args, std::iter::empty(), limit)
    }

    /// As [`Git::run`], but a non-zero exit becomes an error carrying git's stderr.
    ///
    /// # Errors
    ///
    /// As [`Git::run`], plus a non-zero exit status.
    pub fn checked(&self, args: &[&str], limit: Timeout) -> Result<Output, RunError> {
        let label = label("git", args);
        let output = self.run(args, limit)?;
        if output.status.success() {
            return Ok(output);
        }
        Err(RunError::Failed {
            command: label,
            status: output.status,
            stderr: String::from_utf8_lossy(&output.stderr).trim().to_owned(),
        })
    }

    /// Feeds `input` to git's stdin while draining its stdout.
    ///
    /// Writing every byte and only then reading fills the pipe buffer and wedges
    /// both sides - measured at roughly 2,300 files with 145-byte paths, and the
    /// threshold scales with path length.
    ///
    /// # Errors
    ///
    /// As [`Git::run`].
    pub fn stream<I>(&self, args: &[&str], input: I, limit: Timeout) -> Result<Output, RunError>
    where
        I: Iterator<Item = Vec<u8>> + Send,
    {
        drive(self.build(args), &label("git", args), input, limit)
    }

    fn build(&self, args: &[&str]) -> Command {
        let mut command = Command::new("git");
        command.arg("-C").arg(self.cwd).args(PINNED).args(args);
        for name in HIJACKING {
            command.env_remove(name);
        }
        // A credential prompt under launchd would block until the timeout.
        command.env("GIT_TERMINAL_PROMPT", "0");
        // Set after the removals above, which strip an inherited one, so the only
        // replacement global config git can see is the one Tycho wrote.
        if let Some(config) = TRUST_CONFIG.get() {
            command.env("GIT_CONFIG_GLOBAL", config);
        }
        if let Some(index) = self.index {
            command.env("GIT_INDEX_FILE", index);
        }
        command
    }
}

/// Runs a program with the terminal's own stdout and stderr, and no timeout.
///
/// The one deliberate exception to the timeout rule, and it exists for `tycho log -f`:
/// a follow *is* an unbounded wait, and buffering its output through a pipe would
/// defeat the point of following. Nothing that reads a result may use this - the
/// caller gets an exit code and nothing else.
///
/// # Errors
///
/// If the program cannot be spawned or waited on.
pub fn stream_to_terminal(program: &str, args: &[&str]) -> Result<i32, RunError> {
    let mut child = Command::new(program)
        .args(args)
        .spawn()
        .map_err(|source| RunError::Spawn {
            program: program.to_owned(),
            source,
        })?;
    let status = child.wait().map_err(|source| RunError::Io {
        command: program.to_owned(),
        source,
    })?;
    Ok(status.code().unwrap_or(1))
}

/// Runs a non-git program. Separate from [`Git`] so nothing can reach git without
/// the pins.
///
/// # Errors
///
/// If the program cannot be spawned, times out, or a pipe fails.
pub fn command(program: &str, args: &[&str], limit: Timeout) -> Result<Output, RunError> {
    let mut child = Command::new(program);
    child.args(args);
    drive(child, &label(program, args), std::iter::empty(), limit)
}

fn label(program: &str, args: &[&str]) -> String {
    let mut text = program.to_owned();
    for arg in args {
        text.push(' ');
        text.push_str(arg);
    }
    text
}

fn drive<I>(mut command: Command, label: &str, input: I, limit: Timeout) -> Result<Output, RunError>
where
    I: Iterator<Item = Vec<u8>> + Send,
{
    command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = command.spawn().map_err(|source| RunError::Spawn {
        program: label.split(' ').next().unwrap_or(label).to_owned(),
        source,
    })?;

    let (Some(mut stdin), Some(mut stdout), Some(mut stderr)) =
        (child.stdin.take(), child.stdout.take(), child.stderr.take())
    else {
        return Err(RunError::Io {
            command: label.to_owned(),
            source: io::Error::other("child was spawned without its pipes"),
        });
    };

    thread::scope(|scope| {
        let writer = scope.spawn(move || {
            for chunk in input {
                stdin.write_all(&chunk)?;
            }
            stdin.flush()
        });
        let out_reader = scope.spawn(move || {
            let mut buffer = Vec::new();
            stdout.read_to_end(&mut buffer).map(|_| buffer)
        });
        let err_reader = scope.spawn(move || {
            let mut buffer = Vec::new();
            stderr.read_to_end(&mut buffer).map(|_| buffer)
        });

        let waited = wait_with_timeout(&mut child, limit).map_err(|source| RunError::Io {
            command: label.to_owned(),
            source,
        })?;

        let stdout = joined(out_reader.join(), label)?;
        let stderr = joined(err_reader.join(), label)?;
        let written = writer.join();

        let Some(status) = waited else {
            return Err(RunError::Timeout {
                command: label.to_owned(),
                after: limit.as_duration(),
            });
        };

        // A write error against a child that then failed is a symptom, not the
        // cause, so git's own stderr wins. A broken pipe is not an error at all:
        // a command that reads no stdin, such as `rev-parse`, closes it while the
        // writer is still going.
        if status.success() {
            match written {
                Ok(Ok(())) => {}
                Ok(Err(source)) if source.kind() == io::ErrorKind::BrokenPipe => {}
                Ok(Err(source)) => {
                    return Err(RunError::Io {
                        command: label.to_owned(),
                        source,
                    });
                }
                Err(_) => {
                    return Err(RunError::Io {
                        command: label.to_owned(),
                        source: io::Error::other("the stdin thread panicked"),
                    });
                }
            }
        }

        Ok(Output {
            status,
            stdout,
            stderr,
        })
    })
}

fn joined<T>(result: thread::Result<io::Result<T>>, label: &str) -> Result<T, RunError> {
    match result {
        Ok(Ok(value)) => Ok(value),
        Ok(Err(source)) => Err(RunError::Io {
            command: label.to_owned(),
            source,
        }),
        Err(_) => Err(RunError::Io {
            command: label.to_owned(),
            source: io::Error::other("a pipe thread panicked"),
        }),
    }
}

/// `None` when the deadline passed, in which case the child has been killed and
/// reaped. `Child::kill` needs `&mut Child`, so a watchdog thread cannot hold it
/// while this one waits - polling is the shape that works without a dependency.
fn wait_with_timeout(child: &mut Child, limit: Timeout) -> io::Result<Option<ExitStatus>> {
    let deadline = Instant::now() + limit.as_duration();
    let mut poll = Duration::from_micros(200);
    loop {
        if let Some(status) = child.try_wait()? {
            return Ok(Some(status));
        }
        let now = Instant::now();
        if now >= deadline {
            child.kill()?;
            child.wait()?;
            return Ok(None);
        }
        thread::sleep(poll.min(deadline.saturating_duration_since(now)));
        poll = (poll * 2).min(Duration::from_millis(20));
    }
}

#[cfg(test)]
mod tests {
    use super::{Git, RunError, Timeout, command};
    use std::path::Path;

    fn text(bytes: &[u8]) -> String {
        String::from_utf8_lossy(bytes).trim().to_owned()
    }

    /// `ping` rather than `sleep` on Windows, which has neither `sleep` nor a
    /// `timeout` that runs without a console.
    #[cfg(windows)]
    const SLOW: (&str, &[&str]) = ("ping", &["-n", "31", "127.0.0.1"]);
    #[cfg(unix)]
    const SLOW: (&str, &[&str]) = ("sleep", &["30"]);

    #[test]
    fn a_child_that_outlives_its_limit_is_killed() {
        let (program, args) = SLOW;
        let error = command(program, args, Timeout::from_millis(150))
            .expect_err("a 30s child cannot finish in 150ms");
        assert!(
            matches!(error, RunError::Timeout { .. }),
            "expected a timeout, got {error}"
        );
    }

    #[test]
    fn a_missing_program_names_itself() {
        let error = command("tycho-no-such-program", &[], Timeout::QUICK)
            .expect_err("the program does not exist");
        assert!(
            matches!(error, RunError::Spawn { .. }),
            "expected a spawn failure, got {error}"
        );
    }

    #[test]
    fn the_config_pins_reach_git() {
        let git = Git::at(Path::new("."));
        for (key, want) in [
            ("core.autocrlf", "false"),
            ("core.eol", "lf"),
            ("core.quotePath", "false"),
            ("user.name", "tycho"),
            ("user.email", "tycho@localhost"),
        ] {
            let out = git
                .checked(&["config", "--get", key], Timeout::QUICK)
                .expect("git config reads the pinned value");
            assert_eq!(text(&out.stdout), want, "{key}");
        }
    }

    #[test]
    fn a_non_zero_exit_is_the_answer_for_run_and_an_error_for_checked() {
        let git = Git::at(Path::new("."));
        let args = ["check-ref-format", "refs/heads/bad name"];
        let out = git.run(&args, Timeout::QUICK).expect("git ran");
        assert!(!out.status.success());
        let error = git
            .checked(&args, Timeout::QUICK)
            .expect_err("checked fails");
        assert!(
            matches!(error, RunError::Failed { .. }),
            "expected a failure, got {error}"
        );
    }

    #[test]
    fn the_hijacking_variables_are_removed_from_the_child() {
        let index = Path::new("/tmp/scratch-index");
        let command = Git::at(Path::new(".")).with_index(index).build(&["status"]);
        let env: Vec<_> = command.get_envs().collect();

        for name in super::HIJACKING {
            if name == "GIT_INDEX_FILE" {
                continue;
            }
            assert!(
                env.contains(&(name.as_ref(), None)),
                "{name} is not removed from the child environment"
            );
        }
        assert!(env.contains(&("GIT_INDEX_FILE".as_ref(), Some(index.as_ref()))));
        assert!(env.contains(&("GIT_TERMINAL_PROMPT".as_ref(), Some("0".as_ref()))));
    }

    #[test]
    fn stdin_is_written_while_stdout_is_drained() {
        let git = Git::at(Path::new("."));
        let out = git
            .stream(
                &["hash-object", "-t", "blob", "--stdin"],
                std::iter::once(b"hello\n".to_vec()),
                Timeout::QUICK,
            )
            .expect("git ran");
        assert_eq!(
            text(&out.stdout),
            "ce013625030ba8dba906f756967f9e9ca394464a"
        );
    }
}
