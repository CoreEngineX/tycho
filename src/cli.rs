//! Layer 5. The clap surface, the exit-code contract, and rendering.

pub mod doctor;
pub mod render;
pub mod rules;
pub mod run;
pub mod service;

use clap::{Args, Parser, Subcommand};
use std::path::PathBuf;
use std::process::ExitCode;

/// The mark from `docs/logo.svg`, rendered. ASCII only, because `--help` is read
/// through pipes and log files as often as on a terminal.
const LOGO: &str = r"
                      :-=*##%%%%%%%%#**=-:
                  +#%@@@@%%%%@@@@@@%%%@@@@@%#=
 .-===--:..       =%+=:.     .%@@%.     .-=*%.
#@@+..                        +@@+
+@@-                          +@@+
 -%@*.                        *@@+
   =%@#:                      *@@+          ......
     :#@@+.                   *@@*       ...:::-=+##+.
        =#@%+:                *@@*                 =@@-
           =#@@*-             *@@*                 -@@=
              -*@@#=.         *@@*                =@@+
                 .+#@@*-.     *@@*              =%@#:
                     :*%@%*-.  :*+           :*@@*:
             .:          -*%@%*-.        .-*@@#=        ..
            +-              .-+#%@#+-.   +##-             :-
           %-                   ..-*%@%#+-.                 =+
          #@.              .-*%@@*-   .-*%%@%*+-.            -%.
          *@@*-:....:-=+#%@@@%*-:.         .-+*%%%%#*+-::...:+@%
           -*%@@@@@@@@%#*=-.  .=*=                :-=+*##%%%%#+.
                              *@@*
                              *@@+
                              *@@+
                              *@@+
                              +@@+";

#[derive(Debug, Parser)]
#[command(name = "tycho", version, about, before_help = LOGO)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
    /// Never colour the output. `NO_COLOR` and a non-terminal stdout mean the same
    #[arg(long, global = true)]
    pub no_color: bool,
}

#[derive(Clone, Debug, Args)]
pub struct RunArgs {
    /// Which profile to back up
    pub profile: Option<String>,
    /// Every profile. The installed agents run one profile each, per scheduling.md
    #[arg(long, conflicts_with = "profile")]
    pub all: bool,
    /// Print the plan and stop before touching the store
    #[arg(long)]
    pub dry_run: bool,
    /// Omit the repository table, which is the expensive half
    #[arg(long)]
    pub quick: bool,
    /// Accept a large drop in a root's entry count
    #[arg(long)]
    pub allow_shrink: bool,
    /// Read this config file instead of the default location
    #[arg(long, value_name = "PATH")]
    pub config: Option<PathBuf>,
}

#[derive(Clone, Debug, Args)]
pub struct PushArgs {
    /// Which profile's store to push
    pub profile: Option<String>,
    /// Every profile, which is what the shared catch-up agent runs
    #[arg(long, conflicts_with = "profile")]
    pub all: bool,
    /// Read this config file instead of the default location
    #[arg(long, value_name = "PATH")]
    pub config: Option<PathBuf>,
}

#[derive(Clone, Debug, Args)]
pub struct StatusArgs {
    /// Limit the report to one profile
    pub profile: Option<String>,
    /// Exit non-zero on red, for monitoring
    #[arg(long)]
    pub check: bool,
    /// With `--check`, make yellow non-zero too
    #[arg(long)]
    pub strict: bool,
    /// Read this config file instead of the default location
    #[arg(long, value_name = "PATH")]
    pub config: Option<PathBuf>,
}

#[derive(Clone, Debug, Args)]
pub struct RestoreArgs {
    /// Which profile's store to read
    pub profile: Option<String>,
    /// Where to put the recovered files. Required, with no default and no fallback
    #[arg(long, value_name = "DIR")]
    pub into: PathBuf,
    /// Read this store or remote directly, without a config file
    #[arg(long, value_name = "PATH", conflicts_with = "profile")]
    pub store: Option<PathBuf>,
    /// The backup to use: "2026-10-19 12:00", "2026-10-19", or "3 days ago"
    #[arg(long, value_name = "WHEN")]
    pub at: Option<String>,
    /// Write a bundle of the named repository's history instead of a working tree
    #[arg(long)]
    pub bundle: bool,
    /// Restore into a destination that already holds things
    #[arg(long)]
    pub force: bool,
    /// Read this config file instead of the default location
    #[arg(long, value_name = "PATH")]
    pub config: Option<PathBuf>,
    /// Paths to recover, after `--`. Omit for the whole backup
    #[arg(last = true, value_name = "PATH")]
    pub paths: Vec<PathBuf>,
}

