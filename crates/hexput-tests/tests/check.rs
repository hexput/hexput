//! Story 1.10: the static check pass. Findings, and — just as load-bearing — silences.
//!
//! The pass promises no false positives: if it reports something, that code really does fail
//! when it is reached. Half the tests here exist to pin the cases where it must stay quiet.

use hexput_check::{Code, Environment, Findings, Outcome, Policy, Severity, check};
use hexput_parser::parse;

/// Environments and policies the sweep below borrows by reference. The environments are
/// `LazyLock` statics rather than consts because `Environment::new` is not a `const fn`.
static STANDARD: std::sync::LazyLock<Environment> = std::sync::LazyLock::new(Environment::new);
static LISTED: std::sync::LazyLock<Environment> =
    std::sync::LazyLock::new(|| Environment::new().with_callables(["warn"]));
const ENABLED: Policy = Policy::new();
const NO_LOOPS: Policy = Policy {
    loops: false,
    ..Policy::new()
};

/// Check `source` with nothing bound and no claim about what is callable — the CLI's default.
fn findings(source: &str) -> Findings {
    let program = parse(source).expect("the source parses");
    check(&program, &Environment::new(), &Policy::new())
}

fn findings_with(source: &str, environment: &Environment) -> Findings {
    let program = parse(source).expect("the source parses");
    check(&program, environment, &Policy::new())
}

fn findings_under(source: &str, policy: &Policy) -> Findings {
    let program = parse(source).expect("the source parses");
    check(&program, &Environment::new(), policy)
}

/// The codes reported, in source order.
fn codes(findings: &Findings) -> Vec<&'static str> {
    findings
        .diagnostics()
        .iter()
        .map(|finding| finding.code.as_str())
        .collect()
}

#[track_caller]
fn reports(source: &str, expected: &[&str]) -> Findings {
    let found = findings(source);
    assert_eq!(codes(&found), expected, "for source:\n{source}");
    found
}

#[track_caller]
fn clean(source: &str) {
    let found = findings(source);
    assert_eq!(
        codes(&found),
        Vec::<&str>::new(),
        "expected silence for:\n{source}"
    );
    assert_eq!(found.outcome(), Outcome::Clean);
}

// --- undeclared names ---

#[test]
fn an_undeclared_read_is_reported() {
    let found = reports("return x;", &["reference.undeclared_identifier"]);
    let finding = &found.diagnostics()[0];
    assert_eq!(finding.severity, Severity::Error);
    assert_eq!(finding.message, "`x` is not declared");
    // Spanned on the name itself, exactly as the interpreter spans the same mistake.
    assert_eq!(finding.span.offset, 7);
    assert_eq!(finding.span.len, 1);
}

#[test]
fn an_undeclared_assignment_is_reported() {
    reports("x = 1;", &["reference.undeclared_assignment"]);
}

#[test]
fn a_binding_declared_later_in_the_block_is_still_undeclared() {
    // `let` does not hoist; only named functions do.
    let found = findings("return x; let x = 1;");
    assert!(codes(&found).contains(&"reference.undeclared_identifier"));
}

#[test]
fn a_hoisted_function_is_callable_before_its_declaration() {
    clean("fn g() { return 1; }; return g();");
    clean("return g(); fn g() { return 1; };");
}

#[test]
fn a_member_assignment_reads_its_base() {
    reports("o.k = 1;", &["reference.undeclared_identifier"]);
    clean("let o = { a: 1 }; o.a = 2; return o;");
}

#[test]
fn scopes_are_block_level_and_an_inner_binding_does_not_escape() {
    reports(
        "{ let inner = 1; return inner; }; return inner;",
        &["reference.undeclared_identifier"],
    );
}

#[test]
fn a_for_binding_and_the_loop_body_share_one_scope() {
    clean("for (item in [1, 2]) { return item; };");
    reports(
        "for (item in [1]) { };  return item;",
        &["reference.undeclared_identifier"],
    );
}

#[test]
fn a_parameter_is_in_scope_in_its_body_and_nowhere_else() {
    clean("fn f(a) { return a; }; return f(1);");
    // `f` is itself never read here, which is a second, separate finding.
    reports(
        "fn f(a) { return 1; }; return a;",
        &[
            "reference.unused_variable",
            "reference.undeclared_identifier",
        ],
    );
}

