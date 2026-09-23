//! Thin OS entry point for the daemon. The process arguments pass through untouched and the exit
//! code comes straight back: argument parsing (`--config`), System Config resolution (AD-7), all
//! output and every exit code live in `hexput-daemon`. No logic beyond this hand-off belongs in
//! this file or this crate; see AGENTS.md's Structural Seed for `hexput-bin`'s role.

use std::process::ExitCode;

fn main() -> ExitCode {
    hexput_daemon::run(std::env::args_os())
}
