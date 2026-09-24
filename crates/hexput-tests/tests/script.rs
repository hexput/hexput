//! Story 2.6: Direct Execution — the `ExecutionStart` payload, the wire↔value conversion both
//! ways, and the result guards.

use hexput_port::{
    CorrelationId, Envelope, ErrorBody, MAX_FRAME_LEN, MAX_NESTING_DEPTH, MessageType, Value,
    decode, encode, encode_frame,
};
use hexput_script::MAX_RESULT_DEPTH;
use rmpv::Integer;

/// Serve one Direct Execution with no Registered Functions, on a runtime of its own.
fn direct_execution(payload: &Value) -> Result<Value, Box<ErrorBody>> {
    direct_execution_registering(payload, Vec::new())
}

/// Serve one Direct Execution registering `registrations` — `(name, blanket)` pairs — on a runtime
/// of its own. Its call table is dropped at once, as if the connection were gone: every host call
/// and every question for a per-call handler fails with no reply.
fn direct_execution_registering(
    payload: &Value,
    registrations: Vec<(String, bool)>,
) -> Result<Value, Box<ErrorBody>> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
        .unwrap();
    let (calls, caller) = hexput_rpc::Calls::new();
    drop(calls);
    runtime.block_on(hexput_script::direct_execution(
        payload.clone(),
        registrations,
        caller,
    ))
}

fn s(text: &str) -> Value {
    Value::from(text)
}

fn map(fields: Vec<(&str, Value)>) -> Value {
    Value::Map(fields.into_iter().map(|(k, v)| (s(k), v)).collect())
}

fn payload(source: &str, variables: Vec<(&str, Value)>) -> Value {
    map(vec![("source", s(source)), ("variables", map(variables))])
}

/// The `value` of a successful run.
fn run(source: &str, variables: Vec<(&str, Value)>) -> Value {
    let reply = direct_execution(&payload(source, variables))
        .unwrap_or_else(|body| panic!("{source:?} failed: {body:?}"));
    let Value::Map(mut fields) = reply else {
        panic!("a Result payload is a map");
    };
    assert_eq!(fields.len(), 1, "exactly `value`");
    let (key, value) = fields.remove(0);
    assert_eq!(key, s("value"));
    value
}

fn failure(payload: &Value) -> ErrorBody {
    *direct_execution(payload).expect_err("a refusal")
}

fn int(n: i64) -> Value {
    Value::from(n)
}

// --- the matrix rows ---

#[test]
fn a_script_runs_with_its_starting_variables() {
    assert_eq!(run("return a + 1;", vec![("a", int(2))]), int(3));
}

#[test]
fn no_inputs_is_an_empty_map_not_a_missing_one() {
    assert_eq!(
        run("return [1, \"x\", null];", vec![]),
        Value::Array(vec![int(1), s("x"), Value::Nil])
    );
}

#[test]
fn objects_round_trip_keeping_their_key_order() {
    let o = map(vec![
        ("z", Value::Array(vec![Value::Boolean(true)])),
        ("a", map(vec![("k", s("v"))])),
        ("m", Value::F64(1.5)),
    ]);
    assert_eq!(run("return o;", vec![("o", o.clone())]), o);
}

#[test]
fn a_parse_failure_carries_the_parser_diagnostic_and_span() {
    let source = "let = ;";
    let expected = hexput_parser::parse(source).unwrap_err();
    let body = failure(&payload(source, vec![]));
    assert_eq!(body, ErrorBody::from(&expected));
    assert_eq!(body.category, "syntax");
    assert!(body.span.is_some());
}

#[test]
fn a_runtime_failure_carries_the_interpreter_diagnostic() {
    let body = failure(&payload("let x = 1;\nreturn x / 0;", vec![]));
    assert_eq!(body.category, "arithmetic");
    assert_eq!(body.code, "arithmetic.division_by_zero");
    assert_eq!(body.severity, "error");
    assert_eq!(body.span.unwrap().line, 2);
}

#[test]
fn a_starting_variable_the_script_declares_is_a_duplicate_declaration() {
    let body = failure(&payload("let a = 5; return a;", vec![("a", int(1))]));
    assert_eq!(body.code, "syntax.duplicate_declaration");
}

// --- the payload ---

