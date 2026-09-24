//! Story 2.6: the one Executor entry point (AD-3). Story 3.1: host calls through it — the
//! capability check, the argument rules, and every way a call's reply can end the Script — driven
//! against a stand-in for the connection that answers from the test. Story 3.2: a blanket grant
//! lets a call go ahead at once; every registration here states its grant. Story 3.3: a call
//! without one is put to the Backend's per-call handler first, and only `true` lets it proceed.

use std::sync::{Arc, Mutex};

use hexput_exec::{ARGUMENT_DEPTH_LIMIT, Diagnostic, Host, Value, execute};
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
        hosted_run(source, variables, registered, authorize, answer)
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
    let result = runtime.block_on(execute(program(source), variables, host));
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
