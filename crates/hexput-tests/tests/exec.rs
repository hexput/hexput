//! Story 2.6: the one Executor entry point (AD-3). Story 3.1: host calls through it — the
//! capability check, the argument rules, and every way a call's reply can end the Script — driven
//! against a stand-in for the connection that answers from the test. Story 3.2: a blanket grant
//! lets a call go ahead at once; every registration here states its grant. Story 3.3: a call
//! without one is put to the Backend's per-call handler first, and only `true` lets it proceed.

use std::sync::{Arc, Mutex};

use hexput_exec::{
    ARGUMENT_DEPTH_LIMIT, Diagnostic, Host, Limits, Value, execute, execute_with_limits,
};
use hexput_port::{CorrelationId, Envelope, MessageType};
use hexput_rpc::{Calls, Value as Wire};

fn program(source: &str) -> Arc<hexput_exec::Program> {
    Arc::new(hexput_parser::parse(source).unwrap())
}

fn runtime() -> tokio::runtime::Runtime {
    // Timers: a question for a per-call handler is waited for under a timeout (Story 3.3).
    tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
        .unwrap()
}

/// Run `source` with no host, under a discarding subscriber (see [`run_logged`] for why).
fn run(source: &str, variables: Vec<(Arc<str>, Value)>) -> Result<Value, Diagnostic> {
    let dispatch = tracing::Dispatch::new(
        tracing_subscriber::fmt()
            .with_max_level(tracing::Level::TRACE)
            .with_writer(std::io::sink)
            .finish(),
    );
    tracing::dispatcher::with_default(&dispatch, || {
        runtime().block_on(execute(program(source), variables, Host::none()))
    })
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

/// How the stand-in Backend's per-call handler answers one `Authorize` question.
type Handler = Box<dyn Fn(&str, &[Wire]) -> Reply + Send>;

/// What the stand-in does with one `Authorize` question.
enum Reply {
    /// Answer with this type and payload.
    Answer(MessageType, Wire),
    /// Never answer: the question stays pending.
    Silent,
    /// The connection ends: every pending question and call fails with no reply.
    Hangup,
    /// The question could not be framed: the connection fails it as unsendable.
    Unframable,
}

/// A handler answering every question `Result {value: <answer>}`.
fn answering(answer: Wire) -> Handler {
    Box::new(move |_, _| Reply::Answer(MessageType::Result, map(vec![("value", answer.clone())])))
}

/// Everything one hosted run produced.
struct Run {
    result: Result<Value, Diagnostic>,
    /// Every `Call` the stand-in saw.
    calls: Vec<Made>,
    /// Every `Authorize` question the stand-in saw.
    asked: Vec<Made>,
    /// Every message the stand-in saw, in order: its type and the function's name.
    order: Vec<(MessageType, String)>,
    /// Every event the Executor logged at `debug` or above, one JSON object each.
    events: Vec<serde_json::Value>,
}

/// A handler that refuses every question.
fn refusing() -> Handler {
    answering(Wire::Boolean(false))
}

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

/// Run `source` with `registered` — `(name, blanket)` pairs — each call answered by `answer`, as
/// the connection would: issue the call, and route the reply envelope back under its id.
fn run_hosted(
    source: &str,
    variables: Vec<(Arc<str>, Value)>,
    registered: &[(&str, bool)],
    answer: Answer,
) -> (Result<Value, Diagnostic>, Vec<Made>) {
    let (result, seen, _) = run_logged(source, variables, registered, answer);
    (result, seen)
}

/// A log sink the test reads back.
#[derive(Clone, Default)]
struct Captured(Arc<Mutex<Vec<u8>>>);

impl std::io::Write for Captured {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// [`run_hosted`], also returning every event the Executor logged at `debug` or above, one JSON
/// object each. Every question for a per-call handler is refused.
fn run_logged(
    source: &str,
    variables: Vec<(Arc<str>, Value)>,
    registered: &[(&str, bool)],
    answer: Answer,
) -> (Result<Value, Diagnostic>, Vec<Made>, Vec<serde_json::Value>) {
    let run = run_full(source, variables, registered, refusing(), answer);
    (run.result, run.calls, run.events)
}

/// Run `source` against the stand-in: `authorize` answers each `Authorize` question, `answer`
/// each `Call`.
///
/// Every hosted run installs a subscriber, so no Executor callsite is ever first hit with none:
/// `tracing` caches that interest process-wide, and a callsite cached as disabled would hide the
/// events [`a_refused_call_is_logged_with_its_reason_and_the_script_cannot_tell`] reads.
fn run_full(
    source: &str,
    variables: Vec<(Arc<str>, Value)>,
    registered: &[(&str, bool)],
    authorize: Handler,
    answer: Answer,
) -> Run {
    run_limited(
        source,
        variables,
        registered,
        authorize,
        answer,
        Limits::default(),
    )
}

/// [`run_full`] under a Resource Budget with `limits` (Story 3.6).
fn run_limited(
    source: &str,
    variables: Vec<(Arc<str>, Value)>,
    registered: &[(&str, bool)],
    authorize: Handler,
    answer: Answer,
    limits: Limits,
) -> Run {
    let log = Captured::default();
    let writer = log.clone();
    let dispatch = tracing::Dispatch::new(
        tracing_subscriber::fmt()
            .json()
            .with_max_level(tracing::Level::DEBUG)
            .with_writer(Mutex::new(writer))
            .finish(),
    );
    let mut run = tracing::dispatcher::with_default(&dispatch, || {
        hosted_run(source, variables, registered, authorize, answer, limits)
    });
    let text = String::from_utf8(log.0.lock().unwrap().clone()).unwrap();
    run.events = text
        .lines()
        .map(|line| serde_json::from_str(line).expect("one JSON object per line"))
        .collect();
    run
}

fn hosted_run(
    source: &str,
    variables: Vec<(Arc<str>, Value)>,
    registered: &[(&str, bool)],
    authorize: Handler,
    answer: Answer,
    limits: Limits,
) -> Run {
    let seen: Seen = Arc::default();
    let questions: Seen = Arc::default();
    let order: Arc<Mutex<Vec<(MessageType, String)>>> = Arc::default();
    let runtime = runtime();
    let (mut calls, caller) = Calls::new();
    let (log, asked, sequence) = (
        Arc::clone(&seen),
        Arc::clone(&questions),
        Arc::clone(&order),
    );
    runtime.spawn(async move {
        while let Some(call) = calls.submitted().await {
            let envelope = calls.issue(call).expect("the execution waits for it");
            let id = envelope.id.unwrap();
            let Wire::Map(fields) = &envelope.payload else {
                panic!("a Call payload is a map");
            };
            assert_eq!(fields.len(), 2, "exactly `name` and `arguments`");
            assert_eq!(fields[0].0, s("name"));
            assert_eq!(fields[1].0, s("arguments"));
            let name = fields[0].1.as_str().unwrap().to_owned();
            let Wire::Array(arguments) = fields[1].1.clone() else {
                panic!("`arguments` is an array");
            };
            sequence
                .lock()
                .unwrap()
                .push((envelope.message_type, name.clone()));
            let reply = match envelope.message_type {
                MessageType::Call => {
                    let (message_type, payload) = answer(&name, &arguments);
                    log.lock().unwrap().push((name, arguments));
                    Reply::Answer(message_type, payload)
                }
                MessageType::Authorize => {
                    let reply = authorize(&name, &arguments);
                    asked.lock().unwrap().push((name, arguments));
                    reply
                }
                other => panic!("the Executor sent a `{other}`"),
            };
            match reply {
                Reply::Answer(message_type, payload) => calls
                    .complete(Envelope::new(id, message_type, payload))
                    .unwrap(),
                Reply::Silent => {}
                Reply::Hangup => calls.close(),
                Reply::Unframable => calls.unsendable(id, "its frame would be too large"),
            }
        }
    });
    let host = Host::new(registered.iter().copied(), caller);
    let result = runtime.block_on(execute_with_limits(
        program(source),
        variables,
        host,
        limits,
    ));
    let calls = seen.lock().unwrap().clone();
    let asked = questions.lock().unwrap().clone();
    let order = order.lock().unwrap().clone();
    Run {
        result,
        calls,
        asked,
        order,
        events: Vec::new(),
    }
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
        &[("getOrder", true)],
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
        &[("echo", true)],
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
        &[("double", true), ("next", true), ("count", true)],
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
        &[("nothing", true)],
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
        &[("getOrder", true)],
        Box::new(|_, _| value(Wire::Nil)),
    );
    let error = result.unwrap_err();
    assert_eq!(error.category.as_str(), "capability");
    assert_eq!(error.code.as_str(), "capability.unknown_function");
    assert_eq!(spanned(source, &error), "nope(x)");
    assert!(seen.is_empty());
}

// --- Story 3.3: the per-call handler ---

#[test]
fn a_call_without_a_grant_asks_the_handler_and_true_lets_it_proceed() {
    let run = run_full(
        "return getOrder(7, { a: 1 });",
        vec![],
        &[("getOrder", false)],
        answering(Wire::Boolean(true)),
        Box::new(|_, _| value(Wire::from(42))),
    );
    assert_eq!(run.result.unwrap().as_number(), Some(42.0));
    let arguments = vec![Wire::from(7), map(vec![("a", Wire::from(1))])];
    // The question carries exactly what the call does, and comes first.
    assert_eq!(run.asked, [("getOrder".to_owned(), arguments.clone())]);
    assert_eq!(run.calls, [("getOrder".to_owned(), arguments)]);
    assert_eq!(
        run.order,
        [
            (MessageType::Authorize, "getOrder".to_owned()),
            (MessageType::Call, "getOrder".to_owned())
        ]
    );
}

#[test]
fn a_blanket_granted_call_asks_nothing() {
    let run = run_full(
        "return getOrder(7);",
        vec![],
        &[("getOrder", true)],
        refusing(),
        Box::new(|_, _| value(Wire::from(1))),
    );
    assert_eq!(run.result.unwrap().as_number(), Some(1.0));
    assert!(run.asked.is_empty());
    assert_eq!(run.order, [(MessageType::Call, "getOrder".to_owned())]);
}

#[test]
fn the_handler_is_asked_on_every_call_never_cached() {
    // Popped from the end: `true` for the first call, `false` for the second.
    let answers = Arc::new(Mutex::new(vec![false, true]));
    let run = run_full(
        "let a = getOrder(1); let b = getOrder(2); return [a, b];",
        vec![],
        &[("getOrder", false)],
        Box::new(move |_, _| {
            let allowed = answers.lock().unwrap().pop().unwrap();
            Reply::Answer(
                MessageType::Result,
                map(vec![("value", Wire::Boolean(allowed))]),
            )
        }),
        Box::new(|_, arguments| value(arguments[0].clone())),
    );
    // The first call was allowed and made; the second, asked anew, was denied.
    assert_eq!(
        run.result.unwrap_err().code.as_str(),
        "capability.unknown_function"
    );
    assert_eq!(run.asked.len(), 2, "asked once per call");
    assert_eq!(run.calls, [("getOrder".to_owned(), vec![Wire::from(1)])]);
    assert_eq!(
        run.order.iter().map(|(t, _)| *t).collect::<Vec<_>>(),
        [
            MessageType::Authorize,
            MessageType::Call,
            MessageType::Authorize
        ]
    );
}

#[test]
fn a_question_that_cannot_be_framed_is_denied_as_handler_failed() {
    let (denial, reason, asked) = denied(Box::new(|_, _| Reply::Unframable));
    assert_eq!(reason, "handler_failed");
    assert_eq!(asked, 1);
    assert_eq!(denial.code.as_str(), "capability.unknown_function");
}

/// Run `return getOrder(x);` with `getOrder` registered without a grant, its handler answering as
/// `authorize` does: the Script's error, and the logged refusal's reason. Nothing may be called.
fn denied(authorize: Handler) -> (Diagnostic, String, usize) {
    let run = run_full(
        "let x = 1;\nreturn getOrder(x);",
        vec![],
        &[("getOrder", false)],
        authorize,
        Box::new(|_, _| value(Wire::Nil)),
    );
    assert!(run.calls.is_empty(), "a denied call is never sent");
    let refusals: Vec<_> = run
        .events
        .into_iter()
        .filter(|event| event["fields"]["message"] == "refused a host call")
        .collect();
    assert_eq!(refusals.len(), 1, "{refusals:?}");
    let event = &refusals[0];
    assert_eq!(event["level"], "DEBUG");
    assert_eq!(event["fields"]["function"], "getOrder");
    (
        run.result.unwrap_err(),
        event["fields"]["reason"].as_str().unwrap().to_owned(),
        run.asked.len(),
    )
}

#[test]
fn every_denial_is_the_error_an_unregistered_name_gets_and_only_the_log_tells_them_apart() {
    let source = "let x = 1;\nreturn getOrder(x);";
    let (unregistered, _) = run_hosted(
        source,
        vec![],
        &[("other", true)],
        Box::new(|_, _| value(Wire::Nil)),
    );
    let unregistered = unregistered.unwrap_err();
    assert_eq!(unregistered.code.as_str(), "capability.unknown_function");
    assert_eq!(spanned(source, &unregistered), "getOrder(x)");

    let error = |kind: MessageType, payload: Wire| -> Handler {
        Box::new(move |_, _| Reply::Answer(kind, payload.clone()))
    };
    let cases: Vec<(&str, Handler, &str)> = vec![
        ("false", answering(Wire::Boolean(false)), "refused"),
        ("a string", answering(s("yes")), "handler_invalid"),
        ("a number", answering(Wire::from(1)), "handler_invalid"),
        ("null", answering(Wire::Nil), "handler_invalid"),
        (
            "a Result that is not {value}",
            error(
                MessageType::Result,
                map(vec![("allowed", Wire::Boolean(true))]),
            ),
            "handler_invalid",
        ),
        (
            "an Error",
            error(
                MessageType::Error,
                map(vec![("code", s("x")), ("message", s("no"))]),
            ),
            "handler_failed",
        ),
        (
            "a hangup",
            Box::new(|_, _| Reply::Hangup),
            "handler_no_reply",
        ),
    ];
    for (what, authorize, expected) in cases {
        let (denial, reason, asked) = denied(authorize);
        assert_eq!(reason, expected, "{what}");
        assert_eq!(asked, 1, "{what}");
        // Same category, code, message and span.
        assert_eq!(denial, unregistered, "{what}");
    }
}

#[test]
fn a_handler_that_never_answers_is_denied_after_the_timeout() {
    assert_eq!(
        hexput_exec::AUTHORIZATION_TIMEOUT,
        std::time::Duration::from_secs(5)
    );
    let started = std::time::Instant::now();
    let (denial, reason, asked) = denied(Box::new(|_, _| Reply::Silent));
    assert!(started.elapsed() >= hexput_exec::AUTHORIZATION_TIMEOUT);
    assert_eq!(reason, "handler_timeout");
    assert_eq!(asked, 1);
    assert_eq!(denial.code.as_str(), "capability.unknown_function");
}

#[test]
fn a_refused_call_is_logged_with_its_reason_and_the_script_cannot_tell() {
    let (refused, reason, _) = denied(refusing());
    assert_eq!(reason, "refused");
    let (result, seen, events) = run_logged(
        "let x = 1;\nreturn getOrder(x);",
        vec![],
        &[],
        Box::new(|_, _| value(Wire::Nil)),
    );
    assert!(seen.is_empty());
    let reasons: Vec<_> = events
        .iter()
        .filter(|event| event["fields"]["message"] == "refused a host call")
        .map(|event| event["fields"]["reason"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(reasons, ["unregistered"]);
    let unregistered = result.unwrap_err();
    assert_eq!(refused, unregistered);
    assert!(!refused.message.contains("grant"), "{}", refused.message);
    assert!(!refused.message.contains("refus"), "{}", refused.message);

    // A granted call logs no refusal.
    let (result, seen, events) = run_logged(
        "return getOrder(1);",
        vec![],
        &[("getOrder", true)],
        Box::new(|_, _| value(Wire::from(2))),
    );
    assert_eq!(result.unwrap().as_number(), Some(2.0));
    assert_eq!(seen.len(), 1);
    assert!(
        events
            .iter()
            .all(|event| event["fields"]["message"] != "refused a host call")
    );
}

#[test]
fn an_argument_error_comes_before_the_grant_decision() {
    // Measured first, so an argument error is reported whether or not the call would be granted.
    let source = format!("return getOrder({});", nested(ARGUMENT_DEPTH_LIMIT + 1));
    let (result, seen) = run_hosted(
        &source,
        vec![],
        &[("getOrder", false)],
        Box::new(|_, _| value(Wire::Nil)),
    );
    assert_eq!(result.unwrap_err().code.as_str(), "depth.argument_too_deep");
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
            &[("getOrder", true)],
            Box::new(|_, _| value(Wire::from(99))),
        );
        assert_eq!(result.unwrap().as_number(), Some(1.0), "{source}");
        assert!(seen.is_empty(), "{source}");
    }
    // A starting variable shadows it too: here it is a number, so calling it is a type error.
    let (result, seen) = run_hosted(
        "return getOrder(1);",
        vec![(Arc::from("getOrder"), Value::Number(5.0))],
        &[("getOrder", true)],
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
        &[("getOrder", true)],
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
        let (result, seen) = run_hosted(
            source,
            vec![],
            &[("send", true)],
            Box::new(|_, _| value(Wire::Nil)),
        );
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
        &[("send", true)],
        Box::new(|_, _| value(Wire::from(1))),
    );
    assert_eq!(result.unwrap().as_number(), Some(1.0));
    assert_eq!(seen.len(), 1);

    let deep = nested(13);
    let too_deep = format!("return send(1, {deep});");
    let (result, seen) = run_hosted(
        &too_deep,
        vec![],
        &[("send", true)],
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
        &[("getOrder", true)],
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
            &[("f", true)],
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
        .block_on(execute(
            program(source),
            vec![],
            Host::new([("f", true)], caller),
        ))
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
        Host::new([("f", true)], caller),
    ));
    let error = runtime.block_on(async move {
        let call = calls.submitted().await.unwrap();
        let envelope = calls.issue(call).unwrap();
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
        &[("send", true)],
        Box::new(|_, _| value(Wire::Nil)),
    );
    let error = result.unwrap_err();
    assert_eq!(error.code.as_str(), "host.function_failed");
    assert_eq!(spanned(source, &error), "send(a, b)");
    assert!(seen.is_empty());
}

// --- Story 3.4: no path to the host but a Registered Function ---

/// Spellings a Script might reach for to touch the filesystem, network, process, environment,
/// host memory, modules or reflection. None is bound: there is no standard library (§11).
const AMBIENT: &[&str] = &[
    "fs",
    "readFile",
    "writeFile",
    "open",
    "require",
    "import",
    "process",
    "env",
    "exec",
    "spawn",
    "system",
    "exit",
    "socket",
    "fetch",
    "http",
    "net",
    "eval",
    "Function",
    "globalThis",
    "global",
    "window",
    "self",
    "Deno",
    "std",
    "os",
    "io",
    "__proto__",
    "constructor",
    "prototype",
    "memory",
    "ptr",
    "alloc",
    "sleep",
    "setTimeout",
    "print",
    "console",
    "len",
    "push",
];

#[test]
fn no_ambient_name_is_bound_so_reading_one_is_an_undeclared_identifier() {
    for name in AMBIENT {
        let source = format!("return {name};");
        let error = run(&source, vec![]).unwrap_err();
        assert_eq!(
            error.code.as_str(),
            "reference.undeclared_identifier",
            "reading `{name}`"
        );
    }
}

#[test]
fn calling_an_ambient_name_is_a_capability_denial_and_nothing_reaches_the_backend() {
    for name in AMBIENT {
        let source = format!("return {name}(\"/etc/passwd\");");
        // The Session registered something else, granted blanket: only that is reachable.
        let (result, seen) = run_hosted(
            &source,
            vec![],
            &[("getOrder", true)],
            Box::new(|_, _| value(Wire::Nil)),
        );
        let error = result.unwrap_err();
        assert_eq!(
            error.code.as_str(),
            "capability.unknown_function",
            "calling `{name}`"
        );
        assert!(seen.is_empty(), "calling `{name}` sent {seen:?}");
    }
}

#[test]
fn a_member_call_on_an_ambient_name_is_still_just_an_undeclared_identifier() {
    // `process.exit(1)`: the base is read first, and nothing named `process` exists.
    for source in ["return process.exit(1);", "return fs.readFile(\"x\");"] {
        let (result, seen) = run_hosted(
            source,
            vec![],
            &[("getOrder", true)],
            Box::new(|_, _| value(Wire::Nil)),
        );
        assert_eq!(
            result.unwrap_err().code.as_str(),
            "reference.undeclared_identifier",
            "{source}"
        );
        assert!(seen.is_empty());
    }
}

#[test]
fn an_ambient_call_asks_no_handler_whatever_the_session_registered() {
    // With a per-call registration and with none at all, an unregistered ambient name is refused
    // before any question or call: nothing reaches the Backend, and the log says `unregistered`.
    for registered in [&[("getOrder", false)][..], &[][..]] {
        for name in ["process", "fetch", "require"] {
            let run = run_full(
                &format!("return {name}(1);"),
                vec![],
                registered,
                answering(Wire::Boolean(true)),
                Box::new(|_, _| value(Wire::Nil)),
            );
            assert_eq!(
                run.result.unwrap_err().code.as_str(),
                "capability.unknown_function"
            );
            assert!(
                run.asked.is_empty() && run.calls.is_empty(),
                "{:?}",
                run.order
            );
            assert!(
                run.events
                    .iter()
                    .any(|event| event["fields"]["reason"] == "unregistered"),
                "{:?}",
                run.events
            );
        }
    }
}

#[test]
fn no_value_carries_a_reflective_member_or_a_method() {
    // Reflection spellings are ordinary keys: absent on objects (null), and not properties of
    // other values at all (a `type` error) — never a way out.
    for source in [
        "return ({}).constructor;",
        "return ({}).__proto__;",
        "return ({}).prototype;",
    ] {
        assert!(run(source, vec![]).unwrap().is_null(), "{source}");
    }
    for source in [
        "return \"x\".constructor;",
        "return [].__proto__;",
        "return (fn() { return 1; }).prototype;",
    ] {
        assert_eq!(
            run(source, vec![]).unwrap_err().category.as_str(),
            "type",
            "{source}"
        );
    }
    // A method call on a plain value is an ordinary property call: nothing is sent.
    for source in [
        "return [].push(1);",
        "return \"s\".len();",
        "return ({ a: 1 }).exec(\"x\");",
    ] {
        let (result, seen) = run_hosted(
            source,
            vec![],
            &[("push", true), ("len", true), ("exec", true)],
            Box::new(|_, _| value(Wire::Nil)),
        );
        assert!(result.is_err(), "{source}");
        assert_ne!(result.unwrap_err().category.as_str(), "host", "{source}");
        assert!(seen.is_empty(), "{source} sent {seen:?}");
    }
}

// --- Story 3.5: CPU time and memory budgets ---

/// The Script's CPU time limit: `hexput-enforce`'s documented default.
const CPU_TIME: std::time::Duration = hexput_enforce::DEFAULT_CPU_TIME;

/// A loop that doubles `s` twelve times, to 64 KiB.
const SIXTY_FOUR_KIB: &str =
    "let s = \"xxxxxxxxxxxxxxxx\"; let i = 0; while (i < 12) { s = s + s; i = i + 1; };";

#[test]
fn the_six_dimensions_spell_their_stable_names_in_order() {
    use hexput_shared::budget::Dimension;
    assert_eq!(
        Dimension::ALL
            .iter()
            .map(|d| d.as_str())
            .collect::<Vec<_>>(),
        [
            "cpu_time",
            "memory",
            "allocations",
            "rpc_calls",
            "output_size",
            "side_effects"
        ]
    );
}

#[test]
fn a_runaway_loop_is_stopped_just_past_its_cpu_time_and_frees_its_thread() {
    // One blocking thread: the second execution runs only if the runaway gave its thread back.
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .max_blocking_threads(1)
        .build()
        .unwrap();
    let dispatch = tracing::Dispatch::new(
        tracing_subscriber::fmt()
            .with_max_level(tracing::Level::TRACE)
            .with_writer(std::io::sink)
            .finish(),
    );
    tracing::dispatcher::with_default(&dispatch, || {
        let source = "let x = 0;\nwhile (true) { x = x + 1; }";
        let started = std::time::Instant::now();
        let diagnostic = runtime
            .block_on(execute(program(source), vec![], Host::none()))
            .unwrap_err();
        let took = started.elapsed();
        assert_eq!(diagnostic.code.as_str(), "budget.cpu_time_exceeded");
        assert_eq!(diagnostic.category.as_str(), "budget");
        assert_eq!(diagnostic.span.line, 2, "spanned inside the loop");
        assert!(took >= CPU_TIME, "stopped after {took:?}");
        // The acceptance criterion's generous bound: a slice is milliseconds even in a debug
        // build, so three seconds past the limit is far more than a runaway ever gets, even on
        // a loaded CI machine.
        assert!(
            took < CPU_TIME + std::time::Duration::from_secs(3),
            "stopped after {took:?}"
        );
        let result = runtime
            .block_on(execute(program("return 1;"), vec![], Host::none()))
            .unwrap();
        assert_eq!(result.as_number(), Some(1.0));
    });
}

#[test]
fn endless_recursion_within_the_call_depth_is_stopped_by_cpu_time() {
    let source = "fn fib(n) { if (n < 2) { return n; }; return fib(n - 1) + fib(n - 2); };\n\
                  return fib(60);";
    let diagnostic = run(source, vec![]).unwrap_err();
    assert_eq!(diagnostic.code.as_str(), "budget.cpu_time_exceeded");
}

#[test]
fn a_growing_string_is_stopped_at_the_memory_budget() {
    let source = "let s = \"x\";\nwhile (true) { s = s + s; }";
    let diagnostic = run(source, vec![]).unwrap_err();
    assert_eq!(diagnostic.code.as_str(), "budget.memory_exceeded");
    assert_eq!(diagnostic.category.as_str(), "budget");
    assert_eq!(
        spanned(source, &diagnostic),
        "+",
        "the concatenation that would cross it"
    );
}

#[test]
fn a_growing_collection_is_stopped_at_the_memory_budget() {
    // A thousand distinct 64 KiB elements is 64 MiB.
    let source = format!(
        "{SIXTY_FOUR_KIB}\nlet a = []; let n = 0;\nwhile (true) {{ a[n] = s + n; n = n + 1; }}"
    );
    let diagnostic = run(&source, vec![]).unwrap_err();
    assert_eq!(diagnostic.code.as_str(), "budget.memory_exceeded");
    assert_eq!(diagnostic.span.line, 3, "{:?}", diagnostic.span);
}

#[test]
fn memory_a_script_lets_go_of_is_not_held_against_it() {
    // Two thousand 64 KiB strings — 128 MiB made in all — but only one held at a time.
    let source = format!(
        "{SIXTY_FOUR_KIB} let n = 0; let t = \"\"; while (n < 2000) {{ t = s + n; n = n + 1; }}; \
         return n;"
    );
    assert_eq!(run(&source, vec![]).unwrap().as_number(), Some(2000.0));
}

#[test]
fn waiting_for_the_backend_is_never_charged_as_cpu_time() {
    let wait = CPU_TIME + std::time::Duration::from_millis(300);
    let (result, seen) = run_hosted(
        "let a = slowly(1); return a + 1;",
        vec![],
        &[("slowly", true)],
        Box::new(move |_, _| {
            // The stand-in answers on the runtime's own thread; the execution is waiting.
            std::thread::sleep(wait);
            value(Wire::from(41))
        }),
    );
    assert_eq!(result.unwrap().as_number(), Some(42.0));
    assert_eq!(seen.len(), 1);
}

#[test]
fn cpu_time_adds_up_across_host_calls_and_the_calls_already_made_stand() {
    // Each segment runs well under the limit; together they pass it. The RPC call and
    // side-effect limits (Story 3.6) are lifted, so CPU time is the only dimension that can cross.
    let source = "while (true) {\n  let j = 0; while (j < 2000) { j = j + 1; };\n  ping(j);\n}";
    let run = run_limited(
        source,
        vec![],
        &[("ping", true)],
        refusing(),
        Box::new(|_, _| value(Wire::Nil)),
        calls_unbounded(),
    );
    let diagnostic = run.result.unwrap_err();
    assert_eq!(diagnostic.code.as_str(), "budget.cpu_time_exceeded");
    assert!(run.calls.len() > 1, "{} calls", run.calls.len());
    let stopped: Vec<_> = run
        .events
        .iter()
        .filter(|event| event["fields"]["message"] == "stopped an execution over its budget")
        .collect();
    assert_eq!(stopped.len(), 1, "{:?}", run.events);
    assert_eq!(stopped[0]["level"], "DEBUG");
    assert_eq!(stopped[0]["fields"]["dimension"], "cpu_time");
}

/// The default limits with the RPC call and side-effect limits lifted, so a test of CPU time can
/// make as many calls as it takes.
fn calls_unbounded() -> Limits {
    Limits::default()
        .with_rpc_calls(u64::MAX)
        .with_side_effects(u64::MAX)
}

#[test]
fn a_memory_stop_is_logged_with_its_dimension() {
    let run = run_full(
        "let s = \"x\"; while (true) { s = s + s; }",
        vec![],
        &[],
        refusing(),
        Box::new(|_, _| value(Wire::Nil)),
    );
    assert_eq!(
        run.result.unwrap_err().code.as_str(),
        "budget.memory_exceeded"
    );
    let dimensions: Vec<_> = run
        .events
        .iter()
        .filter(|event| event["fields"]["message"] == "stopped an execution over its budget")
        .map(|event| event["fields"]["dimension"].clone())
        .collect();
    assert_eq!(dimensions, ["memory"]);
}

#[test]
fn the_call_whose_charge_crosses_the_limit_is_never_sent() {
    // Each segment is well under a slice, so most charges are taken at a call, before the call is
    // sent. The stand-in sees calls 1..=k, each answered at once, and no question: whatever
    // call the Script stopped at, or was about to make, was never sent. The RPC call and
    // side-effect limits (Story 3.6) are lifted, so CPU time is the only dimension that can cross.
    let source = "let n = 0;\nwhile (true) {\n  let j = 0; while (j < 200) { j = j + 1; };\n  \
                  n = n + 1;\n  ping(n);\n}";
    let run = run_limited(
        source,
        vec![],
        &[("ping", true)],
        refusing(),
        Box::new(|_, _| value(Wire::Nil)),
        calls_unbounded(),
    );
    let diagnostic = run.result.unwrap_err();
    assert_eq!(diagnostic.code.as_str(), "budget.cpu_time_exceeded");
    // Where the limit is crossed depends on timing: at the charge taken at a call (spanned on
    // `ping(n)`, which is then never sent) or at a slice boundary inside the inner loop. Either
    // way, every call that was sent was answered and resumed from, and none came after the stop.
    let at = spanned(source, &diagnostic);
    assert!(at == "ping(n)" || source.contains(at), "{at:?}");
    assert!(run.asked.is_empty(), "no Authorize: {:?}", run.asked);
    assert!(!run.calls.is_empty());
    let sent: Vec<_> = run
        .calls
        .iter()
        .map(|(_, arguments)| arguments[0].as_u64().unwrap())
        .collect();
    let expected: Vec<u64> = (1..=sent.len() as u64).collect();
    assert_eq!(
        sent, expected,
        "every call sent was answered and resumed from"
    );
    assert!(
        run.order.iter().all(|(kind, _)| *kind == MessageType::Call),
        "{:?}",
        run.order
    );
}

#[test]
fn a_reply_that_crosses_the_memory_budget_ends_the_script_on_its_call() {
    let source = "let big = fetch();\nreturn 1;";
    let (result, seen) = run_hosted(
        source,
        vec![],
        &[("fetch", true)],
        Box::new(|_, _| value(Wire::from("x".repeat(65 * 1024 * 1024)))),
    );
    let diagnostic = result.unwrap_err();
    assert_eq!(diagnostic.code.as_str(), "budget.memory_exceeded");
    assert_eq!(spanned(source, &diagnostic), "fetch()");
    assert_eq!(seen.len(), 1);
}

// --- Story 3.6: allocations, RPC calls, output size and side effects ---

/// The fixed Script of Story 3.6 (the interpreter tests pin its seventeen allocations), making two
/// host calls and returning `{ name: "hello, 42", n: 9 }`.
const FIXED: &str = "let greeting = \"hello\";\n\
                     let name = greeting + \", \" + 42;\n\
                     let a = [];\n\
                     let i = 0;\n\
                     while (i < 9) { a[i] = i; i = i + 1; };\n\
                     let o = { x: 1, y: \"z\" };\n\
                     o.w = 2;\n\
                     for (k in o) { let seen = k; };\n\
                     ping(a); ping(o);\n\
                     return { name: name, n: i };";

/// The exact MessagePack bytes of a Script result's `{value}` payload, as the codec writes it.
fn encoded_payload(result: &Value) -> Vec<u8> {
    let payload = map(vec![("value", hexput_exec::wire::to_wire(result))]);
    rmp_serde::to_vec(&payload).unwrap()
}

/// The stops the Executor logged, by dimension.
fn stopped_dimensions(events: &[serde_json::Value]) -> Vec<serde_json::Value> {
    events
        .iter()
        .filter(|event| event["fields"]["message"] == "stopped an execution over its budget")
        .map(|event| event["fields"]["dimension"].clone())
        .collect()
}

#[test]
fn the_fixed_script_makes_two_calls_and_a_twenty_six_byte_result() {
    let (result, seen) = run_hosted(
        FIXED,
        vec![],
        &[("ping", true)],
        Box::new(|_, _| value(Wire::Nil)),
    );
    let result = result.unwrap();
    assert_eq!(seen.len(), 2);
    // A one-entry map 1, `value` 6, a two-entry map 1, `name` 5, "hello, 42" 10, `n` 2, 9 1.
    assert_eq!(encoded_payload(&result).len(), 26);
    assert_eq!(hexput_exec::wire::payload_size(&result, usize::MAX), 26);
}

#[test]
fn the_fixed_script_uses_two_of_the_hundred_rpc_calls_and_side_effects() {
    // Ninety-eight more calls fit exactly; ninety-nine do not.
    for (more, fits) in [(98, true), (99, false)] {
        let source = format!("let c = 0; while (c < {more}) {{ c = c + 1; ping(c); }};\n{FIXED}");
        let (result, seen) = run_hosted(
            &source,
            vec![],
            &[("ping", true)],
            Box::new(|_, _| value(Wire::Nil)),
        );
        if fits {
            assert!(result.is_ok(), "{result:?}");
            assert_eq!(seen.len(), 100);
        } else {
            let diagnostic = result.unwrap_err();
            assert_eq!(diagnostic.code.as_str(), "budget.rpc_calls_exceeded");
            assert_eq!(spanned(&source, &diagnostic), "ping(o)");
            assert_eq!(
                seen.len(),
                100,
                "the call that would cross it is never sent"
            );
        }
    }
}

#[test]
fn the_payload_size_is_the_exact_encoded_length() {
    let values = [
        "null",
        "true",
        "0",
        "-0",
        "127",
        "128",
        "255",
        "256",
        "65535",
        "65536",
        "4294967295",
        "4294967296",
        "9007199254740992",
        "-1",
        "-32",
        "-33",
        "-128",
        "-129",
        "-32768",
        "-32769",
        "-2147483648",
        "-2147483649",
        "-9007199254740992",
        "1.5",
        "9007199254740994",
        "\"\"",
        "\"0123456789012345678901234567890\"",
        "\"01234567890123456789012345678901\"",
        "[]",
        "[1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15]",
        "[1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16]",
        "{ a: [1, { b: \"c\" }], d: null }",
    ];
    for source in values {
        let result = run(&format!("return {source};"), vec![]).unwrap();
        assert_eq!(
            hexput_exec::wire::payload_size(&result, usize::MAX),
            encoded_payload(&result).len(),
            "{source}"
        );
    }
    // Long strings and collections, at each header size's boundaries.
    for length in [255, 256, 65_535, 65_536] {
        let text = Value::String(Arc::from("x".repeat(length)));
        let array = Value::Array(hexput_interpreter::Array::from_values(
            (0..length).map(|i| Value::Number(i as f64)).collect(),
        ));
        let keys: Vec<String> = (0..length).map(|i| format!("k{i}")).collect();
        let object = Value::Object(hexput_interpreter::Object::from_entries(
            keys.iter().map(|key| (key.as_str(), Value::Null)),
        ));
        let result = Value::Array(hexput_interpreter::Array::from_values(vec![
            text, array, object,
        ]));
        assert_eq!(
            hexput_exec::wire::payload_size(&result, usize::MAX),
            encoded_payload(&result).len(),
            "length {length}"
        );
    }
}

#[test]
fn a_payload_past_the_limit_stops_being_measured() {
    // `[x, x]` nested sixty times is 2^60 elements on the wire: measured only just past the
    // limit, never expanded.
    let mut value = Value::Array(hexput_interpreter::Array::from_values(vec![Value::Null]));
    for _ in 0..60 {
        value = Value::Array(hexput_interpreter::Array::from_values(vec![
            value.clone(),
            value,
        ]));
    }
    let size = hexput_exec::wire::payload_size(&value, 1024);
    assert!(size > 1024 && size < 2048, "{size}");
}

#[test]
fn a_result_past_the_output_size_budget_is_refused_though_it_fits_a_frame() {
    // A `{value}` payload of a string is 1 + 6 + 5 + its bytes: at the 1 MiB limit exactly, and
    // one byte past it.
    let limit = hexput_enforce::DEFAULT_OUTPUT_SIZE;
    let fits = limit - 12;
    let result = run(
        "return s;",
        vec![(Arc::from("s"), Value::String(Arc::from("x".repeat(fits))))],
    )
    .unwrap();
    assert_eq!(encoded_payload(&result).len(), limit);

    let run = run_full(
        "return s;",
        vec![(
            Arc::from("s"),
            Value::String(Arc::from("x".repeat(fits + 1))),
        )],
        &[],
        refusing(),
        Box::new(|_, _| value(Wire::Nil)),
    );
    let diagnostic = run.result.unwrap_err();
    assert_eq!(diagnostic.code.as_str(), "budget.output_size_exceeded");
    assert_eq!(diagnostic.category.as_str(), "budget");
    assert_eq!(stopped_dimensions(&run.events), ["output_size"]);
}

#[test]
fn an_rpc_flood_is_stopped_at_the_call_that_would_cross_the_limit() {
    let source = "let n = 0;\nwhile (true) { n = n + 1; ping(n); }";
    let run = run_full(
        source,
        vec![],
        &[("ping", true)],
        refusing(),
        Box::new(|_, _| value(Wire::Nil)),
    );
    let diagnostic = run.result.unwrap_err();
    assert_eq!(diagnostic.code.as_str(), "budget.rpc_calls_exceeded");
    assert_eq!(diagnostic.category.as_str(), "budget");
    assert_eq!(spanned(source, &diagnostic), "ping(n)");
    // The hundred calls made stand, each answered; the hundred-and-first was never sent.
    let sent: Vec<u64> = run
        .calls
        .iter()
        .map(|(_, arguments)| arguments[0].as_u64().unwrap())
        .collect();
    assert_eq!(sent, (1..=100).collect::<Vec<u64>>());
    assert_eq!(stopped_dimensions(&run.events), ["rpc_calls"]);
}

#[test]
fn a_refused_call_counts_before_its_capability_is_decided() {
    // A hundred calls made; the next call is to an unregistered name. It is charged before the
    // capability decision, so it crosses the RPC call budget rather than being refused.
    let over = "let n = 0; while (n < 100) { n = n + 1; ping(n); };\nnope();";
    let (result, seen) = run_hosted(
        over,
        vec![],
        &[("ping", true)],
        Box::new(|_, _| value(Wire::Nil)),
    );
    let diagnostic = result.unwrap_err();
    assert_eq!(diagnostic.code.as_str(), "budget.rpc_calls_exceeded");
    assert_eq!(spanned(over, &diagnostic), "nope()");
    assert_eq!(seen.len(), 100);
    // Ninety-nine calls: the refusal is the hundredth, a counted call ending in `capability`.
    let within = "let n = 0; while (n < 99) { n = n + 1; ping(n); };\nnope();";
    let (result, _) = run_hosted(
        within,
        vec![],
        &[("ping", true)],
        Box::new(|_, _| value(Wire::Nil)),
    );
    assert_eq!(
        result.unwrap_err().code.as_str(),
        "capability.unknown_function"
    );
}

#[test]
fn a_denied_call_counts_and_its_question_is_part_of_it() {
    let registered = [("ping", true), ("ask", false)];
    // Ninety-nine calls, then a question the handler refuses: counted as the hundredth call,
    // ending in `capability` — the question is not a second count.
    let within = "let n = 0; while (n < 99) { n = n + 1; ping(n); };\nask();";
    let run = run_full(
        within,
        vec![],
        &registered,
        refusing(),
        Box::new(|_, _| value(Wire::Nil)),
    );
    assert_eq!(
        run.result.unwrap_err().code.as_str(),
        "capability.unknown_function"
    );
    assert_eq!(run.asked.len(), 1);
    // A hundred calls: the call is charged before anything is asked, and nothing is.
    let over = "let n = 0; while (n < 100) { n = n + 1; ping(n); };\nask();";
    let run = run_full(
        over,
        vec![],
        &registered,
        refusing(),
        Box::new(|_, _| value(Wire::Nil)),
    );
    assert_eq!(
        run.result.unwrap_err().code.as_str(),
        "budget.rpc_calls_exceeded"
    );
    assert!(run.asked.is_empty(), "{:?}", run.asked);
    // Allowed per call: a hundred questions, each answered `true`, and a hundred calls count as
    // a hundred RPC calls — exactly the budget — since each question is part of its call.
    let allowed = "let n = 0; while (n < 100) { n = n + 1; ask(n); };\nreturn n;";
    let run = run_full(
        allowed,
        vec![],
        &registered,
        answering(Wire::Boolean(true)),
        Box::new(|_, _| value(Wire::Nil)),
    );
    assert_eq!(run.result.unwrap().as_number(), Some(100.0));
    assert_eq!((run.asked.len(), run.calls.len()), (100, 100));
}

#[test]
fn a_call_whose_arguments_cannot_be_sent_is_not_counted() {
    // A hundred calls made: a hundred-and-first with a sendable argument crosses the RPC call
    // budget, but one whose argument cannot be sent ends in that argument's error — it never
    // became a host call, so it is not counted.
    let deep = "[".repeat(ARGUMENT_DEPTH_LIMIT + 1) + &"]".repeat(ARGUMENT_DEPTH_LIMIT + 1);
    for (argument, code) in [
        ("1", "budget.rpc_calls_exceeded"),
        ("fn() {}", "type.function_argument"),
        ("c", "type.cyclic_argument"),
        (deep.as_str(), "depth.argument_too_deep"),
    ] {
        let source = format!(
            "let c = []; c[0] = c; let n = 0; while (n < 100) {{ n = n + 1; ping(n); }};\n\
             ping({argument});"
        );
        let (result, seen) = run_hosted(
            &source,
            vec![],
            &[("ping", true)],
            Box::new(|_, _| value(Wire::Nil)),
        );
        assert_eq!(result.unwrap_err().code.as_str(), code, "ping({argument})");
        assert_eq!(seen.len(), 100);
    }
}

#[test]
fn allocation_churn_is_stopped_though_memory_stays_low() {
    // Each iteration builds a short string from twenty literals and lets it go: thirty-nine
    // allocations, and a few bytes held. (Collections would do, but the heap keeps a collection
    // until the execution ends.) A lower allocation limit than the default keeps the test far
    // inside its CPU time on any machine; the default is `hexput-enforce`'s tested constant.
    let literals = vec!["\"a\""; 20].join(" + ");
    let source = format!("while (true) {{\n  let t = {literals};\n}}");
    let run = run_limited(
        &source,
        vec![],
        &[],
        refusing(),
        Box::new(|_, _| value(Wire::Nil)),
        Limits::default().with_allocations(50_000),
    );
    let diagnostic = run.result.unwrap_err();
    assert_eq!(diagnostic.code.as_str(), "budget.allocations_exceeded");
    assert_eq!(diagnostic.category.as_str(), "budget");
    assert_eq!(diagnostic.span.line, 2, "{:?}", diagnostic.span);
    assert_eq!(stopped_dimensions(&run.events), ["allocations"]);
}

#[test]
fn the_allocation_limit_is_the_last_allocation_allowed() {
    // The fixed Script makes seventeen allocations (pinned by the interpreter tests).
    for (limit, fits) in [(17, true), (16, false)] {
        let run = run_limited(
            FIXED,
            vec![],
            &[("ping", true)],
            refusing(),
            Box::new(|_, _| value(Wire::Nil)),
            Limits::default().with_allocations(limit),
        );
        match run.result {
            Ok(_) => assert!(fits, "a limit of {limit}"),
            Err(diagnostic) => {
                assert!(!fits, "a limit of {limit}: {diagnostic:?}");
                assert_eq!(diagnostic.code.as_str(), "budget.allocations_exceeded");
                assert_eq!(spanned(FIXED, &diagnostic), "{ name: name, n: i }");
            }
        }
    }
}

#[test]
fn the_side_effect_limit_is_its_own() {
    // Three side effects allowed and a hundred RPC calls: the fourth call crosses the side-effect
    // limit alone, and is never sent.
    let source = "let n = 0;\nwhile (true) { n = n + 1; ping(n); }";
    let run = run_limited(
        source,
        vec![],
        &[("ping", true)],
        refusing(),
        Box::new(|_, _| value(Wire::Nil)),
        Limits::default().with_side_effects(3),
    );
    let diagnostic = run.result.unwrap_err();
    assert_eq!(diagnostic.code.as_str(), "budget.side_effects_exceeded");
    assert_eq!(spanned(source, &diagnostic), "ping(n)");
    assert_eq!(run.calls.len(), 3);
    assert_eq!(stopped_dimensions(&run.events), ["side_effects"]);
}

#[test]
fn each_dimension_crossed_alone_names_only_itself() {
    let fixed = || -> Answer { Box::new(|_, _| value(Wire::Nil)) };
    let crossed = |limits: Limits| {
        run_limited(
            FIXED,
            vec![],
            &[("ping", true)],
            refusing(),
            fixed(),
            limits,
        )
        .result
        .unwrap_err()
        .code
        .as_str()
        .to_owned()
    };
    // The fixed Script: seventeen allocations, two calls (two side effects), a 26-byte result.
    let at_limits = Limits::default()
        .with_allocations(17)
        .with_rpc_calls(2)
        .with_side_effects(2)
        .with_output_size(26);
    assert!(
        run_limited(
            FIXED,
            vec![],
            &[("ping", true)],
            refusing(),
            fixed(),
            at_limits
        )
        .result
        .is_ok()
    );
    assert_eq!(
        crossed(at_limits.with_allocations(16)),
        "budget.allocations_exceeded"
    );
    assert_eq!(
        crossed(at_limits.with_rpc_calls(1)),
        "budget.rpc_calls_exceeded"
    );
    assert_eq!(
        crossed(at_limits.with_side_effects(1)),
        "budget.side_effects_exceeded"
    );
    assert_eq!(
        crossed(at_limits.with_output_size(25)),
        "budget.output_size_exceeded"
    );
    assert_eq!(crossed(at_limits.with_memory(0)), "budget.memory_exceeded");
    assert_eq!(
        crossed(at_limits.with_cpu_time(std::time::Duration::ZERO)),
        "budget.cpu_time_exceeded"
    );
}
