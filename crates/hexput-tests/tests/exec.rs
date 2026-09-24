//! Story 2.6: the one Executor entry point (AD-3). Story 3.1: host calls through it — the
//! capability check, the argument rules, and every way a call's reply can end the Script — driven
//! against a stand-in for the connection that answers from the test.

use std::sync::{Arc, Mutex};

use hexput_exec::{ARGUMENT_DEPTH_LIMIT, Diagnostic, Host, Value, execute};
use hexput_port::{CorrelationId, Envelope, MessageType};
use hexput_rpc::{Calls, Value as Wire};

fn program(source: &str) -> Arc<hexput_exec::Program> {
    Arc::new(hexput_parser::parse(source).unwrap())
}

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap()
}

/// Run `source` with no host.
fn run(source: &str, variables: Vec<(Arc<str>, Value)>) -> Result<Value, Diagnostic> {
    runtime().block_on(execute(program(source), variables, Host::none()))
}

#[test]
fn execute_runs_a_script_with_its_starting_variables() {
    let result = run("return a + 1;", vec![(Arc::from("a"), Value::Number(2.0))]).unwrap();
    assert_eq!(result.as_number(), Some(3.0));
}

#[test]
fn execute_with_no_variables_is_plain_evaluation() {
    let result = run("let x = \"hi\"; return x;", vec![]).unwrap();
    assert_eq!(result.as_str(), Some("hi"));
}

#[test]
fn execute_returns_the_runtime_diagnostic() {
    let diagnostic = run("return 1 / 0;", vec![]).unwrap_err();
    assert_eq!(diagnostic.code.as_str(), "arithmetic.division_by_zero");
}

#[test]
fn a_starting_variable_the_script_also_declares_is_a_duplicate_declaration() {
    let diagnostic = run(
        "let a = 5; return a;",
        vec![(Arc::from("a"), Value::Number(1.0))],
    )
    .unwrap_err();
    assert_eq!(diagnostic.code.as_str(), "syntax.duplicate_declaration");
}

// --- Story 3.1: host calls ---

/// How the stand-in Backend answers one call: the reply's type and payload.
type Answer = Box<dyn Fn(&str, &[Wire]) -> (MessageType, Wire) + Send>;

/// One call the stand-in saw: `(name, arguments)`.
type Made = (String, Vec<Wire>);

/// Every call the stand-in saw.
type Seen = Arc<Mutex<Vec<Made>>>;

fn s(text: &str) -> Wire {
    Wire::from(text)
}

fn map(fields: Vec<(&str, Wire)>) -> Wire {
    Wire::Map(fields.into_iter().map(|(k, v)| (s(k), v)).collect())
}

/// A `Result {value}` answer.
fn value(value: Wire) -> (MessageType, Wire) {
    (MessageType::Result, map(vec![("value", value)]))
}

/// Run `source` with `registered` callable, each call answered by `answer`, as the connection
/// would: issue the call, and route the reply envelope back under its id.
fn run_hosted(
    source: &str,
    variables: Vec<(Arc<str>, Value)>,
    registered: &[&str],
    answer: Answer,
) -> (Result<Value, Diagnostic>, Vec<Made>) {
    let seen: Seen = Arc::default();
    let runtime = runtime();
    let (mut calls, caller) = Calls::new();
    let log = Arc::clone(&seen);
    runtime.spawn(async move {
        while let Some(call) = calls.submitted().await {
            let envelope = calls.issue(call);
            let id = envelope.id.unwrap();
            let Wire::Map(fields) = &envelope.payload else {
                panic!("a Call payload is a map");
            };
            assert_eq!(envelope.message_type, MessageType::Call);
            assert_eq!(fields.len(), 2, "exactly `name` and `arguments`");
            assert_eq!(fields[0].0, s("name"));
            assert_eq!(fields[1].0, s("arguments"));
            let name = fields[0].1.as_str().unwrap().to_owned();
            let Wire::Array(arguments) = fields[1].1.clone() else {
                panic!("`arguments` is an array");
            };
            let (message_type, payload) = answer(&name, &arguments);
            log.lock().unwrap().push((name, arguments));
            calls
                .complete(Envelope::new(id, message_type, payload))
                .unwrap();
        }
    });
    let host = Host::new(registered.iter().copied(), caller);
    let result = runtime.block_on(execute(program(source), variables, host));
    let seen = seen.lock().unwrap().clone();
    (result, seen)
}