#[test]
fn a_closure_sees_a_binding_declared_after_it() {
    // Deferred body walking: `f` is hoisted, `x` is declared, and only then is `f` called.
    clean("fn f() { return x; }; let x = 1; return f();");
    clean("let g = fn() { return y; }; let y = 2; return g();");
}

// --- arity ---

#[test]
fn a_wrong_argument_count_against_a_local_function_is_reported() {
    let found = reports(
        "fn f(a) { return a; }; return f(1, 2);",
        &["arity.argument_count"],
    );
    assert_eq!(
        found.diagnostics()[0].message,
        "this call passes 2 arguments, but the function takes 1"
    );
}

#[test]
fn one_argument_is_spelled_in_the_singular() {
    let found = reports(
        "fn f() { return 1; }; return f(1);",
        &["arity.argument_count"],
    );
    assert_eq!(
        found.diagnostics()[0].message,
        "this call passes 1 argument, but the function takes 0"
    );
}

#[test]
fn a_function_valued_let_is_checked_for_arity_too() {
    reports(
        "let f = fn(a) { return a; }; return f();",
        &["arity.argument_count"],
    );
    // A parenthesized function literal is the function it wraps, as it is everywhere else.
    reports(
        "let f = (fn(a) { return a; }); return f();",
        &["arity.argument_count"],
    );
}

#[test]
fn a_named_function_nothing_reads_is_reported_like_any_other_declaration() {
    // `fn helper` and `let helper = fn` are the same mistake and now read the same way.
    for source in [
        "fn helper() { return 1; }; return 2;",
        "let helper = fn() { return 1; }; return 2;",
    ] {
        let found = findings(source);
        assert_eq!(codes(&found), ["reference.unused_variable"], "{source}");
        assert!(!found.has_errors());
    }
    // Calling it is reading it, including from another function's body.
    clean("fn helper() { return 1; }; return helper();");
    clean("fn a() { return b(); }; fn b() { return 1; }; return a();");
}

#[test]
fn a_reassigned_name_is_never_checked_for_arity() {
    // The name no longer certainly holds the function it was declared with. An assignment
    // anywhere counts, including from a nested block or another function's body — the scan
    // reads the whole block arena, not just the top level.
    for source in [
        "fn f(a) { return a; }; f = 1; return f(1, 2);",
        "fn f(a) { return a; }; if (true) { f = 1; }; return f(1, 2);",
        "fn f(a) { return a; }; { f = 1; }; return f(1, 2);",
        "fn f(a) { return a; }; fn g() { f = 1; return 0; }; return f(1, 2) + g();",
        "let f = fn(a) { return a; }; f = fn(b, c) { return b; }; return f(1, 2);",
    ] {
        assert!(
            !codes(&findings(source)).contains(&"arity.argument_count"),
            "an assignment should have ended the arity check for: {source}"
        );
    }
}

#[test]
fn a_call_whose_callee_is_not_a_bare_name_is_not_checked() {
    clean("let o = { f: fn(a) { return a; } }; return o.f(1, 2, 3);");
}

// --- literal operands ---

#[test]
fn a_literal_operand_type_error_is_reported() {
    let found = reports("return \"abc\" * 2;", &["type.operand_mismatch"]);
    let finding = &found.diagnostics()[0];
    assert_eq!(
        finding.message,
        "cannot apply `*` to string and number: the string does not look like a number"
    );
    // The offending operand, not the operator — the interpreter spans it the same way.
    assert_eq!(finding.span.offset, 7);
    assert_eq!(finding.span.len, 5);
}

#[test]
fn a_collection_literal_in_a_string_concatenation_is_reported() {
    let found = reports("return [1] + \"x\";", &["type.operand_mismatch"]);
    assert_eq!(
        found.diagnostics()[0].message,
        "cannot apply `+` to array and string: an array cannot be converted to a string"
    );
}

#[test]
fn unary_negation_of_a_non_numeric_literal_is_reported() {
    let found = reports("return -\"abc\";", &["type.operand_mismatch"]);
    assert_eq!(
        found.diagnostics()[0].message,
        "cannot apply unary `-` to a string: the string does not look like a number"
    );
}