#[test]
fn a_malformed_payload_is_refused_naming_the_key() {
    let cases: Vec<(Value, &str)> = vec![
        (Value::Nil, "missing `source` and `variables`"),
        (Value::Map(vec![]), "missing `source` and `variables`"),
        (map(vec![("variables", map(vec![]))]), "missing `source`"),
        (
            map(vec![("source", Value::Nil), ("variables", map(vec![]))]),
            "missing `source`",
        ),
        (map(vec![("source", s("return 1;"))]), "missing `variables`"),
        (int(3), "must be a map"),
        (
            map(vec![("source", int(1)), ("variables", map(vec![]))]),
            "`source` is not a string",
        ),
        (
            map(vec![
                ("source", s("return 1;")),
                ("variables", Value::Array(vec![])),
            ]),
            "`variables` is not a map",
        ),
        (
            map(vec![
                ("source", s("return 1;")),
                ("variables", map(vec![])),
                ("extra", int(1)),
            ]),
            "unknown key `extra`",
        ),
        (
            map(vec![
                ("source", s("return 1;")),
                ("source", s("return 2;")),
                ("variables", map(vec![])),
            ]),
            "repeats the key `source`",
        ),
        (
            Value::Map(vec![(int(1), int(1))]),
            "a key that is not a string",
        ),
        (
            payload("return 1;", vec![("user-id", int(1))]),
            "\"user-id\"",
        ),
        (payload("return 1;", vec![("let", int(1))]), "\"let\""),
        (payload("return 1;", vec![("1a", int(1))]), "\"1a\""),
        (payload("return 1;", vec![("", int(1))]), "\"\""),
        (
            payload("return 1;", vec![("a", int(1)), ("a", int(2))]),
            "`a` twice",
        ),
        (
            map(vec![
                ("source", s("return 1;")),
                ("variables", Value::Map(vec![(int(7), int(1))])),
            ]),
            "`variables` has a key that is not a string",
        ),
    ];
    for (payload, expected) in cases {
        let body = failure(&payload);
        assert_eq!(body.code, "protocol.invalid_payload", "{payload}");
        assert_eq!(body.category, "protocol");
        assert!(body.span.is_none());
        assert!(
            body.message.contains(expected),
            "{:?} names {expected:?}",
            body.message
        );
    }
}

#[test]
fn nothing_runs_when_the_payload_is_refused() {
    // The Script would fail at runtime; the payload's refusal is what comes back.
    let body = failure(&payload("return 1 / 0;", vec![("x", Value::F64(f64::NAN))]));
    assert_eq!(body.code, "protocol.invalid_payload");
}

#[test]
fn a_lossy_value_is_refused_naming_its_path() {
    let invalid_utf8 = rmpv::decode::read_value(&mut &[0xa2, 0xff, 0xfe][..]).unwrap();
    assert!(
        matches!(&invalid_utf8, Value::String(text) if text.is_err()),
        "a str value holding invalid UTF-8"
    );
    let nested = |leaf: Value| map(vec![("tags", Value::Array(vec![s("a"), s("b"), leaf]))]);
    let cases: Vec<(Value, &str)> = vec![
        (Value::F64(f64::NAN), "NaN"),
        (Value::F64(f64::INFINITY), "inf"),
        (Value::F64(f64::NEG_INFINITY), "-inf"),
        (Value::F32(f32::NAN), "NaN"),
        (Value::Binary(vec![1, 2]), "binary"),
        (Value::Ext(5, vec![0]), "extension (type 5)"),
        (Value::from(1_u64 << 60), "outside ±2^53"),
        (Value::from(-(1_i64 << 60)), "outside ±2^53"),
        (Value::from((1_u64 << 53) + 1), "outside ±2^53"),
        (Value::from(u64::MAX), "outside ±2^53"),
        (invalid_utf8, "not valid UTF-8"),
        (Value::Map(vec![(int(1), int(1))]), "not a string"),
        (
            Value::Map(vec![(s("k"), int(1)), (s("k"), int(2))]),
            "repeats the key \"k\"",
        ),
    ];
    for (leaf, expected) in cases {
        let body = failure(&payload("return 1;", vec![("user", nested(leaf.clone()))]));
        assert_eq!(body.code, "protocol.invalid_payload", "{leaf}");
        assert!(
            body.message.contains("`variables.user.tags[2]`"),
            "{:?} names the path",
            body.message
        );
        assert!(
            body.message.contains(expected),
            "{:?} names {expected:?}",
            body.message
        );
    }
}

#[test]
fn a_path_quotes_odd_keys_and_stays_bounded() {
    let long = "k".repeat(10_000);
    let body = failure(&payload(
        "return 1;",
        vec![(
            "o",
            map(vec![(
                "a b",
                map(vec![(long.as_str(), Value::Binary(vec![]))]),
            )]),
        )],
    ));
    assert!(
        body.message.starts_with("`variables.o[\"a b\"].kkk"),
        "{}",
        body.message
    );
    assert!(body.message.chars().count() < 400, "{}", body.message);
}

