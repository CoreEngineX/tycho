use std::process::Command as Process;
use tycho::cli::Command;

struct Run {
    code: i32,
    stdout: String,
    stderr: String,
}

fn tycho(args: &[&str]) -> Run {
    let output = Process::new(env!("CARGO_BIN_EXE_tycho"))
        .args(args)
        .output()
        .expect("the test binary was built by cargo and must be runnable");
    Run {
        code: output
            .status
            .code()
            .expect("tycho is never killed by a signal"),
        stdout: String::from_utf8(output.stdout).expect("stdout is utf-8"),
        stderr: String::from_utf8(output.stderr).expect("stderr is utf-8"),
    }
}

#[test]
fn version_is_the_crate_version() {
    let run = tycho(&["--version"]);
    assert_eq!(run.code, 0);
    assert_eq!(
        run.stdout.trim(),
        format!("tycho {}", env!("CARGO_PKG_VERSION"))
    );
}

#[test]
fn help_lists_every_public_command() {
    let run = tycho(&["--help"]);
    assert_eq!(run.code, 0);
    for name in Command::NAMES {
        let listed = run.stdout.contains(&format!("\n  {name} "));
        if name == Command::INTERNAL {
            assert!(!listed, "'{name}' is internal and must stay hidden");
        } else {
            assert!(listed, "'{name}' is missing from --help");
        }
    }
}

#[test]
fn help_is_pure_ascii() {
    let run = tycho(&["--help"]);
    assert!(
        run.stdout.is_ascii(),
        "help output is read through pipes and log files, so it must stay ASCII"
    );
}

#[test]
fn a_hidden_command_still_runs() {
    assert_eq!(tycho(&["probe-access"]).code, 1);
}

#[test]
fn an_unimplemented_command_fails_loudly() {
    let run = tycho(&["status"]);
    assert_eq!(run.code, 1);
    assert!(run.stderr.contains("not implemented"), "{}", run.stderr);
}

#[test]
fn no_subcommand_is_a_usage_error() {
    assert_eq!(tycho(&[]).code, 2);
}

#[test]
fn an_unknown_subcommand_is_a_usage_error() {
    assert_eq!(tycho(&["backup"]).code, 2);
}