#[test]
fn groups_are_transparent_to_the_literal_rules() {
    reports("return ((\"abc\")) * ((2));", &["type.operand_mismatch"]);
}

#[test]
fn conversions_the_language_performs_are_not_reported() {
    clean("return \"10\" - 1;");
    clean("return \" -3.5e2 \" * 2;");
    clean("return true + 1;");
    clean("return null + 1;");
    clean("return \"Total: \" + 5;");
    clean("return \"a\" + \"b\";");
    clean("return \"a\" < \"b\";");
}

#[test]
fn equality_and_logical_operators_never_report() {
    clean("return [1] == 1;");
    clean("return {} != \"x\";");
    clean("return [1] && \"abc\";");
    clean("return !{};");
}

#[test]
fn nothing_is_inferred_across_a_binding() {
    // This fails at run time, and reporting it would need exactly the inference §10 forbids.
    clean("let s = \"abc\"; return s * 2;");
}

// --- warnings ---

#[test]
fn an_unused_local_is_a_warning_and_never_rejects() {
    let found = reports("let x = 1; return 2;", &["reference.unused_variable"]);
    assert_eq!(found.diagnostics()[0].severity, Severity::Warning);
    assert_eq!(found.outcome(), Outcome::Warnings);
    assert!(!found.has_errors());
}

#[test]
fn a_binding_only_assigned_is_still_unread() {
    reports(
        "let x = 1; x = 2; return 3;",
        &["reference.unused_variable"],
    );
}

#[test]
fn parameters_for_bindings_and_starting_variables_are_never_reported_unused() {
    clean("fn f(unusedParameter) { return 1; }; return f(1);");
    clean("for (unusedItem in [1, 2]) { return 1; };");
    let environment = Environment::new().with_variables(["unusedInput"]);
    assert!(findings_with("return 1;", &environment).is_empty());
}

#[test]
fn unreachable_code_is_a_warning() {
    let found = findings("return 1; let dead = 2;");
    assert_eq!(found.diagnostics()[0].code, Code::UNREACHABLE_CODE);
    assert_eq!(found.diagnostics()[0].severity, Severity::Warning);
    assert!(!found.has_errors());
}

#[test]
fn unreachable_code_after_loop_control_is_reported() {
    let found = findings("while (true) { break; return 1; };");
    assert_eq!(codes(&found), ["syntax.unreachable_code"]);
    let found = findings("while (true) { continue; return 1; };");
    assert_eq!(codes(&found), ["syntax.unreachable_code"]);
}

#[test]
fn a_return_that_ends_its_block_is_not_unreachable_code() {
    clean("fn f() { return 1; }; return f();");
}

#[test]
fn nothing_inside_unreachable_code_is_reported_as_an_error() {
    // Code that can never run can never fail, so an error there would be a claim about code
    // that is certainly not reached. The warning has already said the only true thing about it.
    for source in [
        "return 1; x = 2;",
        "return 1; return nope;",
        "return 1; return \"abc\" * 2;",
        "while (true) { break; return nope; };",
    ] {
        let found = findings(source);
        assert_eq!(codes(&found), ["syntax.unreachable_code"], "{source}");
        assert!(!found.has_errors(), "{source}");
    }
    // A hoisted declaration past the terminator is still live, so its body is still walked.
    assert_eq!(
        codes(&findings("return g(); fn g() { return nope; };")),
        ["reference.undeclared_identifier"]
    );
}

// --- the callable-name list ---

#[test]
fn an_unknown_call_is_silent_when_no_list_is_supplied() {
    clean("log(\"hi\"); return 1;");
}

#[test]
fn an_unknown_call_is_reported_when_a_list_is_supplied() {
    let environment = Environment::new().with_callables(["warn"]);
    let found = findings_with("log(\"hi\"); return 1;", &environment);
    assert_eq!(codes(&found), ["capability.unknown_function"]);
    assert_eq!(found.diagnostics()[0].severity, Severity::Error);
}

#[test]
fn a_listed_name_is_callable_with_any_argument_count() {
    // Arity is the host's business for a Registered Function, and is never guessed here.
    let environment = Environment::new().with_callables(["log"]);
    assert!(findings_with("log(1, 2, 3); return 1;", &environment).is_empty());
}

