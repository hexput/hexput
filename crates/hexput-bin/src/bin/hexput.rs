//! Thin OS entry point for the `hexput` CLI (eval + check). The process arguments pass through
//! untouched and the exit code comes straight back: argument parsing, all output and every exit
//! code live in `hexput-cli-core`. No logic beyond this hand-off belongs in this file or this
//! crate; see AGENTS.md's Structural Seed for `hexput-bin`'s role.

use std::process::ExitCode;

fn main() -> ExitCode {
    hexput_cli_core::run(std::env::args_os())
}
