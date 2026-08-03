//! Layer 5. The clap surface, the exit-code contract, and rendering.

use clap::{Parser, Subcommand};
use std::process::ExitCode;

#[derive(Debug, Parser)]
#[command(name = "tycho", version, about)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Clone, Copy, Debug, Subcommand)]
pub enum Command {
    /// Capture, commit and push
    Run,
    /// Push what the store already holds to any remote that is behind
    Push,
    /// Per-profile schedule, store summary and remote health
    Status,
    /// The store's commits, rendered
    History,
    /// Recover to a destination directory
    Restore,
    /// Manage watched roots
    Watch,
    /// Manage ignore rules
    Ignore,
    /// Manage re-include rules
    Reinclude,
    /// Validate, locate or create the config file
    Config,
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
    pub const ALL: [Self; 13] = [
        Self::Run,
        Self::Push,
        Self::Status,
        Self::History,
        Self::Restore,
        Self::Watch,
        Self::Ignore,
        Self::Reinclude,
        Self::Config,
        Self::Service,
        Self::Doctor,
        Self::ProbeAccess,
        Self::Log,
    ];

    /// The subcommand as it is typed, which must stay identical to clap's own
    /// kebab-case rename of the variant.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Run => "run",
            Self::Push => "push",
            Self::Status => "status",
            Self::History => "history",
            Self::Restore => "restore",
            Self::Watch => "watch",
            Self::Ignore => "ignore",
            Self::Reinclude => "reinclude",
            Self::Config => "config",
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
    let name = command.name();
    eprintln!("tycho: {name} is not implemented yet");
    Exit::Failure
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

    #[test]
    fn every_name_is_the_name_clap_accepts() {
        for command in Command::ALL {
            let name = command.name();
            let parsed = Cli::try_parse_from(["tycho", name])
                .unwrap_or_else(|e| panic!("'{name}' is not a subcommand clap knows: {e}"));
            assert_eq!(parsed.command.name(), name);
        }
    }
}