/// Where a restore reads from.
///
/// A sum type rather than two optionals, because `--store` reads **no config file at
/// all** and that is the whole disaster case: on a replacement machine there is no
/// config, no state file and no local store, so a restore that needed a profile from
/// a config would be unusable exactly when it is needed.
#[derive(Clone, Debug)]
pub enum Source {
    Profile(Option<String>),
    Store(PathBuf),
}

impl RestoreArgs {
    #[must_use]
    pub fn source(&self) -> Source {
        self.store.as_ref().map_or_else(
            || Source::Profile(self.profile.clone()),
            |path| Source::Store(path.clone()),
        )
    }
}

#[derive(Clone, Debug, Args)]
pub struct DoctorArgs {
    /// Limit the report to one profile
    pub profile: Option<String>,
    /// Measure what cannot be read: the agent's Full Disk Access grant through
    /// launchd, notification delivery, and a full fsck
    #[arg(long)]
    pub deep: bool,
    /// Limit the remote checks to one name
    #[arg(long, value_name = "NAME")]
    pub remote: Option<String>,
    /// Read this config file instead of the default location
    #[arg(long, value_name = "PATH")]
    pub config: Option<PathBuf>,
}

#[derive(Clone, Debug, Args)]
pub struct ProbeArgs {
    /// Where to write what was found
    #[arg(long, value_name = "PATH")]
    pub out: PathBuf,
    /// Roots to try reading
    pub roots: Vec<PathBuf>,
}

#[derive(Clone, Debug, Args)]
pub struct LogArgs {
    /// Which profile's log
    pub profile: Option<String>,
    /// Follow the file as it grows
    #[arg(short = 'f', long)]
    pub follow: bool,
    /// How many lines to show
    #[arg(short = 'n', default_value_t = 40)]
    pub count: usize,
    /// Read this config file instead of the default location
    #[arg(long, value_name = "PATH")]
    pub config: Option<PathBuf>,
}

#[derive(Clone, Debug, Args)]
pub struct ServiceArgs {
    #[command(subcommand)]
    pub action: ServiceAction,
    /// Which profile's agent. Omit for every profile in the config
    #[arg(global = true)]
    pub profile: Option<String>,
    /// Read this config file instead of the default location
    #[arg(long, value_name = "PATH", global = true)]
    pub config: Option<PathBuf>,
}

#[derive(Clone, Copy, Debug, Subcommand)]
pub enum ServiceAction {
    /// Generate the plists and load them
    Install,
    /// Unload and remove the plists. Never touches the store or any backup
    Uninstall,
    /// Whether each agent is loaded, its last exit status, and its next run
    Status,
    /// Unload and reload, for after a config change
    Restart,
}

#[derive(Clone, Debug, Args)]
pub struct HistoryArgs {
    /// Which profile's store to read
    pub profile: Option<String>,
    /// One path's history instead of the backup list
    #[arg(long, value_name = "PATH")]
    pub path: Option<PathBuf>,
    /// How many backups to list
    #[arg(short = 'n', default_value_t = 20)]
    pub count: usize,
    /// Read this config file instead of the default location
    #[arg(long, value_name = "PATH")]
    pub config: Option<PathBuf>,
}

#[derive(Clone, Debug, Args)]
pub struct ConfigArgs {
    #[command(subcommand)]
    pub action: ConfigAction,
    /// Read this config file instead of the default location
    #[arg(long, value_name = "PATH", global = true)]
    pub config: Option<PathBuf>,
}

#[derive(Clone, Copy, Debug, Subcommand)]
pub enum ConfigAction {
    /// Report every problem at once, rather than stopping at the first
    Check,
    /// Print where the config file is read from
    Path,
    /// Write a starter file, refusing if one is already there
    Init {
        /// Overwrite, after writing a `.bak` beside it
        #[arg(long)]
        force: bool,
    },
}

#[derive(Clone, Debug, Args)]
pub struct RuleArgs {
    #[command(subcommand)]
    pub action: RuleAction,
    /// Which profile. A flag rather than a positional, because `watch add PATH` and
    /// `watch add PROFILE PATH` are otherwise indistinguishable to a parser
    #[arg(short = 'p', long, global = true, value_name = "PROFILE")]
    pub profile: Option<String>,
    /// Read this config file instead of the default location
    #[arg(long, value_name = "PATH", global = true)]
    pub config: Option<PathBuf>,
}

#[derive(Clone, Debug, Subcommand)]
pub enum RuleAction {
    /// Add an entry
    Add { value: String },
    /// Remove an entry
    Rm { value: String },
    /// Print the entries
    List,
}

