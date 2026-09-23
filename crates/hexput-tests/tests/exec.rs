//! Story 2.6: the one Executor entry point (AD-3).

use std::sync::Arc;

use hexput_exec::{Value, execute};

fn program(source: &str) -> hexput_exec::Program {
    hexput_parser::parse(source).unwrap()
}

#[test]
fn execute_runs_a_script_with_its_starting_variables() {
    let result = execute(
        &program("return a + 1;"),
        vec![(Arc::from("a"), Value::Number(2.0))],
    )
    .unwrap();
    assert_eq!(result.as_number(), Some(3.0));
}

#[test]
fn execute_with_no_variables_is_plain_evaluation() {
    let result = execute(&program("let x = \"hi\"; return x;"), vec![]).unwrap();
    assert_eq!(result.as_str(), Some("hi"));
}

#[test]
fn execute_returns_the_runtime_diagnostic() {
    let diagnostic = execute(&program("return 1 / 0;"), vec![]).unwrap_err();
    assert_eq!(diagnostic.code.as_str(), "arithmetic.division_by_zero");
}

#[test]
fn a_starting_variable_the_script_also_declares_is_a_duplicate_declaration() {
    let diagnostic = execute(
        &program("let a = 5; return a;"),
        vec![(Arc::from("a"), Value::Number(1.0))],
    )
    .unwrap_err();
    assert_eq!(diagnostic.code.as_str(), "syntax.duplicate_declaration");
}
