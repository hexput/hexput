//! The `hexput` command line, as `clap` sees it (Story 1.9 decision 1).
//!
//! One parser for every command the CLI grows: Story 1.10's `check` becomes a second
//! [`Command`] variant, and the daemon's own `--config` flag (AD-7) inherits the same style
//! rather than hand-rolling one.

use std::path::PathBuf;

use clap::{Args, ColorChoice, Parser, Subcommand};

#[derive(Debug, Parser)]
#[command(
    name = "hexput",
    version,
    about = "Run Hexput scripts locally — no daemon, socket, or Backend involved.",
    // Decision 4: no colour this story, so no TTY detection and no ANSI anywhere.
    color = ColorChoice::Never
)]
pub(crate) struct Cli {
    #[command(subcommand)]
    pub(crate) command: Command,
}

#[derive(Debug, Subcommand)]
pub(crate) enum Command {
    /// Evaluate a script and print its result.
    Eval(EvalArgs),
}

#[derive(Debug, Args)]
pub(crate) struct EvalArgs {
    /// Path of the script to evaluate.
    #[arg(value_name = "SCRIPT")]
    pub(crate) script: PathBuf,

    /// Bind a starting variable, as `name=<hexput expression>`. The value is ordinary Hexput
    /// source, so every type is reachable: `--var n=5`, `--var s='"hi"'`, `--var xs='[1, 2]'`,
    /// `--var o='{ a: 1 }'`. Repeat the flag for each variable; a name may be supplied once.
    #[arg(long = "var", value_name = "NAME=EXPRESSION")]
    pub(crate) variables: Vec<String>,
}
