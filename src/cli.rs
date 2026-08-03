//! Layer 5. The clap surface, the exit-code contract, and rendering.

pub mod render;
pub mod run;

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
}

#[derive(Clone, Debug, Args)]
pub struct RunArgs {
    /// Which profile to back up
    pub profile: Option<String>,
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
pub struct HistoryArgs {
    /// Which profile's store to read
    pub profile: Option<String>,
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
    Restore,
    /// Manage watched roots
    Watch,
    /// Manage ignore rules
    Ignore,
    /// Manage re-include rules
    Reinclude,
    /// Validate, locate or create the config file
    Config(ConfigArgs),
    /// launchd lifecycle for the backup agents
    Service,
    /// Environment, service, remote and volume health
    Doctor,
    /// Measure the agent's own Full Disk Access grant
    #[command(hide = true)]
    ProbeAccess,
    /// Tail the log file
    Log,
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
            Self::Restore => "restore",
            Self::Watch => "watch",
            Self::Ignore => "ignore",
            Self::Reinclude => "reinclude",
            Self::Config(_) => "config",
            Self::Service => "service",
            Self::Doctor => "doctor",
            Self::ProbeAccess => "probe-access",
            Self::Log => "log",
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
        other => {
            eprintln!("tycho: {} is not implemented yet", other.name());
            Exit::Failure
        }
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