#[test]
fn an_empty_list_is_not_the_same_as_no_list() {
    let empty = Environment::new().with_callables(Vec::<String>::new());
    assert_eq!(
        codes(&findings_with("log(1); return 1;", &empty)),
        ["capability.unknown_function"],
        "an empty list means nothing but local functions is callable"
    );
    assert!(
        findings("log(1); return 1;").is_empty(),
        "no list at all suppresses the finding entirely"
    );
}

#[test]
fn a_local_function_is_callable_whatever_the_list_says() {
    let environment = Environment::new().with_callables(Vec::<String>::new());
    assert!(findings_with("fn f() { return 1; }; return f();", &environment).is_empty());
}

// --- starting variables ---

#[test]
fn a_starting_variable_is_declared_in_the_root_scope() {
    let environment = Environment::new().with_variables(["n"]);
    assert!(findings_with("return n * 2;", &environment).is_empty());
    assert_eq!(
        codes(&findings("return n * 2;")),
        ["reference.undeclared_identifier"]
    );
}

#[test]
fn a_starting_variable_the_script_also_declares_is_a_duplicate() {
    let environment = Environment::new().with_variables(["n"]);
    for source in ["let n = 1; return n;", "fn n() { return 1; }; return n();"] {
        let found = findings_with(source, &environment);
        assert!(
            codes(&found).contains(&"syntax.duplicate_declaration"),
            "for source: {source}"
        );
    }
}

#[test]
fn a_repeated_starting_variable_name_binds_once_and_reports_nothing() {
    // The interpreter's own entry point documents last-wins for a repeated name and leaves
    // rejecting it to the caller; a finding here would have no source span to point at.
    let environment = Environment::new().with_variables(["n", "n"]);
    assert!(findings_with("return n;", &environment).is_empty());
}

#[test]
fn a_starting_variable_may_be_shadowed_by_an_inner_block() {
    let environment = Environment::new().with_variables(["n"]);
    assert!(findings_with("{ let n = 1; return n; };", &environment).is_empty());
}

// --- policy ---

#[test]
fn every_toggle_defaults_to_enabled() {
    let policy = Policy::new();
    assert_eq!(policy, Policy::default());
    assert!(
        policy.loops
            && policy.conditionals
            && policy.callbacks
            && policy.object_literals
            && policy.array_literals
            && policy.rpc_calls
    );
}

#[test]
fn a_disabled_construct_is_reported_with_its_toggle() {
    let cases = [
        (
            Policy {
                loops: false,
                ..Policy::new()
            },
            "while (true) { break; };",
            "loops",
        ),
        (
            Policy {
                loops: false,
                ..Policy::new()
            },
            "for (i in [1]) { };",
            "loops",
        ),
        (
            Policy {
                conditionals: false,
                ..Policy::new()
            },
            "if (true) { };",
            "conditionals",
        ),
        (
            Policy {
                callbacks: false,
                ..Policy::new()
            },
            "fn f() { return 1; }; return f();",
            "callbacks",
        ),
        (
            Policy {
                object_literals: false,
                ..Policy::new()
            },
            "return { a: 1 };",
            "object_literals",
        ),
        (
            Policy {
                array_literals: false,
                ..Policy::new()
            },
            "return [1];",
            "array_literals",
        ),
        (
            Policy {
                rpc_calls: false,
                ..Policy::new()
            },
            "log(1); return 1;",
            "rpc_calls",
        ),
    ];
    for (policy, source, toggle) in cases {
        let found = findings_under(source, &policy);
        let finding = found
            .diagnostics()
            .iter()
            .find(|finding| finding.code == Code::CONSTRUCT_DISABLED)
            .unwrap_or_else(|| panic!("expected a policy finding for {source}"));
        assert!(
            finding.message.contains(toggle),
            "expected the message to name `{toggle}`, got: {}",
            finding.message
        );
        assert_eq!(finding.category.as_str(), "policy");
        // The finding is an error, and an error is what makes a caller reject the Script — which
        // is the whole of the CLI's exit-1 rule, exercised over every other finding in
        // `tests/cli_core.rs`. The check command has no toggle flag of its own today (Story 3.9
        // owns that surface), so this is where the severity half of that row is pinned.
        assert_eq!(finding.severity, Severity::Error);
        assert!(found.has_errors());
        assert_eq!(found.outcome(), Outcome::Errors);
    }
}