#[test]
fn a_refused_name_is_echoed_bounded() {
    let name = "-".repeat(10_000);
    let body = failure(&payload("return 1;", vec![(name.as_str(), int(1))]));
    assert!(body.message.chars().count() < 300, "{}", body.message);
}

#[test]
fn integers_up_to_two_to_the_fifty_three_are_exact_numbers() {
    let limit = 1_i64 << 53;
    for n in [limit, -limit, 0, -1, 42] {
        assert_eq!(run("return n;", vec![("n", int(n))]), int(n));
    }
    assert_eq!(
        run("return n;", vec![("n", Value::from(1_u64 << 53))]),
        int(limit)
    );
    assert_eq!(
        run("return n;", vec![("n", Value::F32(0.5))]),
        Value::F64(0.5)
    );
    assert_eq!(run("return n;", vec![("n", Value::F64(2.0))]), int(2));
}

// --- the result ---

#[test]
fn numbers_leave_as_integers_when_whole_and_exact_otherwise_as_floats() {
    assert_eq!(run("return 1.5;", vec![]), Value::F64(1.5));
    assert_eq!(run("return -0;", vec![]), int(0));
    assert_eq!(run("return 0 - 7;", vec![]), int(-7));
    assert_eq!(run("return 1e300;", vec![]), Value::F64(1e300));
    let limit = 9_007_199_254_740_992.0_f64;
    assert_eq!(
        run("return n;", vec![("n", Value::F64(limit))]),
        int(1_i64 << 53)
    );
    assert_eq!(
        run("return n * 2;", vec![("n", Value::F64(limit))]),
        Value::F64(limit * 2.0)
    );
}

/// A Script returning `n` nested arrays around `null`.
fn nested(n: usize) -> Result<Value, Box<ErrorBody>> {
    nested_by("[a]", n)
}

/// A Script returning `n` levels of `wrap` (an expression over the level below, `a`) around
/// `null`.
fn nested_by(wrap: &str, n: usize) -> Result<Value, Box<ErrorBody>> {
    direct_execution(&payload(
        &format!("let a = null; let i = 0; while (i < n) {{ a = {wrap}; i = i + 1; }}; return a;"),
        vec![("n", Value::from(n as u64))],
    ))
}

#[test]
fn a_result_at_the_depth_limit_is_sent_and_decodes_back() {
    assert_eq!(MAX_RESULT_DEPTH, MAX_NESTING_DEPTH - 2);
    let reply = nested(MAX_RESULT_DEPTH).unwrap();
    let envelope = Envelope::new(CorrelationId(1), MessageType::Result, reply);
    let bytes = encode(&envelope).unwrap();
    assert_eq!(
        decode(&bytes).unwrap(),
        envelope,
        "the Daemon's own decoder reads it back"
    );
}

#[test]
fn a_result_past_the_depth_limit_is_refused_before_it_is_converted() {
    for n in [MAX_RESULT_DEPTH + 1, 100_000] {
        let body = nested(n).unwrap_err();
        assert_eq!(body.code, "protocol.result_too_deep");
        assert_eq!(body.category, "protocol");
    }
}

#[test]
fn nested_objects_count_toward_the_depth_limit_like_arrays() {
    let reply = nested_by("{ k: a }", MAX_RESULT_DEPTH).unwrap();
    let envelope = Envelope::new(CorrelationId(1), MessageType::Result, reply);
    let bytes = encode(&envelope).unwrap();
    assert_eq!(decode(&bytes).unwrap(), envelope);
    // 10 000 levels: far past the limit, and well within the CPU time budget on a debug build.
    for n in [MAX_RESULT_DEPTH + 1, 10_000] {
        let body = nested_by("{ k: a }", n).unwrap_err();
        assert_eq!(body.code, "protocol.result_too_deep");
    }
}

#[test]
fn a_result_just_under_the_frame_passes_the_frame_check() {
    // The size check is a lower bound: a result that fits must never be refused by it. Under the
    // default Resource Budget such a result is refused first — as past the output size budget
    // (Story 3.6) — so the frame check is asserted on its own.
    let text = "x".repeat(MAX_FRAME_LEN - 1024);
    let result = hexput_exec::Value::String(text.as_str().into());
    assert_eq!(hexput_exec::wire::check_result(&result), Ok(()));
    let envelope = Envelope::new(
        CorrelationId(1),
        MessageType::Result,
        Value::Map(vec![(s("value"), hexput_exec::wire::to_wire(&result))]),
    );
    let body = encode(&envelope).unwrap();
    encode_frame(&body).expect("its frame is within the maximum");
    let body = failure(&payload("return t;", vec![("t", s(&text))]));
    assert_eq!(body.code, "budget.output_size_exceeded");
}

