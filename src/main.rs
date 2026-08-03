use clap::Parser;
use std::process::ExitCode;
use tycho::cli::{Cli, dispatch};

fn main() -> ExitCode {
    dispatch(Cli::parse().command).into()
}