#[test]
fn an_enabled_policy_reports_nothing_by_itself() {
    clean("if (true) { for (i in [1]) { }; }; return { a: [1] };");
}

// --- the pass's own contract ---

#[test]
fn the_pass_is_pure_and_keeps_nothing_between_calls() {
    let source = "let x = 1; return y;";
    let program = parse(source).expect("the source parses");
    let first = check(&program, &Environment::new(), &Policy::new());
    let second = check(&program, &Environment::new(), &Policy::new());
    assert_eq!(first, second);
    assert!(!first.is_empty());
}

#[test]
fn findings_come_out_in_source_order() {
    let found = findings("let a = 1; return z;");
    let offsets: Vec<usize> = found
        .diagnostics()
        .iter()
        .map(|finding| finding.span.offset)
        .collect();
    let mut sorted = offsets.clone();
    sorted.sort_unstable();
    assert_eq!(offsets, sorted);
}

#[test]
fn a_clean_script_reports_clean() {
    let found = findings("let x = 1; return x;");
    assert_eq!(found.outcome(), Outcome::Clean);
    assert_eq!(found.len(), 0);
    assert_eq!(found.error_count(), 0);
}

#[test]
fn warnings_only_is_distinguished_from_clean_and_from_errors() {
    assert_eq!(findings("return 1;").outcome(), Outcome::Clean);
    assert_eq!(
        findings("let unused = 1; return 2;").outcome(),
        Outcome::Warnings
    );
    assert_eq!(findings("return nope;").outcome(), Outcome::Errors);
    assert_eq!(findings("return nope;").error_count(), 1);
}

#[test]
fn adversarial_nesting_does_not_overflow_the_host_stack() {
    // Blocks, expressions and function bodies all nest; each walks on the work stack.
    let depth = 20_000;
    let blocks = format!("{}{}", "{ ".repeat(depth), "} ".repeat(depth));
    assert!(findings(&blocks).is_empty());

    let groups = format!("return {}1{};", "(".repeat(depth), ")".repeat(depth));
    assert!(findings(&groups).is_empty());

    let arrays = format!("return {}1{};", "[".repeat(depth), "]".repeat(depth));
    assert!(findings(&arrays).is_empty());

    let functions = format!(
        "{}return 1;{}",
        "fn f() { ".repeat(depth),
        " };".repeat(depth)
    );
    let found = findings(&functions);
    assert!(!found.has_errors(), "{:?}", found.diagnostics().first());
}

#[test]
fn a_deeply_nested_call_chain_is_walked_without_recursion() {
    let depth = 20_000;
    let source = format!(
        "fn f(a) {{ return a; }}; return {}1{};",
        "f(".repeat(depth),
        ")".repeat(depth)
    );
    assert!(findings(&source).is_empty());
}

// --- the duplicated §4 rules, held to the interpreter's own behaviour ---