#[derive(Clone, Debug, Subcommand)]
pub enum Command {
    /// Capture, commit and push
    Run(RunArgs),
    /// Push what the store already holds to any remote that is behind
    Push(PushArgs),
    /// Per-profile schedule, store summary and remote health
    Status(StatusArgs),
    /// The store's commits, rendered
    History(HistoryArgs),
    /// Recover to a destination directory
    Restore(RestoreArgs),
    /// Manage watched roots
    Watch(RuleArgs),
    /// Manage ignore rules
    Ignore(RuleArgs),
    /// Manage re-include rules
    Reinclude(RuleArgs),
    /// Validate, locate or create the config file
    Config(ConfigArgs),
    /// Scheduler lifecycle for the backup agents
    Service(ServiceArgs),
    /// Environment, service, remote and volume health
    Doctor(DoctorArgs),
    /// Measure the agent's own Full Disk Access grant
    #[command(hide = true)]
    ProbeAccess(ProbeArgs),
    /// Tail the log file
    Log(LogArgs),
}

impl Command {
    /// Every subcommand as it is typed, which must stay identical to clap's own
    /// kebab-case rename of the variants.
    pub const NAMES: [&'static str; 13] = [
        "run",
        "push",
        "status",
        "history",
        "restore",
        "watch",
        "ignore",
        "reinclude",
        "config",
        "service",
        "doctor",
        "probe-access",
        "log",
    ];

    /// The one command `--help` does not list, because `cli.md` marks it internal.
    pub const INTERNAL: &'static str = "probe-access";

    #[must_use]
    pub const fn name(&self) -> &'static str {
        match self {
            Self::Run(_) => "run",
            Self::Push(_) => "push",
            Self::Status(_) => "status",
            Self::History(_) => "history",
            Self::Restore(_) => "restore",
            Self::Watch(_) => "watch",
            Self::Ignore(_) => "ignore",
            Self::Reinclude(_) => "reinclude",
            Self::Config(_) => "config",
            Self::Service(_) => "service",
            Self::Doctor(_) => "doctor",
            Self::ProbeAccess(_) => "probe-access",
            Self::Log(_) => "log",
        }
    }
}

/// The process exit contract. A usage error exits 2, which clap emits during parsing
/// before any of this is reached.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[must_use]
pub enum Exit {
    Ok,
    Failure,
    Warning,
}

impl Exit {
    #[must_use]
    pub const fn code(self) -> u8 {
        match self {
            Self::Ok => 0,
            Self::Failure => 1,
            Self::Warning => 3,
        }
    }
}

impl From<Exit> for ExitCode {
    fn from(exit: Exit) -> Self {
        Self::from(exit.code())
    }
}

pub fn dispatch(command: Command) -> Exit {
    match command {
        Command::Run(args) => run::run(&args),
        Command::Push(args) => run::push(&args),
        Command::Status(args) => run::status(&args),
        Command::Config(args) => run::config(&args),
        Command::History(args) => run::history(&args),
        Command::Restore(args) => run::restore(&args),
        Command::Service(args) => service::dispatch(&args),
        Command::Log(args) => run::log(&args),
        Command::Doctor(args) => doctor::dispatch(&args),
        Command::ProbeAccess(args) => doctor::probe_access(&args),
        Command::Watch(args) => rules::dispatch(crate::config_edit::List::Watch, &args),
        Command::Ignore(args) => rules::dispatch(crate::config_edit::List::Ignore, &args),
        Command::Reinclude(args) => rules::dispatch(crate::config_edit::List::Reinclude, &args),
    }
}

#[cfg(test)]
mod tests {
    use super::{Cli, Command, Exit};
    use clap::Parser;

    #[test]
    fn exit_codes_match_the_contract() {
        assert_eq!(Exit::Ok.code(), 0);
        assert_eq!(Exit::Failure.code(), 1);
        assert_eq!(Exit::Warning.code(), 3);
    }

    /// Asking each name for its own help proves clap knows it, without depending on
    /// which of them take required arguments.
    #[test]
    fn every_name_is_the_name_clap_accepts() {
        for name in Command::NAMES {
            let error = Cli::try_parse_from(["tycho", name, "--help"])
                .err()
                .unwrap_or_else(|| panic!("'{name} --help' should have printed help"));
            assert_eq!(
                error.kind(),
                clap::error::ErrorKind::DisplayHelp,
                "'{name}' is not a subcommand clap knows"
            );
        }
    }

    #[test]
    fn a_parsed_command_reports_the_name_it_was_typed_as() {
        for (args, want) in [
            (vec!["tycho", "run"], "run"),
            (vec!["tycho", "config", "check"], "config"),
            (vec!["tycho", "doctor"], "doctor"),
        ] {
            let parsed = Cli::try_parse_from(&args).expect("parses");
            assert_eq!(parsed.command.name(), want);
        }
    }
}
