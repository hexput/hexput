//! Eval + check command logic (Stories 1.9-1.10) for the `hexput` CLI binary.
//! Depends on hexput-lexer, hexput-parser, hexput-interpreter, hexput-check — and, of
//! non-workspace crates, `clap` alone. Diagnostics are reached through `hexput-parser`'s and
//! `hexput-interpreter`'s re-exports; a direct `hexput-shared` or `hexput-ast` edge is not in the
//! Spine's crate graph and `scripts/check-crate-graph.py` asserts the four edges exactly.
//! Binds no Architecture Decision directly.
//!
//! # Shape
//!
//! Only `hexput-bin` owns a `fn main()` or ends the process: [`run`] and [`run_with`] return a
//! [`ExitCode`] and write through sinks, so the whole CLI is exercised in-process by a test that
//! asserts exact stdout, stderr and exit code without spawning a binary.
//!
//! # Contract (Story 1.9 decision 5)
//!
//! * `0` — the Script evaluated; its result is on stdout in Hexput literal form, one line.
//! * `2` — the invocation was wrong: an unknown flag, a missing operand, a malformed, repeated or
//!   non-evaluating `--var`. Nothing is read or evaluated.
//! * `1` — the Hexput was wrong, or its file could not be read: a lexical, syntax or runtime
//!   diagnostic (rendered by Story 1.8's renderer with the script path as origin), a file that
//!   does not exist or is not a file, or bytes that are not UTF-8.
//!
//! Every failure writes to stderr, so the result can be piped; stdout carries a successful
//! evaluation's result, and the help or version text when the user explicitly asks for it (exit
//! `0` — asking is not a failure, and the answer is what they wanted to read or pipe). There is
//! no colour, no TTY detection and no ANSI anywhere (decision 4).

mod args;
mod print;

use std::ffi::OsString;
use std::io::Write;
use std::path::Path;
use std::process::ExitCode;

use clap::Parser as _;
use hexput_interpreter::{Value, evaluate, evaluate_with_variables};
use hexput_lexer::{TokenKind, tokenize};
use hexput_parser::{Diagnostic, RenderOptions, StatementKind, parse, render_diagnostic};

use args::{Cli, Command, EvalArgs};

/// The invocation was wrong (a bad flag, a missing operand, a malformed `--var`), as distinct
/// from the Hexput being wrong — so a calling script can tell the two apart.
const USAGE: u8 = 2;
/// The Hexput was wrong, or its source could not be read.
const FAILURE: u8 = 1;

/// Run the CLI over `args` (including the program name, as `std::env::args_os` yields it),
/// writing to the process's own stdout and stderr.
///
/// Never panics and never ends the process: the caller — `hexput-bin`'s `main`, the one place a
/// `fn main()` lives — returns the [`ExitCode`].
pub fn run<I, T>(args: I) -> ExitCode
where
    I: IntoIterator<Item = T>,
    T: Into<OsString> + Clone,
{
    let stdout = std::io::stdout();
    let stderr = std::io::stderr();
    run_with(args, &mut stdout.lock(), &mut stderr.lock())
}

/// [`run`] against caller-supplied sinks, so the output contract is testable in-process.
///
/// A sink that fails mid-write (a closed pipe) does not change the exit code: the work either
/// succeeded or it did not, and that is what the code reports.
pub fn run_with<I, T>(args: I, out: &mut dyn Write, err: &mut dyn Write) -> ExitCode
where
    I: IntoIterator<Item = T>,
    T: Into<OsString> + Clone,
{
    let cli = match Cli::try_parse_from(args) {
        Ok(cli) => cli,
        Err(error) => {
            // `use_stderr` is false only when the user explicitly asked for help or the version.
            // That is neither a failure nor an evaluation's result, but it is what they asked
            // for, so it goes to stdout where `hexput --help | less` can find it.
            return if error.use_stderr() {
                let _ = write!(err, "{}", error.render());
                ExitCode::from(USAGE)
            } else {
                let _ = write!(out, "{}", error.render());
                ExitCode::SUCCESS
            };
        }
    };
    match &cli.command {
        Command::Eval(arguments) => eval(arguments, out, err),
    }
}

/// The eval command: bind the starting variables, read the file, parse, evaluate, print.
///
/// The variables come first on purpose — a malformed `--var` is a usage error, reported before
/// the script is so much as opened.
fn eval(arguments: &EvalArgs, out: &mut dyn Write, err: &mut dyn Write) -> ExitCode {
    let variables = match starting_variables(&arguments.variables) {
        Ok(variables) => variables,
        Err(report) => {
            let _ = writeln!(err, "{report}");
            return ExitCode::from(USAGE);
        }
    };

    let path = arguments.script.display().to_string();
    let source = match read_script(&arguments.script) {
        Ok(source) => source,
        Err(report) => {
            let _ = writeln!(err, "{report}");
            return ExitCode::from(FAILURE);
        }
    };

    // One rendering for every lexical, syntax and runtime failure, with the script path as the
    // origin label. Story 1.8's output is used exactly as produced — never post-processed.
    let render = |diagnostic: &Diagnostic| {
        render_diagnostic(diagnostic, &source, RenderOptions::new().with_origin(&path))
    };

    let program = match parse(&source) {
        Ok(program) => program,
        Err(diagnostic) => {
            let _ = writeln!(err, "{}", render(&diagnostic));
            return ExitCode::from(FAILURE);
        }
    };
    let result = match evaluate_with_variables(&program, variables) {
        Ok(result) => result,
        Err(diagnostic) => {
            let _ = writeln!(err, "{}", render(&diagnostic));
            return ExitCode::from(FAILURE);
        }
    };

    let _ = writeln!(out, "{}", print::literal(&result));
    ExitCode::SUCCESS
}