/// Every literal-operand case the pass can meet, each paired with the source that exercises it.
/// The assertion is not "the pass is right" but "the pass and the interpreter agree" — which is
/// the only thing that keeps a second implementation of §4.2/§4.3 from drifting.
///
/// The comparison is over `type.operand_mismatch` alone. A pair like `2 / 0` raises
/// `arithmetic.division_by_zero` at run time and the pass deliberately says nothing about it
/// (§10 scopes it to type errors between literal operands), so a runtime error carrying any
/// other code is not a disagreement and is not compared.
#[test]
fn a_literal_operand_finding_matches_what_the_interpreter_does() {
    let operands = [
        "null",
        "true",
        "false",
        "0",
        "2",
        "\"abc\"",
        "\"10\"",
        "\" -3 \"",
        "\"\"",
        // Strings that *start* like a number but are not one: the only cases that reach the
        // trailing-garbage guard at the end of the re-implemented number rule.
        "\"0x10\"",
        "\".5\"",
        "\"1.\"",
        "[1]",
        "[]",
        "{ a: 1 }",
        "{}",
        // A function literal is an operand type of its own, with its own conversion arms.
        "fn(a) { return a; }",
    ];
    let operators = [
        "+", "-", "*", "/", "%", "<", "<=", ">", ">=", "==", "!=", "&&", "||",
    ];
    let mut compared = 0;
    for left in operands {
        for right in operands {
            for operator in operators {
                let source = format!("return {left} {operator} {right};");
                let program = parse(&source).expect("the source parses");
                let found = check(&program, &Environment::new(), &Policy::new());
                let finding = found
                    .diagnostics()
                    .iter()
                    .find(|finding| finding.code == hexput_check::Code::OPERAND_MISMATCH);
                let runtime = hexput_interpreter::evaluate(&program);
                match (finding, &runtime) {
                    (Some(finding), Err(error))
                        if error.code == hexput_check::Code::OPERAND_MISMATCH =>
                    {
                        assert_eq!(finding.code, error.code, "for {source}");
                        assert_eq!(finding.message, error.message, "for {source}");
                        assert_eq!(finding.span, error.span, "for {source}");
                        assert_eq!(finding.category.as_str(), error.category.as_str());
                    }
                    (None, Err(error)) if error.code == hexput_check::Code::OPERAND_MISMATCH => {
                        panic!("the interpreter raised but the pass stayed silent: {source}");
                    }
                    (Some(finding), _) => panic!(
                        "the pass reported but the interpreter did not: {source}\n{}",
                        finding.message
                    ),
                    (None, _) => {}
                }
                compared += 1;
            }
        }
    }
    assert_eq!(compared, operands.len() * operands.len() * operators.len());
}

#[test]
fn a_unary_literal_finding_matches_what_the_interpreter_does() {
    for operand in [
        "null",
        "true",
        "0",
        "\"abc\"",
        "\"10\"",
        "\"0x10\"",
        "[1]",
        "{ a: 1 }",
        "fn() { return 1; }",
    ] {
        for operator in ["-", "!"] {
            let source = format!("return {operator}{operand};");
            let program = parse(&source).expect("the source parses");
            let found = check(&program, &Environment::new(), &Policy::new());
            let finding = found.diagnostics().first();
            match (finding, hexput_interpreter::evaluate(&program)) {
                (Some(finding), Err(error)) => {
                    assert_eq!(finding.code, error.code, "for {source}");
                    assert_eq!(finding.message, error.message, "for {source}");
                    assert_eq!(finding.span, error.span, "for {source}");
                }
                (None, Err(error)) => panic!("the pass stayed silent for {source}: {error}"),
                (Some(finding), Ok(_)) => {
                    panic!(
                        "the pass reported but {source} evaluates: {}",
                        finding.message
                    )
                }
                (None, Ok(_)) => {}
            }
        }
    }
}

/// The pass's central promise, stated as a test: anything it calls clean must not fail for a
/// reason it claims to check.
#[test]
fn a_script_the_pass_calls_clean_evaluates_without_those_failures() {
    let checked = [
        "let x = 1; return x + 1;",
        "fn f(a) { return a * 2; }; return f(21);",
        "let items = [1, 2, 3]; let total = 0; for (item in items) { total = total + item; }; return total;",
        "let o = { a: 1, b: \"x\" }; return o.a + 1;",
        "fn even(n) { if (n == 0) { return true; }; return odd(n - 1); }; fn odd(n) { if (n == 0) { return false; }; return even(n - 1); }; return even(4);",
        "let make = fn(n) { return fn() { return n; }; }; return make(7)();",
    ];
    for source in checked {
        let program = parse(source).expect("the source parses");
        let found = check(&program, &Environment::new(), &Policy::new());
        assert!(
            found.is_empty(),
            "expected no findings for {source}, got {:?}",
            found.diagnostics()
        );
        hexput_interpreter::evaluate(&program)
            .unwrap_or_else(|error| panic!("a clean script failed to evaluate: {source}\n{error}"));
    }
}

// --- the producer sweep, as Story 1.8 established it for the other three crates ---