/// The text a diagnostic's span covers in `source`.
fn spanned<'a>(source: &'a str, diagnostic: &Diagnostic) -> &'a str {
    &source[diagnostic.span.range()]
}

#[test]
fn a_registered_function_is_called_and_the_script_resumes_with_its_value() {
    let (result, seen) = run_hosted(
        "return getOrder(7).total;",
        vec![],
        &["getOrder"],
        Box::new(|_, _| value(map(vec![("total", Wire::from(3))]))),
    );
    assert_eq!(result.unwrap().as_number(), Some(3.0));
    assert_eq!(
        seen,
        [("getOrder".to_owned(), vec![Wire::from(7)])],
        "exactly one call, with its arguments as data"
    );
}

#[test]
fn arguments_travel_as_wire_data_in_order() {
    let (result, seen) = run_hosted(
        "let o = { a: [1, 2.5], b: null }; return echo(o, \"x\", true, -0);",
        vec![],
        &["echo"],
        Box::new(|_, arguments| value(arguments[0].clone())),
    );
    let expected = map(vec![
        ("a", Wire::Array(vec![Wire::from(1), Wire::F64(2.5)])),
        ("b", Wire::Nil),
    ]);
    assert_eq!(
        seen[0].1,
        [expected, s("x"), Wire::Boolean(true), Wire::from(0)]
    );
    let result = result.unwrap();
    let object = result.as_object().unwrap();
    assert_eq!(object.get("a").unwrap().as_array().unwrap().len(), 2);
    assert!(object.get("b").unwrap().is_null());
}

#[test]
fn calls_run_wherever_an_expression_can_and_each_waits_for_its_reply() {
    let source = "
        fn twice(n) { return double(n) + double(n); };
        let total = 0;
        for (i in [1, 2, 3]) { total = total + twice(i); };
        let box = { n: 0 };
        next(box).n = 5;
        return [total, count(), box.n];
    ";
    let counter = Arc::new(Mutex::new(0));
    let (result, seen) = run_hosted(
        source,
        vec![],
        &["double", "next", "count"],
        Box::new(move |name, arguments| match name {
            "double" => value(Wire::from(arguments[0].as_i64().unwrap() * 2)),
            "next" => value(map(vec![("n", Wire::from(1))])),
            _ => {
                let mut counter = counter.lock().unwrap();
                *counter += 1;
                value(Wire::from(*counter))
            }
        }),
    );
    let result = result.unwrap();
    let items: Vec<f64> = result
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_number().unwrap())
        .collect();
    // (2+2) + (4+4) + (6+6); one `count`; the object `next` returned was assigned, not `box`.
    assert_eq!(items, [24.0, 1.0, 0.0]);
    assert_eq!(seen.len(), 8, "six `double`s, one `next`, one `count`");
}

#[test]
fn a_nil_value_is_null() {
    let (result, _) = run_hosted(
        "return nothing();",
        vec![],
        &["nothing"],
        Box::new(|_, _| value(Wire::Nil)),
    );
    assert!(result.unwrap().is_null());
}

#[test]
fn an_unregistered_name_is_a_capability_error_and_nothing_is_sent() {
    let source = "let x = 1;\nreturn nope(x);";
    let (result, seen) = run_hosted(
        source,
        vec![],
        &["getOrder"],
        Box::new(|_, _| value(Wire::Nil)),
    );
    let error = result.unwrap_err();
    assert_eq!(error.category.as_str(), "capability");
    assert_eq!(error.code.as_str(), "capability.unknown_function");
    assert_eq!(spanned(source, &error), "nope(x)");
    assert!(seen.is_empty());
}

#[test]
fn with_no_host_every_host_call_is_a_capability_error() {
    let error = run("return getOrder(1);", vec![]).unwrap_err();
    assert_eq!(error.code.as_str(), "capability.unknown_function");
}