/// Read a script file as UTF-8 source. Both failures name the path: a caller with several of them
/// on one line needs to know which one this was.
fn read_script(path: &Path) -> Result<String, String> {
    let bytes = std::fs::read(path)
        .map_err(|error| format!("hexput: cannot read `{}`: {error}", path.display()))?;
    // Never lossy: a replacement character would change the source the spans point into, and the
    // author would be shown a line they did not write.
    String::from_utf8(bytes).map_err(|error| {
        format!(
            "hexput: cannot read `{}`: the file is not valid UTF-8 (byte {} begins an invalid sequence)",
            path.display(),
            error.utf8_error().valid_up_to()
        )
    })
}

/// Turn each `--var name=<expression>` into a name and a [`Value`], in the order given.
///
/// Every failure here is a usage error: the invocation was wrong, so nothing is read or run.
fn starting_variables(arguments: &[String]) -> Result<Vec<(String, Value)>, String> {
    let mut bound: Vec<(String, Value)> = Vec::new();
    for argument in arguments {
        let Some((name, expression)) = argument.split_once('=') else {
            return Err(format!(
                "hexput: --var `{argument}` is not `name=<expression>`: the variable name and its \
                 Hexput expression are separated by `=`"
            ));
        };
        if !is_identifier(name) {
            return Err(format!(
                "hexput: --var `{argument}`: `{name}` is not a variable name — a name is ASCII \
                 letters, digits and `_`, does not begin with a digit, and is not a reserved word"
            ));
        }
        // Decision 5: never last-wins. Silently dropping one of two supplied values is the kind
        // of thing a person debugs for an hour.
        if bound.iter().any(|(existing, _)| existing == name) {
            return Err(format!(
                "hexput: --var `{name}` was supplied more than once; give each starting variable \
                 exactly once"
            ));
        }
        let value = evaluate_expression(expression)
            .map_err(|report| format!("hexput: --var `{name}`: {report}"))?;
        bound.push((name.to_owned(), value));
    }
    Ok(bound)
}

/// Evaluate one `--var` value (decision 2).
///
/// The value is ordinary Hexput source, run as `return <expression>;` through the same parser and
/// evaluator the script itself goes through — so all six types are reachable, the quoting rules
/// are the language's own, and nothing here can drift from §3. It sees no starting variables of
/// its own: an input is a value, not a computation over the other inputs.
///
/// Exactly **one** expression: the synthetic `return` must be the whole program and must carry a
/// value. Anything else — an empty value, or a statement list that would let `n=1; let q = 2`
/// bind `1` and silently drop the rest — is rejected rather than half-honoured.
fn evaluate_expression(expression: &str) -> Result<Value, String> {
    let source = format!("return {expression};");
    let render = |diagnostic: &Diagnostic| {
        format!(
            "its value is not a Hexput expression that evaluates\n{}",
            render_diagnostic(
                diagnostic,
                &source,
                RenderOptions::new().with_origin("--var"),
            )
        )
    };
    let program = parse(&source).map_err(|diagnostic| render(&diagnostic))?;
    let one_expression = match program.statements.as_slice() {
        [only] => matches!(only.kind, StatementKind::Return { value: Some(_), .. }),
        _ => false,
    };
    if !one_expression {
        return Err(if expression.trim().is_empty() {
            "its value is empty; supply a Hexput expression, such as `5`, `\"hi\"`, `[1, 2]` or \
             `{ a: 1 }`"
                .to_owned()
        } else {
            "its value must be exactly one Hexput expression, not a statement or a list of them"
                .to_owned()
        });
    }
    evaluate(&program).map_err(|diagnostic| render(&diagnostic))
}

/// Whether `text` is a §2 identifier: the lexer's own judgement, so the reserved-word list lives
/// in exactly one place. `text` must lex to a single `Ident` covering all of it — which rejects
/// the empty string, a reserved word, anything with a space, and `a//b`, whose comment would
/// otherwise leave one `Ident` behind.
pub(crate) fn is_identifier(text: &str) -> bool {
    match tokenize(text).as_deref() {
        Ok([token]) => {
            matches!(token.kind, TokenKind::Ident(_))
                && token.span.offset == 0
                && token.span.len == text.len()
        }
        _ => false,
    }
}