#[test]
fn a_result_certain_to_exceed_a_frame_is_past_the_output_size_budget_first() {
    // 2^25 bytes of string: twice the maximum frame, and far past the 1 MiB output size budget,
    // which is charged before the frame is checked.
    let body = failure(&payload(
        "let t = \"x\"; let i = 0; while (i < 25) { t = t + t; i = i + 1; }; return t;",
        vec![],
    ));
    assert_eq!(body.code, "budget.output_size_exceeded");
    let result = hexput_exec::Value::String("x".repeat(MAX_FRAME_LEN + 1).into());
    assert_eq!(
        hexput_exec::wire::check_result(&result),
        Err(hexput_exec::wire::Unsendable::TooLarge)
    );
}

#[test]
fn a_result_sharing_one_collection_exponentially_is_refused_without_expanding_it() {
    // Small in the execution — each level shares the one below — but 2^60 leaves on the wire.
    // Neither the output size nor the frame check expands it.
    let body = failure(&payload(
        "let a = [1]; let i = 0; while (i < 60) { a = [a, a]; i = i + 1; }; return a;",
        vec![],
    ));
    assert_eq!(body.code, "budget.output_size_exceeded");
    let mut shared = hexput_exec::Value::Null;
    for _ in 0..60 {
        shared = hexput_exec::Value::Array(hexput_interpreter::Array::from_values(vec![
            shared.clone(),
            shared,
        ]));
    }
    assert_eq!(
        hexput_exec::wire::check_result(&shared),
        Err(hexput_exec::wire::Unsendable::TooLarge)
    );
}

#[test]
fn a_function_result_is_the_interpreter_type_error() {
    let body = failure(&payload("fn f() { return 1; }; return f;", vec![]));
    assert_eq!(body.code, "type.function_result");
}

#[test]
fn integer_values_are_encoded_as_integers() {
    // Pinned so an SDK can rely on it: `3` arrives as a MessagePack integer, not a float.
    let value = run("return 3;", vec![]);
    assert!(matches!(value, Value::Integer(i) if i == Integer::from(3)));
}

#[test]
fn an_invalid_utf8_string_off_the_wire_is_refused() {
    // Built as raw MessagePack: the variable `t` is a str8 of two bytes that are not UTF-8. The
    // Daemon's codec hands such a str on as binary; either way it is refused, naming the path.
    let mut bytes = vec![0x83];
    for (key, value) in [("id", &[0x01][..]), ("type", &b"\xaeExecutionStart"[..])] {
        bytes.extend(rmp_serde::to_vec(key).unwrap());
        bytes.extend(value);
    }
    bytes.extend(rmp_serde::to_vec("payload").unwrap());
    bytes.push(0x82);
    bytes.extend(rmp_serde::to_vec("source").unwrap());
    bytes.extend(rmp_serde::to_vec("return t;").unwrap());
    bytes.extend(rmp_serde::to_vec("variables").unwrap());
    bytes.push(0x81);
    bytes.extend(rmp_serde::to_vec("t").unwrap());
    bytes.extend([0xd9, 0x02, 0xff, 0xfe]);
    let envelope = decode(&bytes).unwrap();
    let body = failure(&envelope.payload);
    assert_eq!(body.code, "protocol.invalid_payload");
    assert!(body.message.contains("`variables.t`"), "{}", body.message);
}

/// Story 3.2: the registrations' grants reach the Executor. Story 3.3: a call without a blanket
/// grant asks the per-call handler, and when no answer can come — here the connection is gone —
/// it is refused with the same error as an unregistered name.
#[test]
fn a_function_registered_without_a_grant_is_a_capability_error_when_no_handler_answers() {
    let source = "return getOrder(1);";
    let body = *direct_execution_registering(
        &payload(source, vec![]),
        vec![("getOrder".to_owned(), false)],
    )
    .expect_err("refused before anything is sent");
    assert_eq!(body.code, "capability.unknown_function");
    let unregistered = failure(&payload(source, vec![]));
    assert_eq!(body, unregistered, "the same error as an unregistered name");
}