/// Every code the check pass produces, reached from real source, with the text its span must
/// slice out. `tests/shared.rs` pins the full list of code strings against `Code::ALL`.
#[test]
fn every_finding_code_spans_the_offending_source() {
    let cases: [(&str, &Environment, &Policy, Code, &str); 8] = [
        (
            "return x;",
            &STANDARD,
            &ENABLED,
            Code::UNDECLARED_IDENTIFIER,
            "x",
        ),
        (
            "x = 1;",
            &STANDARD,
            &ENABLED,
            Code::UNDECLARED_ASSIGNMENT,
            "x",
        ),
        (
            "fn f(a) { return a; }; return f(1, 2);",
            &STANDARD,
            &ENABLED,
            Code::ARGUMENT_COUNT,
            "(1, 2)",
        ),
        (
            "return \"abc\" * 2;",
            &STANDARD,
            &ENABLED,
            Code::OPERAND_MISMATCH,
            "\"abc\"",
        ),
        (
            "return 1; let dead = 2;",
            &STANDARD,
            &ENABLED,
            Code::UNREACHABLE_CODE,
            "let dead = 2;",
        ),
        (
            "let x = 1; return 2;",
            &STANDARD,
            &ENABLED,
            Code::UNUSED_VARIABLE,
            "x",
        ),
        (
            "log(1); return 1;",
            &LISTED,
            &ENABLED,
            Code::UNKNOWN_FUNCTION,
            "log(1)",
        ),
        (
            "while (true) { break; };",
            &STANDARD,
            &NO_LOOPS,
            Code::CONSTRUCT_DISABLED,
            "while",
        ),
    ];
    for (source, environment, policy, code, offending) in cases {
        let program = parse(source).expect("the source parses");
        let found = check(&program, environment, policy);
        let finding = found
            .diagnostics()
            .iter()
            .find(|finding| finding.code == code)
            .unwrap_or_else(|| panic!("no `{code}` finding for {source}"));
        assert_eq!(
            &source[finding.span.range()],
            offending,
            "{source}: {finding}"
        );
        assert_eq!(
            hexput_tests::line_and_column(source, finding.span.offset),
            (finding.span.line, finding.span.column),
            "{source}: {finding}"
        );
    }
    for (i, (_, _, _, code, _)) in cases.iter().enumerate() {
        for (_, _, _, other, _) in &cases[i + 1..] {
            assert_ne!(code, other, "the sweep must reach each code once");
        }
    }
    // The pass produces the eight codes above and no others. `Code::DUPLICATE_DECLARATION` is
    // the ninth and is covered by its own test, because it needs a starting variable to reach.
    assert!(!findings("let n = 1; return n;").has_errors());
}

/// Decision 4: two of §10's findings belong to the parser, and a parsed AST can never carry
/// them. Pinned here so the guarantee cannot quietly move out from under the check pass.
#[test]
fn the_parser_rejects_what_the_pass_deliberately_does_not_check() {
    for (source, code) in [
        ("let x = 1; let x = 2;", "syntax.duplicate_declaration"),
        ("fn f(a, a) { return a; };", "syntax.duplicate_declaration"),
        ("break;", "syntax.loop_control_outside_loop"),
        ("continue;", "syntax.loop_control_outside_loop"),
        (
            "fn f() { break; }; while (true) { return f(); };",
            "syntax.loop_control_outside_loop",
        ),
    ] {
        let diagnostic = parse(source).expect_err("the parser rejects this source");
        assert_eq!(diagnostic.code.as_str(), code, "for source: {source}");
    }
}

/// Story 3.1 review: the check and the runtime underline the same range for an unknown call —
/// the whole call, name through closing parenthesis.
#[test]
fn an_unknown_call_is_spanned_like_the_runtime_capability_error() {
    let source = "let x = 1;\nreturn getOrder(x).total;";
    let program = parse(source).expect("the source parses");
    let environment = Environment::new().with_callables(Vec::<String>::new());
    let found = check(&program, &environment, &Policy::new());
    let finding = &found.diagnostics()[0];
    assert_eq!(finding.code, Code::UNKNOWN_FUNCTION);
    assert_eq!(&source[finding.span.range()], "getOrder(x)");
    let runtime = hexput_interpreter::evaluate(&program).unwrap_err();
    assert_eq!(runtime.code, finding.code);
    assert_eq!(runtime.span, finding.span);
}