#[test]
fn a_local_binding_shadows_a_registered_name_and_nothing_is_sent() {
    let cases = [
        "fn getOrder(x) { return x; }; return getOrder(1);",
        "let getOrder = fn(x) { return x; }; return getOrder(1);",
        "let f = fn(getOrder) { return getOrder(1); }; return f(fn(x) { return x; });",
    ];
    for source in cases {
        let (result, seen) = run_hosted(
            source,
            vec![],
            &["getOrder"],
            Box::new(|_, _| value(Wire::from(99))),
        );
        assert_eq!(result.unwrap().as_number(), Some(1.0), "{source}");
        assert!(seen.is_empty(), "{source}");
    }
    // A starting variable shadows it too: here it is a number, so calling it is a type error.
    let (result, seen) = run_hosted(
        "return getOrder(1);",
        vec![(Arc::from("getOrder"), Value::Number(5.0))],
        &["getOrder"],
        Box::new(|_, _| value(Wire::from(99))),
    );
    assert_eq!(result.unwrap_err().code.as_str(), "type.not_callable");
    assert!(seen.is_empty());
}

#[test]
fn a_host_function_is_not_a_value() {
    let (result, seen) = run_hosted(
        "let f = getOrder; return f(1);",
        vec![],
        &["getOrder"],
        Box::new(|_, _| value(Wire::Nil)),
    );
    assert_eq!(
        result.unwrap_err().code.as_str(),
        "reference.undeclared_identifier"
    );
    assert!(seen.is_empty());
}

#[test]
fn an_argument_that_is_or_holds_a_function_or_a_cycle_is_refused_and_nothing_is_sent() {
    let cases = [
        (
            "return send(1, fn() { return 1; });",
            "type.function_argument",
            "fn() { return 1; }",
        ),
        (
            "let f = fn() {}; return send([f]);",
            "type.function_argument",
            "[f]",
        ),
        (
            "let a = []; a[0] = a; return send(a);",
            "type.cyclic_argument",
            "a",
        ),
    ];
    for (source, code, argument) in cases {
        let (result, seen) =
            run_hosted(source, vec![], &["send"], Box::new(|_, _| value(Wire::Nil)));
        let error = result.unwrap_err();
        assert_eq!(error.category.as_str(), "type", "{source}");
        assert_eq!(error.code.as_str(), code, "{source}");
        assert_eq!(spanned(source, &error), argument, "{source}");
        assert!(seen.is_empty(), "{source}");
    }
}

/// `levels` arrays nested inside each other, as Hexput source.
fn nested(levels: usize) -> String {
    format!("{}{}", "[".repeat(levels), "]".repeat(levels))
}

#[test]
fn an_argument_at_the_depth_limit_is_sent_and_one_level_deeper_is_refused() {
    assert_eq!(ARGUMENT_DEPTH_LIMIT, 12);
    let at_limit = format!("return send(1, {});", nested(12));
    let (result, seen) = run_hosted(
        &at_limit,
        vec![],
        &["send"],
        Box::new(|_, _| value(Wire::from(1))),
    );
    assert_eq!(result.unwrap().as_number(), Some(1.0));
    assert_eq!(seen.len(), 1);

    let deep = nested(13);
    let too_deep = format!("return send(1, {deep});");
    let (result, seen) = run_hosted(
        &too_deep,
        vec![],
        &["send"],
        Box::new(|_, _| value(Wire::from(1))),
    );
    let error = result.unwrap_err();
    assert_eq!(error.category.as_str(), "depth");
    assert_eq!(error.code.as_str(), "depth.argument_too_deep");
    assert_eq!(spanned(&too_deep, &error), deep);
    assert!(seen.is_empty());
}

