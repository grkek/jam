//! Build, run, and test automation for the workspace.

use std::process::ExitCode;

const AVAILABLE_COMMANDS: [&str; 4] = ["build", "run", "test", "check"];

fn main() -> ExitCode {
    let requested_command = std::env::args().nth(1);
    match requested_command.as_deref() {
        Some(command) if AVAILABLE_COMMANDS.contains(&command) => {
            eprintln!("`{command}` is not implemented yet.");
            ExitCode::FAILURE
        }
        _ => {
            eprintln!("usage: cargo xtask <{}>", AVAILABLE_COMMANDS.join("|"));
            ExitCode::FAILURE
        }
    }
}
