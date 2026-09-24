//! Story 1.9: the `hexput eval` command — exact stdout, exact stderr, exact exit code.
//!
//! Almost everything here drives `run_with` in-process rather than spawning the binary: the whole
//! point of the three-layer split is that the CLI is a function over two sinks. The one exception
//! runs the real `hexput` process, because `hexput-bin`'s `main` — `args_os()` and the exit code —
//! is the one thing an in-process call cannot check.

use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};
use std::sync::atomic::{AtomicU32, Ordering};

use hexput_cli_core::run_with;

/// A temporary directory, removed when the test ends. No dev-dependency buys this: one directory
/// per call, named for the process and a counter, so parallel tests never collide.
struct Sandbox {
    dir: PathBuf,
}

impl Sandbox {
    fn new() -> Self {
        static NEXT: AtomicU32 = AtomicU32::new(0);
        let dir = std::env::temp_dir().join(format!(
            "hexput-cli-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).expect("a temporary directory");
        Self { dir }
    }

    fn file(&self, name: &str, bytes: impl AsRef<[u8]>) -> PathBuf {
        let path = self.dir.join(name);
        std::fs::write(&path, bytes).expect("a temporary file");
        path
    }
}

impl Drop for Sandbox {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

struct Run {
    code: String,
    stdout: String,
    stderr: String,
}

impl Run {
    /// `ExitCode` exposes no accessor, so its `Debug` form is the comparison — against a real
    /// `ExitCode`, never a hand-written string, so a platform's spelling of it cannot matter.
    fn exit(&self, expected: u8) -> &Self {
        assert_eq!(
            self.code,
            format!("{:?}", ExitCode::from(expected)),
            "expected exit {expected}\nstdout: {}\nstderr: {}",
            self.stdout,
            self.stderr
        );
        self
    }

    fn printed(&self, expected: &str) -> &Self {
        assert_eq!(self.stdout, expected, "stderr: {}", self.stderr);
        assert_eq!(self.stderr, "", "nothing should be reported");
        self
    }

    /// Exactly what a failure must look like: nothing on stdout, and stderr containing `needle`.
    fn reported(&self, needle: &str) -> &Self {
        assert_eq!(self.stdout, "", "a failure must not write to stdout");
        assert!(
            self.stderr.contains(needle),
            "expected stderr to contain {needle:?}, got:\n{}",
            self.stderr
        );
        self
    }
}

fn cli(arguments: &[&str]) -> Run {
    let mut out: Vec<u8> = Vec::new();
    let mut err: Vec<u8> = Vec::new();
    let mut argv = vec!["hexput".to_owned()];
    argv.extend(arguments.iter().map(|a| (*a).to_owned()));
    let code = run_with(argv, &mut out, &mut err);
    Run {
        code: format!("{code:?}"),
        stdout: String::from_utf8(out).expect("stdout is UTF-8"),
        stderr: String::from_utf8(err).expect("stderr is UTF-8"),
    }
}

fn eval_at(path: &Path, variables: &[&str]) -> Run {
    let path = path.to_str().expect("a UTF-8 temporary path");
    let mut arguments = vec!["eval", path];
    for variable in variables {
        arguments.push("--var");
        arguments.push(variable);
    }
    cli(&arguments)
}

/// Evaluate `source` from a file, the way a script author would.
fn eval(source: &str) -> Run {
    let sandbox = Sandbox::new();
    eval_at(&sandbox.file("script.hxp", source), &[])
}

fn eval_with(source: &str, variables: &[&str]) -> Run {
    let sandbox = Sandbox::new();
    eval_at(&sandbox.file("script.hxp", source), variables)
}

// --- a script that works ---

#[test]
fn prints_the_result_and_exits_zero() {
    eval("return 2 + 3;").exit(0).printed("5\n");
}

#[test]
fn a_script_that_runs_off_the_end_yields_null() {
    eval("let x = 1;").exit(0).printed("null\n");
    eval("return;").exit(0).printed("null\n");
}

#[test]
fn an_empty_script_yields_null() {
    eval("").exit(0).printed("null\n");
}

#[test]
fn a_collection_result_prints_as_hexput_source() {
    eval(r#"return { a: [1, "x"] };"#)
        .exit(0)
        .printed("{ a: [1, \"x\"] }\n");
}

#[test]
fn empty_collections_print_without_inner_space() {
    eval("return [];").exit(0).printed("[]\n");
    eval("return {};").exit(0).printed("{}\n");
}

#[test]
fn every_scalar_prints_in_literal_form() {
    eval("return null;").exit(0).printed("null\n");
    eval("return true;").exit(0).printed("true\n");
    eval("return false;").exit(0).printed("false\n");
    eval("return -0;").exit(0).printed("0\n");
    // Through the language's own number formatting, so a number has one spelling everywhere.
    eval("return 1e21;").exit(0).printed("1e+21\n");
    eval("return 0.1 + 0.2;")
        .exit(0)
        .printed("0.30000000000000004\n");
    eval("return 5;").exit(0).printed("5\n");
}

#[test]
fn strings_print_with_their_escapes() {
    eval(r#"return "a\"b\\c\nd\te";"#)
        .exit(0)
        .printed("\"a\\\"b\\\\c\\nd\\te\"\n");
    // Non-ASCII content is verbatim: strings are full Unicode (§2).
    eval(r#"return "merhaba ☕";"#)
        .exit(0)
        .printed("\"merhaba ☕\"\n");
    // A control character with no one-letter escape takes the §3 `\u{…}` form.
    eval(r#"return "a\u{7}b";"#)
        .exit(0)
        .printed("\"a\\u{7}b\"\n");
}

#[test]
fn object_keys_are_bare_only_where_the_language_allows() {
    eval(r#"return { a: 1, "b c": 2, "let": 3, "9": 4, "": 5 };"#)
        .exit(0)
        .printed("{ a: 1, \"b c\": 2, \"let\": 3, \"9\": 4, \"\": 5 }\n");
}

#[test]
fn a_printed_result_parses_back_to_itself() {
    let printed = eval(r#"return { a: [1, "x\n", null, true], "b c": -2.5 };"#)
        .exit(0)
        .stdout
        .trim_end()
        .to_owned();
    eval(&format!("return {printed};"))
        .exit(0)
        .printed(&format!("{printed}\n"));
}

#[test]
fn a_deeply_nested_result_prints_in_full_without_overflowing_the_host_stack() {
    let depth = 20_000;
    let run = eval(&format!(
        "let a = []; let i = 0; while (i < {depth}) {{ a = [a]; i = i + 1; }}; return a;"
    ));
    run.exit(0);
    assert_eq!(run.stderr, "");
    // `depth` wrappings around the innermost `[]`.
    assert_eq!(run.stdout.matches('[').count(), depth + 1);
    assert_eq!(run.stdout.matches(']').count(), depth + 1);
    assert!(run.stdout.ends_with("]\n"));
}

#[test]
fn a_deeply_nested_object_result_prints_in_full_without_overflowing_the_host_stack() {
    // The object path through the printer is a different walk from the array path: it pushes a
    // key and the closing `" }"` as their own steps.
    let depth = 20_000;
    let run = eval(&format!(
        "let a = {{}}; let i = 0; while (i < {depth}) {{ a = {{ inner: a }}; i = i + 1; }}; return a;"
    ));
    run.exit(0);
    assert_eq!(run.stderr, "");
    assert_eq!(run.stdout.matches("inner: ").count(), depth);
    assert!(run.stdout.ends_with("} }\n"));
    // Every level closed, down to the empty object at the bottom.
    assert_eq!(run.stdout.matches(" }").count(), depth);
    assert!(run.stdout.contains("{ inner: {} }"));
}

// --- a script that fails: Story 1.8's rendering, on stderr, with the path as origin ---

#[test]
fn a_syntax_error_renders_against_the_script_path() {
    let sandbox = Sandbox::new();
    let path = sandbox.file("broken.hxp", "let x = 1 +;\n");
    let run = eval_at(&path, &[]);
    run.exit(1).reported("syntax.expected_syntax");
    assert_eq!(
        run.stderr,
        format!(
            "{}:1:12: error syntax[syntax.expected_syntax]: expected an expression, found `;`\n    \
             let x = 1 +;\n               ^\n",
            path.display()
        )
    );
}

#[test]
fn a_lexical_error_renders_the_same_way() {
    eval("return \"unterminated;")
        .exit(1)
        .reported("lex.unterminated_string");
}

#[test]
fn a_runtime_error_is_spanned_on_the_offending_operand() {
    let run = eval(r#"return "abc" * 2;"#);
    run.exit(1).reported("type.operand_mismatch");
    assert!(
        run.stderr.contains("\n           ^^^^^\n"),
        "the marker should sit under `\"abc\"`:\n{}",
        run.stderr
    );
}

#[test]
fn returning_a_function_is_reported_not_printed() {
    eval("fn f() {}; return f;")
        .exit(1)
        .reported("type.function_result");
}

#[test]
fn reading_a_name_nobody_supplied_is_the_interpreters_own_reference_error() {
    eval("return n * 2;")
        .exit(1)
        .reported("reference.undeclared_identifier");
}

#[test]
fn a_host_call_has_no_host_under_eval_and_is_a_capability_error() {
    // Story 3.1: a call to an undeclared name is a host call; `hexput eval` has no host.
    eval("return getOrder(1);")
        .exit(1)
        .reported("capability.unknown_function");
}

// --- starting variables ---

#[test]
fn a_bound_variable_is_what_the_script_reads() {
    eval_with("return n * 2;", &["n=21"])
        .exit(0)
        .printed("42\n");
}

#[test]
fn every_type_is_reachable_as_a_starting_variable() {
    eval_with("return v;", &["v=null"])
        .exit(0)
        .printed("null\n");
    eval_with("return v;", &["v=true"])
        .exit(0)
        .printed("true\n");
    eval_with("return v;", &["v=2.5"]).exit(0).printed("2.5\n");
    eval_with("return v;", &[r#"v="hi""#])
        .exit(0)
        .printed("\"hi\"\n");
    eval_with("return v;", &["v=[1, 2]"])
        .exit(0)
        .printed("[1, 2]\n");
    eval_with("return v;", &["v={ a: 1 }"])
        .exit(0)
        .printed("{ a: 1 }\n");
}

#[test]
fn a_starting_variable_may_be_an_expression_over_literals() {
    eval_with("return v;", &["v=1 + 2 * 3"])
        .exit(0)
        .printed("7\n");
    eval_with("return v;", &["v=-3"]).exit(0).printed("-3\n");
}

#[test]
fn several_variables_bind_independently() {
    eval_with("return a + b;", &["a=1", "b=2"])
        .exit(0)
        .printed("3\n");
}

#[test]
fn a_starting_variable_behaves_like_a_top_level_binding() {
    eval_with("n = n + 1; return n;", &["n=1"])
        .exit(0)
        .printed("2\n");
    eval_with("{ let n = 9; }; return n;", &["n=1"])
        .exit(0)
        .printed("1\n");
}

#[test]
fn a_malformed_variable_is_a_usage_error_and_the_script_is_never_read() {
    // The script path does not exist: reaching it at all would be reported, and is not.
    let run = cli(&["eval", "/no/such/script.hxp", "--var", "n=1 +"]);
    run.exit(2).reported("--var `n`");
    assert!(
        !run.stderr.contains("/no/such/script.hxp"),
        "the script must not be read:\n{}",
        run.stderr
    );
    // The rendering points into the expression the caller actually wrote.
    assert!(run.stderr.contains("return 1 +;"), "{}", run.stderr);
}

#[test]
fn a_variable_whose_expression_fails_at_runtime_is_a_usage_error() {
    cli(&["eval", "/no/such/script.hxp", "--var", r#"n="abc" * 2"#])
        .exit(2)
        .reported("type.operand_mismatch");
}

#[test]
fn a_variable_without_an_equals_sign_is_a_usage_error() {
    cli(&["eval", "/no/such/script.hxp", "--var", "n"])
        .exit(2)
        .reported("name=<expression>");
}

#[test]
fn a_variable_name_must_be_an_identifier() {
    for bad in ["1n=1", "a b=1", "=1", "let=1", "a-b=1", "é=1"] {
        cli(&["eval", "/no/such/script.hxp", "--var", bad])
            .exit(2)
            .reported("is not a variable name");
    }
}

#[test]
fn a_variable_value_must_be_exactly_one_expression() {
    // Binding `1` and silently dropping the rest is the failure this rejects.
    cli(&["eval", "/no/such/script.hxp", "--var", "n=1; let q = 2"])
        .exit(2)
        .reported("exactly one Hexput expression");
    cli(&["eval", "/no/such/script.hxp", "--var", "n=let q = 2"])
        .exit(2)
        .reported("--var `n`");
}

#[test]
fn a_variable_with_no_value_is_a_usage_error_not_null() {
    cli(&["eval", "/no/such/script.hxp", "--var", "n="])
        .exit(2)
        .reported("its value is empty");
    cli(&["eval", "/no/such/script.hxp", "--var", "n=   "])
        .exit(2)
        .reported("its value is empty");
}

#[test]
fn a_variable_the_script_also_declares_is_reported_never_discarded() {
    // §5: a starting variable is a top-level binding, so the script's own `let n` would be a
    // redeclaration in the same block — and silently keeping one of the two values is decision 5's
    // "debugged for an hour" failure.
    eval_with("let n = 3; return n;", &["n=9"])
        .exit(1)
        .reported("syntax.duplicate_declaration");
}

#[test]
fn a_repeated_variable_name_is_a_usage_error_never_last_wins() {
    let run = eval_with("return n;", &["n=1", "n=2"]);
    run.exit(2).reported("supplied more than once");
    assert!(run.stderr.contains('n'), "{}", run.stderr);
}

// --- the file itself ---

#[test]
fn a_missing_file_names_the_path_and_the_reason() {
    cli(&["eval", "/no/such/script.hxp"])
        .exit(1)
        .reported("/no/such/script.hxp");
}

#[test]
fn a_directory_is_reported_rather_than_read() {
    let sandbox = Sandbox::new();
    let run = eval_at(&sandbox.dir, &[]);
    run.exit(1).reported("cannot read");
}

#[test]
fn non_utf8_bytes_are_reported_never_lossily_converted() {
    let sandbox = Sandbox::new();
    let path = sandbox.file("bytes.hxp", b"return \xff;");
    let run = eval_at(&path, &[]);
    run.exit(1).reported("not valid UTF-8");
    assert!(run.stderr.contains(&path.display().to_string()));
    assert!(
        !run.stderr.contains('\u{fffd}'),
        "no replacement character may appear:\n{}",
        run.stderr
    );
}

// --- usage ---

#[test]
fn no_arguments_is_a_usage_error() {
    cli(&[]).exit(2).reported("Usage:");
}

#[test]
fn an_unknown_flag_is_a_usage_error() {
    let sandbox = Sandbox::new();
    let path = sandbox.file("script.hxp", "return 1;");
    let path = path.to_str().expect("a UTF-8 temporary path");
    cli(&["eval", path, "--nope"]).exit(2).reported("--nope");
}

#[test]
fn a_missing_script_operand_is_a_usage_error() {
    cli(&["eval"]).exit(2).reported("Usage:");
}

#[test]
fn an_unknown_subcommand_is_a_usage_error() {
    cli(&["nope"]).exit(2).reported("Usage:");
}

#[test]
fn asking_for_help_or_the_version_writes_to_stdout_and_succeeds() {
    // Explicitly asked for, so it is neither a failure nor an evaluation's result: it goes where
    // `hexput --help | less` can find it, and asking is not a failure.
    let run = cli(&["--help"]);
    run.exit(0);
    assert!(run.stdout.contains("Usage:"), "{}", run.stdout);
    assert_eq!(run.stderr, "");

    let run = cli(&["--version"]);
    run.exit(0);
    assert!(run.stdout.contains("hexput"), "{}", run.stdout);
    assert_eq!(run.stderr, "");
}

// --- the binary itself ---

/// The `hexput` binary `hexput-bin` produces, beside this test executable in the target
/// directory.
///
/// `env!("CARGO_BIN_EXE_hexput")` would be the direct way to name it, but Cargo defines that
/// variable only for tests **of the package declaring the binary** — and `hexput-bin` produces no
/// lib, so it cannot even be a dev-dependency here (Cargo ignores such a dependency outright).
/// The workspace build that precedes the test run produces the binary; if it is missing, say so
/// rather than failing as though the CLI were broken.
fn hexput_binary() -> PathBuf {
    let path = std::env::current_exe()
        .expect("this test executable's own path")
        // …/target/<profile>/deps/cli_core-<hash> → …/target/<profile>
        .parent()
        .and_then(Path::parent)
        .expect("a target directory above this test executable")
        .join(format!("hexput{}", std::env::consts::EXE_SUFFIX));
    assert!(
        path.is_file(),
        "`{}` is not built; run the workspace build first (`cargo build --workspace`)",
        path.display()
    );
    path
}

/// Everything above drives `run_with`, which cannot catch a mistake in how `main` hands the
/// process arguments over or returns the exit code. This one runs the real binary.
#[test]
fn the_binary_passes_its_arguments_through_and_returns_the_exit_code() {
    let sandbox = Sandbox::new();
    let binary = hexput_binary();
    let good = sandbox.file("good.hxp", "return n * 2;");
    let output = Command::new(&binary)
        .args([
            "eval",
            good.to_str().expect("a UTF-8 path"),
            "--var",
            "n=21",
        ])
        .output()
        .expect("the `hexput` binary runs");
    assert_eq!(output.status.code(), Some(0));
    assert_eq!(String::from_utf8_lossy(&output.stdout), "42\n");
    assert_eq!(String::from_utf8_lossy(&output.stderr), "");

    let broken = sandbox.file("broken.hxp", "let x = 1 +;\n");
    let output = Command::new(&binary)
        .args(["eval", broken.to_str().expect("a UTF-8 path")])
        .output()
        .expect("the `hexput` binary runs");
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(String::from_utf8_lossy(&output.stdout), "");
    assert_eq!(
        String::from_utf8_lossy(&output.stderr),
        format!(
            "{}:1:12: error syntax[syntax.expected_syntax]: expected an expression, found `;`\n    \
             let x = 1 +;\n               ^\n",
            broken.display()
        )
    );
}

// --- sinks that fail ---

/// A closed pipe: every write fails. `hexput eval script.hxp | head -1` is exactly this.
struct BrokenPipe;

impl std::io::Write for BrokenPipe {
    fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
        Err(std::io::Error::from(std::io::ErrorKind::BrokenPipe))
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Err(std::io::Error::from(std::io::ErrorKind::BrokenPipe))
    }
}

#[test]
fn a_sink_that_fails_mid_write_changes_nothing_but_the_output() {
    let sandbox = Sandbox::new();
    let run = |path: &Path| {
        let argv = [
            "hexput".to_owned(),
            "eval".to_owned(),
            path.to_str().expect("a UTF-8 path").to_owned(),
        ];
        format!("{:?}", run_with(argv, &mut BrokenPipe, &mut BrokenPipe))
    };
    // The work either succeeded or it did not; whether anyone could read about it is not part of
    // that, and neither failure may panic.
    assert_eq!(
        run(&sandbox.file("good.hxp", "return 1;")),
        format!("{:?}", ExitCode::from(0))
    );
    assert_eq!(
        run(&sandbox.file("broken.hxp", "let x = 1 +;")),
        format!("{:?}", ExitCode::from(1))
    );
}

// --- Story 1.10: the `hexput check` command ---

fn check_at(path: &Path, flags: &[&str]) -> Run {
    let path = path.to_str().expect("a UTF-8 temporary path");
    let mut arguments = vec!["check", path];
    arguments.extend_from_slice(flags);
    cli(&arguments)
}

/// Check `source` from a file, and report what the command wrote where.
fn check_with(source: &str, flags: &[&str]) -> (Run, String) {
    let sandbox = Sandbox::new();
    let path = sandbox.file("script.hxp", source);
    let run = check_at(&path, flags);
    let label = path.to_str().expect("a UTF-8 temporary path").to_owned();
    (run, label)
}

fn check(source: &str) -> (Run, String) {
    check_with(source, &[])
}

/// The summary line the command writes to stdout, whatever else it reported.
#[track_caller]
fn summarized(run: &Run, label: &str, expected: &str) {
    assert_eq!(run.stdout, format!("{label}: {expected}\n"));
}

#[test]
fn a_clean_script_checks_clean_and_exits_zero() {
    let (run, label) = check("let x = 1; return x;");
    run.exit(0);
    summarized(&run, &label, "no findings");
    assert_eq!(run.stderr, "", "a clean script reports nothing");
}

#[test]
fn an_undeclared_read_is_reported_and_exits_one() {
    let (run, label) = check("return x;");
    run.exit(1);
    summarized(&run, &label, "1 finding, 1 error");
    assert!(run.stderr.contains("reference.undeclared_identifier"));
    assert!(run.stderr.contains(&format!("{label}:1:8:")));
}

#[test]
fn an_undeclared_assignment_is_reported_and_exits_one() {
    let (run, _) = check("x = 1;");
    run.exit(1);
    assert!(run.stderr.contains("reference.undeclared_assignment"));
}

#[test]
fn a_binding_used_before_its_let_is_reported() {
    let (run, _) = check("return x; let x = 1;");
    run.exit(1);
    assert!(run.stderr.contains("reference.undeclared_identifier"));
}

#[test]
fn a_wrong_argument_count_is_reported_and_exits_one() {
    let (run, _) = check("fn f(a) { return a; }; return f(1, 2);");
    run.exit(1);
    assert!(run.stderr.contains("arity.argument_count"));
}

#[test]
fn a_reassigned_function_name_is_not_reported_for_arity() {
    let (run, label) = check("fn f(a) { return a; }; f = 1; return f(1, 2);");
    run.exit(0);
    summarized(&run, &label, "no findings");
}

#[test]
fn a_literal_operand_error_is_reported_and_exits_one() {
    let (run, _) = check("return \"abc\" * 2;");
    run.exit(1);
    assert!(run.stderr.contains("type.operand_mismatch"));

    let (run, _) = check("return [1] + \"x\";");
    run.exit(1);
    assert!(run.stderr.contains("type.operand_mismatch"));
}

#[test]
fn an_operand_reached_through_a_binding_is_not_reported() {
    let (run, label) = check("let s = \"abc\"; return s * 2;");
    run.exit(0);
    summarized(&run, &label, "no findings");
}

#[test]
fn warnings_alone_never_reject_a_script() {
    let (run, label) = check("let x = 1; return 2;");
    run.exit(0);
    summarized(&run, &label, "1 finding, 0 errors");
    assert!(run.stderr.contains("reference.unused_variable"));
    assert!(run.stderr.contains("warning"));

    // One warning, not two: the dead statement is not walked at all, so `dead` is never even
    // declared — saying it is also unused would be a second claim about code that cannot run.
    let (run, label) = check("return 1; let dead = 2;");
    run.exit(0);
    summarized(&run, &label, "1 finding, 0 errors");
    assert!(run.stderr.contains("syntax.unreachable_code"));
}

#[test]
fn an_unknown_call_needs_the_callable_flag_to_be_reported() {
    let (run, label) = check("log(\"hi\"); return 1;");
    run.exit(0);
    summarized(&run, &label, "no findings");

    let (run, _) = check_with("log(\"hi\"); return 1;", &["--callable", "warn"]);
    run.exit(1);
    assert!(run.stderr.contains("capability.unknown_function"));

    let (run, label) = check_with("log(\"hi\"); return 1;", &["--callable", "log"]);
    run.exit(0);
    summarized(&run, &label, "no findings");
}

#[test]
fn a_starting_variable_is_declared_by_name_alone() {
    let (run, label) = check_with("return n * 2;", &["--var", "n"]);
    run.exit(0);
    summarized(&run, &label, "no findings");

    let (run, _) = check("return n * 2;");
    run.exit(1);
    assert!(run.stderr.contains("reference.undeclared_identifier"));
}

#[test]
fn a_check_var_carrying_a_value_is_a_usage_error() {
    // The eval command's `name=<expression>` spelling is not accepted here: a static check needs
    // the name and never the value, and evaluating one would be execution.
    let (run, _) = check_with("return n;", &["--var", "n=5"]);
    run.exit(2).reported("--var `n=5`");

    let (run, _) = check_with("return n;", &["--var", "1n"]);
    run.exit(2).reported("--var `1n`");

    let (run, _) = check_with("return n;", &["--var", "let"]);
    run.exit(2).reported("--var `let`");
}

#[test]
fn a_repeated_name_is_a_usage_error_on_either_flag() {
    let (run, _) = check_with("return n;", &["--var", "n", "--var", "n"]);
    run.exit(2).reported("supplied more than once");

    let (run, _) = check_with(
        "log(1); return 1;",
        &["--callable", "log", "--callable", "log"],
    );
    run.exit(2).reported("supplied more than once");

    let (run, _) = check_with("log(1); return 1;", &["--callable", "1log"]);
    run.exit(2).reported("--callable `1log`");
}

#[test]
fn an_unparseable_script_is_reported_and_never_checked() {
    let (run, label) = check("let x = 1 +;");
    run.exit(1);
    assert_eq!(
        run.stdout, "",
        "nothing was checked, so there is no summary"
    );
    assert!(run.stderr.contains("syntax.expected_syntax"));
    assert!(run.stderr.contains(&label));
}

#[test]
fn the_check_command_shares_evals_failures_for_the_file_itself() {
    let sandbox = Sandbox::new();
    let missing = sandbox.dir.join("nope.hxp");
    check_at(&missing, &[]).exit(1).reported("cannot read");
    check_at(&sandbox.dir, &[]).exit(1).reported("cannot read");

    let invalid = sandbox.file("bad.hxp", [0x66, 0x6e, 0xff, 0x28]);
    check_at(&invalid, &[])
        .exit(1)
        .reported("is not valid UTF-8");
}

#[test]
fn an_unknown_flag_on_check_is_a_usage_error() {
    let sandbox = Sandbox::new();
    let path = sandbox.file("script.hxp", "return 1;");
    check_at(&path, &["--nope"]).exit(2);
    cli(&["check"]).exit(2);
}

#[test]
fn an_adversarially_nested_script_is_checked_without_overflowing() {
    let depth = 20_000;
    let source = format!("return {}1{};", "[".repeat(depth), "]".repeat(depth));
    let (run, label) = check(&source);
    run.exit(0);
    summarized(&run, &label, "no findings");
}

#[test]
fn checking_a_script_never_runs_any_of_it() {
    // A script whose *result* would fail at run time checks clean: nothing about it is executed,
    // so nothing about it can fail here.
    let (run, label) = check("let a = []; a[0] = a; return a;");
    run.exit(0);
    summarized(&run, &label, "no findings");
    // And the same source really does fail under eval, so the silence above is about not running
    // it rather than about the mistake not existing.
    let sandbox = Sandbox::new();
    eval_at(
        &sandbox.file("script.hxp", "let a = []; a[0] = a; return a;"),
        &[],
    )
    .exit(1)
    .reported("type.cyclic_result");
}
