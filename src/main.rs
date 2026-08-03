use clap::Parser;
use std::process::ExitCode;
use tycho::cli::{Cli, dispatch};

fn main() -> ExitCode {
    let cli = Cli::parse();
    // Decided once, before anything renders, so no command has to thread it through.
    tycho::cli::render::decide_colour(cli.no_color);
    dispatch(cli.command).into()
}
