//! The `hexput` command line, as `clap` sees it (Story 1.9 decision 1).
//!
//! One parser for every command the CLI grows: the daemon's own `--config` flag (AD-7) inherits
//! the same style rather than hand-rolling one.

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
    /// Report a script's mistakes without running any of it.
    Check(CheckArgs),
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

#[derive(Debug, Args)]
pub(crate) struct CheckArgs {
    /// Path of the script to check.
    #[arg(value_name = "SCRIPT")]
    pub(crate) script: PathBuf,

    /// Declare a starting variable by name, so reading it is not reported as undeclared. A
    /// static check needs the name and never the value, so — unlike `eval` — this flag takes
    /// no `=<expression>` and nothing is evaluated. Repeat the flag for each variable; a name
    /// may be supplied once.
    #[arg(long = "var", value_name = "NAME")]
    pub(crate) variables: Vec<String>,

    /// Declare a name the host makes callable, such as a Registered Function. Supplying this
    /// flag at all turns on reporting calls to names that are neither declared in the script
    /// nor listed here; without it, no call is reported. Repeat the flag for each name.
    #[arg(long = "callable", value_name = "NAME")]
    pub(crate) callables: Vec<String>,
}