#[test]
fn a_backend_error_ends_the_script_with_host_function_failed_on_the_call() {
    let source = "let x = 1;\nreturn getOrder(x) + 1;";
    let (result, seen) = run_hosted(
        source,
        vec![],
        &["getOrder"],
        Box::new(|_, _| {
            (
                MessageType::Error,
                map(vec![("message", s("order store is down"))]),
            )
        }),
    );
    let error = result.unwrap_err();
    assert_eq!(error.category.as_str(), "host");
    assert_eq!(error.code.as_str(), "host.function_failed");
    assert!(
        error.message.contains("order store is down"),
        "{}",
        error.message
    );
    assert_eq!(spanned(source, &error), "getOrder(x)");
    assert_eq!(seen.len(), 1);
}

#[test]
fn a_malformed_reply_is_host_function_failed() {
    let replies: Vec<(MessageType, Wire)> = vec![
        (MessageType::Result, Wire::Nil),
        (MessageType::Result, map(vec![])),
        (
            MessageType::Result,
            map(vec![("value", Wire::from(1)), ("extra", Wire::Nil)]),
        ),
        (
            MessageType::Result,
            map(vec![("value", Wire::Binary(vec![1]))]),
        ),
        (
            MessageType::Result,
            map(vec![("value", Wire::F64(f64::NAN))]),
        ),
        (MessageType::Error, Wire::from(5)),
    ];
    for reply in replies {
        let shown = format!("{reply:?}");
        let (result, _) = run_hosted(
            "return f();",
            vec![],
            &["f"],
            Box::new(move |_, _| reply.clone()),
        );
        let error = result.unwrap_err();
        assert_eq!(error.code.as_str(), "host.function_failed", "{shown}");
        assert_eq!(error.span.offset, 7, "{shown}");
        if shown.contains("Binary") {
            // The path is rooted at the reply's `value`, never at `variables`.
            assert!(error.message.contains("`value`"), "{}", error.message);
            assert!(!error.message.contains("variables"), "{}", error.message);
        }
    }
}

#[test]
fn a_connection_that_ends_before_the_reply_is_host_no_reply() {
    let runtime = runtime();
    let (calls, caller) = Calls::new();
    drop(calls);
    let source = "return f(1);";
    let error = runtime
        .block_on(execute(program(source), vec![], Host::new(["f"], caller)))
        .unwrap_err();
    assert_eq!(error.category.as_str(), "host");
    assert_eq!(error.code.as_str(), "host.no_reply");
    assert_eq!(spanned(source, &error), "f(1)");
}

#[test]
fn a_call_that_cannot_be_framed_fails_and_a_stray_reply_is_handed_back() {
    let runtime = runtime();
    let (mut calls, caller) = Calls::new();
    let task = runtime.spawn(execute(
        program("return f(1);"),
        vec![],
        Host::new(["f"], caller),
    ));
    let error = runtime.block_on(async move {
        let call = calls.submitted().await.unwrap();
        let envelope = calls.issue(call);
        let id = envelope.id.unwrap();
        // A reply naming no pending call comes back untouched.
        let stray = Envelope::new(CorrelationId(id.get() + 1), MessageType::Result, Wire::Nil);
        assert_eq!(calls.complete(stray.clone()), Err(stray));
        assert_eq!(calls.pending(), 1);
        calls.unsendable(id, "too large");
        assert_eq!(calls.pending(), 0);
        task.await.unwrap().unwrap_err()
    });
    assert_eq!(error.code.as_str(), "host.function_failed");
    assert!(
        error.message.contains("could not be sent"),
        "{}",
        error.message
    );
}

#[test]
fn arguments_that_each_fit_a_frame_but_not_together_are_refused_and_nothing_is_sent() {
    // Each string is over half a frame, so each alone fits and the two together cannot.
    let half = "x".repeat(hexput_rpc::MAX_FRAME_LEN / 2 + 1);
    let source = "return send(a, b);";
    let (result, seen) = run_hosted(
        source,
        vec![
            (Arc::from("a"), Value::String(Arc::from(half.as_str()))),
            (Arc::from("b"), Value::String(Arc::from(half.as_str()))),
        ],
        &["send"],
        Box::new(|_, _| value(Wire::Nil)),
    );
    let error = result.unwrap_err();
    assert_eq!(error.code.as_str(), "host.function_failed");
    assert_eq!(spanned(source, &error), "send(a, b)");
    assert!(seen.is_empty());
}
