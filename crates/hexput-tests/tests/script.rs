//! Story 2.6: Direct Execution — the `ExecutionStart` payload, the wire↔value conversion both
//! ways, and the result guards.

use hexput_port::{
    CorrelationId, Envelope, ErrorBody, MAX_FRAME_LEN, MAX_NESTING_DEPTH, MessageType, Setting,
    Settings, Value, decode, encode, encode_frame,
};
use hexput_script::{AstCache, MAX_MODIFICATION_DEPTH, MAX_RESULT_DEPTH, Registration};
use rmpv::Integer;
use std::sync::Arc;

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
    direct_execution_configured(payload, registrations, Settings::new())
}

/// [`direct_execution_registering`] for a Session whose Config sets `settings` (Story 3.7).
fn direct_execution_configured(
    payload: &Value,
    registrations: Vec<(String, bool)>,
    settings: Settings,
) -> Result<Value, Box<ErrorBody>> {
    direct_execution_with(
        payload,
        registrations.into_iter().map(Registration::from).collect(),
        settings,
    )
}

/// [`direct_execution_configured`] with any registrations — Registered Methods too (Story 3.12).
fn direct_execution_with(
    payload: &Value,
    registrations: Vec<Registration>,
    settings: Settings,
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
        settings,
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

/// Whether the result `value` may be sent, by the check production runs: `result_to_wire`.
fn result_check(value: &hexput_exec::Value) -> Result<(), hexput_exec::wire::Unsendable> {
    hexput_exec::wire::result_to_wire(&hexput_exec::Held::plain(value.clone())).map(drop)
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
        (
            Value::Nil,
            "the `ExecutionStart` payload is missing `source` and `variables`",
        ),
        (
            Value::Map(vec![]),
            "the `ExecutionStart` payload is missing `source` and `variables`",
        ),
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
    // 10 000 levels: far past the limit, and well within the CPU time budget on a debug build
    // (100 000 came within a tenth of it).
    for n in [MAX_RESULT_DEPTH + 1, 10_000] {
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
    assert_eq!(result_check(&result), Ok(()));
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
        result_check(&result),
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
        result_check(&shared),
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

// --- Story 3.7: limits from the Session's Config, overridden per execution ---

/// An `ExecutionStart` payload running `source` with no variables and `overrides`.
fn overridden(source: &str, overrides: Value) -> Value {
    map(vec![
        ("source", s(source)),
        ("variables", map(vec![])),
        ("overrides", overrides),
    ])
}

/// Settings with `setting` set to `value`.
fn setting(setting: Setting, value: u64) -> Settings {
    let mut settings = Settings::new();
    settings.set(setting, value).unwrap();
    settings
}

fn budget(key: &str, value: Value) -> Value {
    map(vec![("budget", map(vec![(key, value)]))])
}

/// `getOrder` registered with a blanket grant; the call table is gone, so a call that is sent
/// fails with `host.no_reply`.
fn get_order() -> Vec<(String, bool)> {
    vec![("getOrder".to_owned(), true)]
}

#[test]
fn absent_or_nil_overrides_set_nothing() {
    for payload in [
        payload("return 1;", vec![]),
        overridden("return 1;", Value::Nil),
        overridden("return 1;", map(vec![])),
    ] {
        assert_eq!(
            direct_execution(&payload).unwrap(),
            map(vec![("value", int(1))])
        );
    }
}

#[test]
fn the_config_limit_is_enforced_and_an_override_changes_it_for_one_execution() {
    let config = setting(Setting::RpcCalls, 0);
    let call = payload("return getOrder(1);", vec![]);
    // Config: no host calls at all, so nothing is sent.
    let body = *direct_execution_configured(&call, get_order(), config).unwrap_err();
    assert_eq!(body.code, "budget.rpc_calls_exceeded");
    // Overridden for this execution: the call is made (and, with no connection, unanswered).
    let raised = overridden("return getOrder(1);", budget("rpc_calls", int(1)));
    let body = *direct_execution_configured(&raised, get_order(), config).unwrap_err();
    assert_eq!(body.code, "host.no_reply");
    // The Config itself is unchanged: the next execution is back under it.
    let body = *direct_execution_configured(&call, get_order(), config).unwrap_err();
    assert_eq!(body.code, "budget.rpc_calls_exceeded");
}

#[test]
fn an_override_can_lower_a_limit_too() {
    let body = failure(&overridden(
        "return \"a long enough result\";",
        budget("output_size_bytes", int(8)),
    ));
    assert_eq!(body.code, "budget.output_size_exceeded");
    assert!(body.message.contains("8 bytes"), "{}", body.message);
}

#[test]
fn the_argument_depth_is_the_one_in_force_and_its_error_says_so() {
    let source = "return getOrder([[[1]]]);";
    let config = setting(Setting::ArgumentDepth, 2);
    let body =
        *direct_execution_configured(&payload(source, vec![]), get_order(), config).unwrap_err();
    assert_eq!(body.code, "depth.argument_too_deep");
    assert!(
        body.message.contains("more than 2 arrays"),
        "{}",
        body.message
    );
    // An override raises it for this execution: the argument is sent.
    let raised = overridden(source, map(vec![("argument_depth", int(3))]));
    let body = *direct_execution_configured(&raised, get_order(), config).unwrap_err();
    assert_eq!(body.code, "host.no_reply");
}

#[test]
fn a_bad_override_is_refused_naming_its_path_and_nothing_runs() {
    let cases = [
        (
            budget("cpu_time_ms", int(0)),
            "`overrides.budget.cpu_time_ms` must be an integer from 1 to 60000; found 0",
        ),
        (
            budget("rpc_calls", s("5")),
            "`overrides.budget.rpc_calls` must be an integer from 0 to 100000; found a string",
        ),
        (
            budget("rpc_calls", Value::F64(1.0)),
            "`overrides.budget.rpc_calls` must be an integer from 0 to 100000; found a float",
        ),
        (
            budget("cpu", int(1)),
            "`overrides.budget.cpu` is not a known setting",
        ),
        (
            map(vec![("argument_depth", int(65))]),
            "`overrides.argument_depth` must be an integer from 1 to 64; found 65",
        ),
        (Value::Array(vec![]), "`overrides` is not a map"),
    ];
    for (overrides, expected) in cases {
        // The Script would call the host (and fail with no reply) if it ran.
        let body = *direct_execution_registering(
            &overridden("return getOrder(1);", overrides),
            get_order(),
        )
        .unwrap_err();
        assert_eq!(body.code, "protocol.invalid_payload", "{expected}");
        assert_eq!(body.message, expected);
    }
    let body = failure(&map(vec![
        ("source", s("return 1;")),
        ("variables", map(vec![])),
        ("overrides", map(vec![])),
        ("overrides", map(vec![])),
    ]));
    assert!(
        body.message.contains("repeats the key `overrides`"),
        "{}",
        body.message
    );
}

// --- Story 3.10: the static check mode ---

/// Settings with the check mode `mode`.
fn checked(mode: hexput_port::CheckMode) -> Settings {
    let mut settings = Settings::new();
    settings.set_check(mode);
    settings
}

fn codes(findings: &[ErrorBody]) -> Vec<&str> {
    findings.iter().map(|f| f.code.as_str()).collect()
}

#[test]
fn under_error_an_error_finding_rejects_before_anything_runs() {
    use hexput_port::CheckMode;
    // Were `getOrder(1)` reached, the gone call table would make it `host.no_reply`.
    let body = *direct_execution_configured(
        &payload("getOrder(1); let unused = 1; return x;", vec![]),
        get_order(),
        checked(CheckMode::Error),
    )
    .expect_err("rejected");
    assert_eq!(body.code, "reference.undeclared_identifier");
    assert_eq!(body.severity, "error");
    assert_eq!(
        codes(&body.findings),
        [
            "reference.unused_variable",
            "reference.undeclared_identifier"
        ]
    );
    // Off (the default): the Script runs and reaches the host call.
    let body = *direct_execution_configured(
        &payload("getOrder(1); let unused = 1; return x;", vec![]),
        get_order(),
        Settings::new(),
    )
    .expect_err("the host call fails");
    assert_eq!(body.code, "host.no_reply");
    assert!(body.findings.is_empty());
}

#[test]
fn under_warn_the_result_carries_value_then_findings() {
    use hexput_port::CheckMode;
    let reply = direct_execution_configured(
        &payload("let unused = 1; return 2;", vec![]),
        Vec::new(),
        checked(CheckMode::Warn),
    )
    .unwrap();
    let Value::Map(fields) = reply else {
        panic!("a map")
    };
    assert_eq!(fields[0], (s("value"), int(2)));
    assert_eq!(fields[1].0, s("findings"));
    let findings: Vec<ErrorBody> = rmpv::ext::from_value(fields[1].1.clone()).unwrap();
    assert_eq!(codes(&findings), ["reference.unused_variable"]);
    assert_eq!(fields.len(), 2);
}

#[test]
fn an_override_sets_the_mode_for_one_execution() {
    let source = "if (false) { return x; }; return 1;";
    let body = *direct_execution(&overridden(source, map(vec![("check", s("error"))])))
        .expect_err("checked");
    assert_eq!(body.code, "reference.undeclared_identifier");
    // An override of `off` lifts a Config's `error`.
    let reply = direct_execution_configured(
        &overridden(source, map(vec![("check", s("off"))])),
        Vec::new(),
        checked(hexput_port::CheckMode::Error),
    )
    .unwrap();
    assert_eq!(reply, map(vec![("value", int(1))]));
}

#[test]
fn the_check_sees_starting_variables_registrations_and_toggles() {
    use hexput_port::CheckMode;
    // Starting variables and every registration name, blanket or not, are known.
    let reply = direct_execution_configured(
        &payload("return n;", vec![("n", int(3))]),
        vec![("askFirst".to_owned(), false)],
        checked(CheckMode::Error),
    )
    .unwrap();
    assert_eq!(reply, map(vec![("value", int(3))]));
    let body = *direct_execution_configured(
        &payload("if (false) { askFirst(1); other(2); }; return 1;", vec![]),
        vec![("askFirst".to_owned(), false)],
        checked(CheckMode::Error),
    )
    .expect_err("an unregistered call");
    assert_eq!(body.code, "capability.unknown_function");
    assert_eq!(body.findings.len(), 1);
    // The effective toggles become the policy.
    let body = *direct_execution(&overridden(
        "if (false) { return [1]; }; return 1;",
        map(vec![
            ("check", s("error")),
            (
                "features",
                map(vec![("array_literals", Value::Boolean(false))]),
            ),
        ]),
    ))
    .expect_err("a disabled construct");
    assert_eq!(body.code, "policy.construct_disabled");
    assert_eq!(
        body.message,
        "`[ … ]` is disabled by policy (`features.array_literals`)"
    );
}

#[test]
fn under_warn_a_runtime_failure_carries_the_findings_too() {
    use hexput_port::CheckMode;
    let body = *direct_execution_configured(
        &payload("let unused = 1; return x;", vec![]),
        Vec::new(),
        checked(CheckMode::Warn),
    )
    .expect_err("the runtime fails");
    assert_eq!(body.code, "reference.undeclared_identifier");
    assert_eq!(
        codes(&body.findings),
        [
            "reference.unused_variable",
            "reference.undeclared_identifier"
        ]
    );
    // Off: the same failure, no findings.
    let body = *direct_execution(&payload("let unused = 1; return x;", vec![])).unwrap_err();
    assert!(body.findings.is_empty());
}

/// A Script declaring `n` unused locals, then `tail`.
fn unused_locals(n: usize, tail: &str) -> String {
    let mut source: String = (0..n).map(|i| format!("let u{i} = 1; ")).collect();
    source.push_str(tail);
    source
}

#[test]
fn at_most_max_findings_are_kept_in_source_order() {
    use hexput_port::CheckMode;
    use hexput_script::MAX_FINDINGS;
    assert_eq!(MAX_FINDINGS, 100);
    let reply = direct_execution_configured(
        &payload(&unused_locals(150, "return 1;"), vec![]),
        Vec::new(),
        checked(CheckMode::Warn),
    )
    .unwrap();
    let Value::Map(fields) = reply else {
        panic!("a map")
    };
    let findings: Vec<ErrorBody> = rmpv::ext::from_value(fields[1].1.clone()).unwrap();
    assert_eq!(findings.len(), MAX_FINDINGS);
    let offsets: Vec<u64> = findings.iter().map(|f| f.span.unwrap().offset).collect();
    assert!(offsets.windows(2).all(|w| w[0] < w[1]), "source order");

    // A rejection whose first error lies past the cap keeps it, as the last finding.
    let body = *direct_execution_configured(
        &payload(&unused_locals(150, "return x;"), vec![]),
        Vec::new(),
        checked(CheckMode::Error),
    )
    .expect_err("rejected");
    assert_eq!(body.code, "reference.undeclared_identifier");
    assert_eq!(body.findings.len(), MAX_FINDINGS);
    let last = body.findings.last().unwrap();
    assert_eq!(
        (&last.code, &last.message, last.span),
        (&body.code, &body.message, body.span)
    );
    assert!(
        body.findings[..MAX_FINDINGS - 1]
            .iter()
            .all(|f| f.code == "reference.unused_variable")
    );
}

#[test]
fn findings_that_would_not_fit_a_frame_are_left_out_and_the_value_wins() {
    use hexput_port::CheckMode;
    let mut settings = checked(CheckMode::Warn);
    settings
        .set(Setting::OutputSizeBytes, MAX_FRAME_LEN as u64)
        .unwrap();
    let run = |len: usize| {
        let text = "x".repeat(len);
        direct_execution_configured(
            &payload("let unused = 1; return t;", vec![("t", s(&text))]),
            Vec::new(),
            settings,
        )
        .unwrap()
    };
    let Value::Map(fields) = run(MAX_FRAME_LEN - 4096) else {
        panic!("a map")
    };
    assert_eq!(fields.len(), 2, "room for the findings");
    let Value::Map(fields) = run(MAX_FRAME_LEN - 100) else {
        panic!("a map")
    };
    assert_eq!(fields.len(), 1, "exactly `{{value}}`");
    assert_eq!(fields[0].0, s("value"));
}

// --- Story 3.11: a result's Value Secret holders count towards its wire depth ---

/// A starting variable nested `levels` arrays deep, its outer `held` levels each in a
/// Backend-supplied holder: `levels + held` levels on the wire.
fn nested_held(levels: usize, held: usize) -> Value {
    let mut value = Value::Nil;
    for level in (0..levels).rev() {
        value = Value::Array(vec![value]);
        if level < held {
            value = map(vec![
                ("__secret", map(vec![("ref", s(&format!("r{level}")))])),
                ("value", value),
            ]);
        }
    }
    value
}

#[test]
fn holders_that_take_a_result_past_the_depth_limit_make_it_too_deep() {
    let levels = 100;
    // The Script's own nesting fits; with its holders the result is one level too deep.
    let past = nested_held(levels, MAX_RESULT_DEPTH - levels + 1);
    let body = failure(&payload("return v;", vec![("v", past)]));
    assert_eq!(body.code, "protocol.result_too_deep");
    // At the limit it is sent, and a peer applying the same frame limit reads it back.
    let at = nested_held(levels, MAX_RESULT_DEPTH - levels);
    let value = run("return v;", vec![("v", at.clone())]);
    assert_eq!(
        value, at,
        "every Backend-supplied holder goes back as it came"
    );
    let envelope = Envelope::new(
        CorrelationId(1),
        MessageType::Result,
        map(vec![("value", value)]),
    );
    let bytes = encode(&envelope).unwrap();
    assert_eq!(decode(&bytes).unwrap(), envelope);
}

// --- Story 3.12: Registered Methods and the static check ---

/// `{name: "a"}` held as a `User`.
fn keyed_user() -> Value {
    map(vec![
        ("__secret", map(vec![("ref", s("r1")), ("key", s("User"))])),
        ("value", map(vec![("name", s("a"))])),
    ])
}

#[test]
fn the_check_rejects_overriding_a_method_of_a_keyed_starting_variable() {
    use hexput_port::CheckMode;
    let body = *direct_execution_with(
        &payload("u.save = 1; return 1;", vec![("u", keyed_user())]),
        vec![Registration::method("User", "save", true)],
        checked(CheckMode::Error),
    )
    .expect_err("rejected at submission");
    assert_eq!(body.code, "capability.method_override");
    assert_eq!(codes(&body.findings), ["capability.method_override"]);
    // A method under another key, or a plain starting variable, is nothing to override. Writing
    // the Backend's `r1` object is reported back (Story 3.13); the plain one has no Reference ID.
    let written = map(vec![
        ("value", int(1)),
        (
            "modifications",
            Value::Array(vec![map(vec![
                ("ref", s("r1")),
                ("value", map(vec![("name", s("a")), ("save", int(1))])),
            ])]),
        ),
    ]);
    for (variable, key, expected) in [
        (keyed_user(), "Order", written),
        (map(vec![]), "User", map(vec![("value", int(1))])),
    ] {
        let reply = direct_execution_with(
            &payload("u.save = 1; return u.save;", vec![("u", variable)]),
            vec![Registration::method(key, "save", true)],
            checked(CheckMode::Error),
        )
        .unwrap();
        assert_eq!(reply, expected);
    }
    // With the check off, the runtime refuses it all the same.
    let body = *direct_execution_with(
        &payload("u.save = 1; return 1;", vec![("u", keyed_user())]),
        vec![Registration::method("User", "save", true)],
        Settings::new(),
    )
    .expect_err("refused at runtime");
    assert_eq!(body.code, "capability.method_override");
    assert!(body.findings.is_empty());
}

#[test]
fn a_method_is_no_callable_name_for_the_check() {
    use hexput_port::CheckMode;
    // `save` is a method under `User`: a bare `save()` can never reach it.
    let body = *direct_execution_with(
        &payload("if (false) { save(); }; return 1;", vec![]),
        vec![Registration::method("User", "save", true)],
        checked(CheckMode::Error),
    )
    .expect_err("an unknown call");
    assert_eq!(body.code, "capability.unknown_function");
    // Calling it on a keyed value is no finding; the call itself runs (and here, with the
    // connection gone, gets no reply).
    let body = *direct_execution_with(
        &payload("return u.save();", vec![("u", keyed_user())]),
        vec![Registration::method("User", "save", true)],
        checked(CheckMode::Error),
    )
    .expect_err("no reply");
    assert_eq!(body.code, "host.no_reply");
    assert!(body.findings.is_empty());
}

#[test]
fn a_keyed_scalar_starting_variable_is_no_override_finding() {
    use hexput_port::CheckMode;
    let n = map(vec![
        ("__secret", map(vec![("ref", s("r2")), ("key", s("Num"))])),
        ("value", int(5)),
    ]);
    let body = *direct_execution_with(
        &payload("n.double = 1; return 0;", vec![("n", n)]),
        vec![Registration::method("Num", "double", true)],
        checked(CheckMode::Error),
    )
    .expect_err("the runtime refuses it");
    assert_eq!(body.code, "type.invalid_property_access");
    assert!(body.findings.is_empty(), "{:?}", body.findings);
}

// --- Story 3.13: a result's modifications ---

#[test]
fn a_modification_nested_past_the_frame_makes_the_result_too_deep() {
    let n = map(vec![
        ("__secret", map(vec![("ref", s("r1"))])),
        ("value", Value::from(0)),
    ]);
    let body = failure(&payload(
        "n = v; return 0;",
        vec![
            ("n", n.clone()),
            ("v", nested_held(MAX_MODIFICATION_DEPTH + 1, 0)),
        ],
    ));
    assert_eq!(body.code, "protocol.result_too_deep");
    assert!(body.message.contains("modifications"), "{}", body.message);
    // At the limit it is sent, and a peer applying the same frame limit reads it back.
    let at = nested_held(MAX_MODIFICATION_DEPTH, 0);
    let reply = direct_execution(&payload(
        "n = v; return 0;",
        vec![("n", n), ("v", at.clone())],
    ))
    .unwrap();
    assert_eq!(
        reply,
        map(vec![
            ("value", Value::from(0)),
            (
                "modifications",
                Value::Array(vec![map(vec![("ref", s("r1")), ("value", at)])]),
            ),
        ])
    );
    let envelope = Envelope::new(CorrelationId(1), MessageType::Result, reply);
    let bytes = encode(&envelope).unwrap();
    assert_eq!(decode(&bytes).unwrap(), envelope);
}

// --- Story 4.1: register a Script once for repeated use ---

/// Register `payload` for `client` into `cache` on a runtime of its own, with `registrations` and
/// `settings`. The check, if its mode asks for one, runs here and nowhere else (AD-8).
fn registered(
    payload: &Value,
    client: hexput_shared::ids::ClientId,
    cache: &Arc<AstCache>,
    registrations: Vec<(String, bool)>,
    settings: Settings,
) -> Result<Value, Box<ErrorBody>> {
    registered_as(
        payload,
        client,
        cache,
        registrations.into_iter().map(Registration::from).collect(),
        settings,
    )
}

/// [`registered`] with any [`Registration`]s, Registered Methods included. The Session is open
/// first, as `init` makes it (Story 4.3): a registration for a Client ID that was never opened is
/// refused, see [`register_unopened`].
fn registered_as(
    payload: &Value,
    client: hexput_shared::ids::ClientId,
    cache: &Arc<AstCache>,
    registrations: Vec<Registration>,
    settings: Settings,
) -> Result<Value, Box<ErrorBody>> {
    cache.open(client);
    register_unopened(payload, client, cache, registrations, settings)
}

/// [`registered_as`] without opening the Session first: exactly what a registration sees when its
/// Client ID was never opened, or has ended (Story 4.3).
fn register_unopened(
    payload: &Value,
    client: hexput_shared::ids::ClientId,
    cache: &Arc<AstCache>,
    registrations: Vec<Registration>,
    settings: Settings,
) -> Result<Value, Box<ErrorBody>> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
        .unwrap();
    runtime.block_on(hexput_script::code_register(
        payload.clone(),
        client,
        Arc::clone(cache),
        registrations,
        settings,
    ))
}

/// A `CodeRegister` payload: `source`, and `variables` (names only) when given.
fn registration(source: &str, variables: Option<Vec<&str>>) -> Value {
    let mut fields = vec![("source", s(source))];
    if let Some(variables) = variables {
        fields.push((
            "variables",
            Value::Array(variables.into_iter().map(s).collect()),
        ));
    }
    map(fields)
}

/// The keys of a successful reply, in order.
fn reply_keys(reply: &Value) -> Vec<&str> {
    let Value::Map(fields) = reply else {
        panic!("a Result payload is a map: {reply:?}");
    };
    fields
        .iter()
        .map(|(key, _)| key.as_str().unwrap())
        .collect()
}

/// The `handle` of a successful registration, asserting it is exactly 16 lowercase hex characters.
fn handle_in(reply: &Value) -> hexput_shared::ids::ScriptHandle {
    let Value::Map(fields) = reply else {
        panic!("a Result payload is a map");
    };
    let (key, value) = &fields[0];
    assert_eq!(key.as_str(), Some("handle"));
    let text = value.as_str().expect("a string handle");
    assert_eq!(text.len(), 16, "{text}");
    assert!(
        text.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')),
        "{text}"
    );
    text.parse().unwrap()
}

fn client(byte: u8) -> hexput_shared::ids::ClientId {
    hexput_shared::ids::ClientId::from_bytes([byte; 16])
}

#[test]
fn a_registration_parses_once_into_the_cache_under_its_client_id() {
    let cache = Arc::new(AstCache::with_capacity(10_000));
    let reply = registered(
        &registration("return 1 + x;", None),
        client(1),
        &cache,
        vec![],
        Settings::new(),
    )
    .unwrap();
    assert_eq!(reply_keys(&reply), ["handle"]);
    let handle = handle_in(&reply);
    let program = cache
        .get(client(1), handle)
        .expect("the handle names the entry");
    assert_eq!(*program, hexput_parser::parse("return 1 + x;").unwrap());
    assert_eq!(cache.entry_count(), 1);
}

#[test]
fn every_registration_of_the_same_source_gets_its_own_handle() {
    let cache = Arc::new(AstCache::with_capacity(10_000));
    let payload = registration("return 1;", None);
    let first =
        handle_in(&registered(&payload, client(1), &cache, vec![], Settings::new()).unwrap());
    let second =
        handle_in(&registered(&payload, client(1), &cache, vec![], Settings::new()).unwrap());
    assert_ne!(first, second);
    assert_eq!(cache.entry_count(), 2);
    assert!(cache.get(client(1), first).is_some());
    assert!(cache.get(client(1), second).is_some());
}

#[test]
fn the_first_handle_of_a_cache_is_0000000000000001() {
    let cache = Arc::new(AstCache::with_capacity(10_000));
    let reply = registered(
        &registration("return 1;", None),
        client(1),
        &cache,
        vec![],
        Settings::new(),
    )
    .unwrap();
    assert_eq!(
        reply.as_map().unwrap()[0].1.as_str(),
        Some("0000000000000001")
    );
}

#[test]
fn a_parse_failure_is_the_parser_diagnostic_and_creates_no_entry() {
    let cache = Arc::new(AstCache::with_capacity(10_000));
    let body = *registered(
        &registration("let = ;", None),
        client(1),
        &cache,
        vec![],
        Settings::new(),
    )
    .expect_err("a parse failure");
    assert!(body.code.starts_with("syntax."), "{}", body.code);
    assert!(body.span.is_some(), "the parser's span");
    assert_eq!(cache.entry_count(), 0);
}

#[test]
fn a_malformed_registration_is_refused_naming_the_key_and_creates_no_entry() {
    let cache = Arc::new(AstCache::with_capacity(10_000));
    let cases: Vec<(Value, &[&str])> = vec![
        (Value::Nil, &["`source`"]),
        (map(vec![]), &["`source`"]),
        (map(vec![("source", int(1))]), &["`source`"]),
        (map(vec![("source", s("1;")), ("x", int(1))]), &["`x`"]),
        (
            map(vec![("source", s("1;")), ("overrides", map(vec![]))]),
            &["`overrides`"],
        ),
        (
            map(vec![("source", s("1;")), ("variables", map(vec![]))]),
            &["`variables`"],
        ),
        (
            map(vec![
                ("source", s("1;")),
                ("variables", Value::Array(vec![s("a"), s("a")])),
            ]),
            &["variables[1]", "`a`", "\"a\""],
        ),
        (
            map(vec![
                ("source", s("1;")),
                ("variables", Value::Array(vec![s("1a")])),
            ]),
            &["variables[0]", "`1a`", "\"1a\""],
        ),
    ];
    for (payload, mentions) in cases {
        let body =
            *registered(&payload, client(1), &cache, vec![], Settings::new()).expect_err("refused");
        assert_eq!(body.code, "protocol.invalid_payload", "{payload:?}");
        assert!(
            mentions.iter().any(|m| body.message.contains(m)),
            "{payload:?} refused as {:?}, naming none of {mentions:?}",
            body.message
        );
    }
    assert_eq!(cache.entry_count(), 0);
}

#[test]
fn under_warn_the_registration_is_cached_and_its_findings_come_with_the_handle() {
    use hexput_port::CheckMode;
    let cache = Arc::new(AstCache::with_capacity(10_000));
    let reply = registered(
        &registration("let unused = 1; return x;", Some(vec!["x"])),
        client(1),
        &cache,
        vec![],
        checked(CheckMode::Warn),
    )
    .unwrap();
    assert_eq!(reply_keys(&reply), ["handle", "findings"]);
    handle_in(&reply);
    let findings: Vec<ErrorBody> =
        rmpv::ext::from_value(reply.as_map().unwrap()[1].1.clone()).unwrap();
    assert_eq!(codes(&findings), ["reference.unused_variable"]);
    assert_eq!(findings[0].severity, "warning");
    assert_eq!(cache.entry_count(), 1);
}

#[test]
fn under_error_an_error_finding_rejects_the_registration_with_every_finding() {
    use hexput_port::CheckMode;
    let cache = Arc::new(AstCache::with_capacity(10_000));
    let body = *registered(
        &registration("let unused = 1; return y;", None),
        client(1),
        &cache,
        vec![],
        checked(CheckMode::Error),
    )
    .expect_err("rejected");
    assert_eq!(body.code, "reference.undeclared_identifier");
    assert_eq!(body.severity, "error");
    assert_eq!(
        codes(&body.findings),
        [
            "reference.unused_variable",
            "reference.undeclared_identifier"
        ]
    );
    assert_eq!(cache.entry_count(), 0);
}

#[test]
fn under_error_naming_the_starting_variables_is_accepted_without_findings() {
    use hexput_port::CheckMode;
    let cache = Arc::new(AstCache::with_capacity(10_000));
    let reply = registered(
        &registration("return y;", Some(vec!["y"])),
        client(1),
        &cache,
        vec![],
        checked(CheckMode::Error),
    )
    .unwrap();
    assert_eq!(reply_keys(&reply), ["handle"]);
    assert_eq!(cache.entry_count(), 1);
}

#[test]
fn with_the_check_off_no_pass_runs() {
    let cache = Arc::new(AstCache::with_capacity(10_000));
    let reply = registered(
        &registration("return y;", None),
        client(1),
        &cache,
        vec![],
        Settings::new(),
    )
    .unwrap();
    assert_eq!(reply_keys(&reply), ["handle"]);
    assert_eq!(cache.entry_count(), 1);
}

#[test]
fn the_registrations_are_the_callable_names_the_check_knows() {
    use hexput_port::CheckMode;
    let cache = Arc::new(AstCache::with_capacity(10_000));
    let reply = registered(
        &registration("if (false) { askFirst(1); }; return 1;", None),
        client(1),
        &cache,
        vec![("askFirst".to_owned(), false)],
        checked(CheckMode::Error),
    )
    .unwrap();
    assert_eq!(reply_keys(&reply), ["handle"]);
    let body = *registered(
        &registration("if (false) { other(2); }; return 1;", None),
        client(1),
        &cache,
        vec![("askFirst".to_owned(), false)],
        checked(CheckMode::Error),
    )
    .expect_err("an unregistered call");
    assert_eq!(body.code, "capability.unknown_function");
    assert_eq!(cache.entry_count(), 1);
}

#[test]
fn a_method_is_no_callable_name_for_the_registration_check() {
    use hexput_port::CheckMode;
    // `save` is a method under `User`: a bare `save(1)` can never reach it.
    let cache = Arc::new(AstCache::with_capacity(10_000));
    let body = *registered_as(
        &registration("if (false) { save(1); }; return 1;", None),
        client(1),
        &cache,
        vec![Registration::method("User", "save", true)],
        checked(CheckMode::Error),
    )
    .expect_err("an unknown call");
    assert_eq!(body.code, "capability.unknown_function");
    assert_eq!(cache.entry_count(), 0);
}

#[test]
fn registration_findings_that_would_not_fit_a_frame_are_left_out_and_the_handle_wins() {
    use hexput_port::CheckMode;
    // `MAX_FINDINGS` unused locals, each named long enough that their findings outgrow a frame.
    let long = MAX_FRAME_LEN / hexput_script::MAX_FINDINGS + 1024;
    let source: String = (0..hexput_script::MAX_FINDINGS)
        .map(|n| format!("let {}{n} = 1;\n", "v".repeat(long)))
        .chain(std::iter::once("return 1;".to_owned()))
        .collect();
    let cache = Arc::new(AstCache::with_capacity(10_000));
    let reply = registered(
        &registration(&source, None),
        client(1),
        &cache,
        vec![],
        checked(CheckMode::Warn),
    )
    .unwrap();
    assert_eq!(reply_keys(&reply), ["handle"], "exactly `{{handle}}`");
    assert_eq!(cache.entry_count(), 1);
}

#[test]
fn a_large_warn_result_keeps_at_most_max_findings() {
    use hexput_port::CheckMode;
    let cache = Arc::new(AstCache::with_capacity(10_000));
    let reply = registered(
        &registration(&unused_locals(150, "return 1;"), None),
        client(1),
        &cache,
        vec![],
        checked(CheckMode::Warn),
    )
    .unwrap();
    let findings: Vec<ErrorBody> =
        rmpv::ext::from_value(reply.as_map().unwrap()[1].1.clone()).unwrap();
    assert_eq!(findings.len(), hexput_script::MAX_FINDINGS);
    assert!(
        codes(&findings)
            .iter()
            .all(|c| *c == "reference.unused_variable")
    );
    assert_eq!(cache.entry_count(), 1);
}

#[test]
fn two_clients_registering_the_same_source_are_scoped_apart() {
    let cache = Arc::new(AstCache::with_capacity(10_000));
    let payload = registration("return 1 + x;", None);
    let a = handle_in(&registered(&payload, client(1), &cache, vec![], Settings::new()).unwrap());
    let b = handle_in(&registered(&payload, client(2), &cache, vec![], Settings::new()).unwrap());
    assert_ne!(a, b);
    assert!(cache.get(client(1), a).is_some());
    assert!(cache.get(client(2), b).is_some());
    assert!(cache.get(client(1), b).is_none());
    assert!(cache.get(client(2), a).is_none());
    assert_eq!(cache.handles(client(1)), vec![a]);
    assert_eq!(cache.handles(client(2)), vec![b]);
}

// --- Story 4.2: re-run a registered Script with fresh variables ---

/// Run `payload` as a Cached Execution for `client` against `cache`, on a runtime of its own, with
/// `registrations` and `settings`. Its call table is dropped at once, as in
/// [`direct_execution_with`], so a call that is sent fails with `host.no_reply`.
fn cached_with(
    payload: &Value,
    client: hexput_shared::ids::ClientId,
    cache: &Arc<AstCache>,
    registrations: Vec<Registration>,
    settings: Settings,
) -> Result<Value, Box<ErrorBody>> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
        .unwrap();
    let (calls, caller) = hexput_rpc::Calls::new();
    drop(calls);
    runtime.block_on(hexput_script::cached_execution(
        payload.clone(),
        client,
        cache,
        registrations,
        settings,
        caller,
    ))
}

/// [`cached_with`] for `client` with no registrations and the Daemon's default settings.
fn cached(
    payload: &Value,
    client: hexput_shared::ids::ClientId,
    cache: &Arc<AstCache>,
) -> Result<Value, Box<ErrorBody>> {
    cached_with(payload, client, cache, vec![], Settings::new())
}

/// A `CachedExecutionStart` payload running `handle` with `variables`, and `overrides` when given.
fn cached_payload(
    handle: hexput_shared::ids::ScriptHandle,
    variables: Vec<(&str, Value)>,
    overrides: Option<Value>,
) -> Value {
    let mut fields = vec![
        ("handle", s(&handle.to_string())),
        ("variables", map(variables)),
    ];
    if let Some(overrides) = overrides {
        fields.push(("overrides", overrides));
    }
    map(fields)
}

/// Register `source` for `client` into `cache` with no starting-variable names, returning its handle.
fn handle_of_registration(
    source: &str,
    client: hexput_shared::ids::ClientId,
    cache: &Arc<AstCache>,
) -> hexput_shared::ids::ScriptHandle {
    handle_in(
        &registered(
            &registration(source, None),
            client,
            cache,
            vec![],
            Settings::new(),
        )
        .unwrap(),
    )
}

/// The `value` of a successful Cached Execution, asserting its reply is exactly `{value}`.
fn cached_value(reply: &Value) -> Value {
    assert_eq!(reply_keys(reply), ["value"], "{reply:?}");
    reply.as_map().unwrap()[0].1.clone()
}

/// A Script with a counted loop, which a loops-off toggle refuses.
const COUNTED_LOOP: &str = "let n = 0; while (n < 2) { n = n + 1; }; return n;";

#[test]
fn a_registered_script_runs_with_fresh_variables_and_its_entry_is_untouched() {
    let cache = Arc::new(AstCache::with_capacity(10_000));
    let handle = handle_of_registration("return grade >= 50;", client(1), &cache);
    let reply = cached(
        &cached_payload(handle, vec![("grade", int(72))], None),
        client(1),
        &cache,
    )
    .unwrap();
    assert_eq!(cached_value(&reply), Value::Boolean(true));
    // The same handle, with other variables, gives the other result.
    let reply = cached(
        &cached_payload(handle, vec![("grade", int(12))], None),
        client(1),
        &cache,
    )
    .unwrap();
    assert_eq!(cached_value(&reply), Value::Boolean(false));
    assert_eq!(cache.entry_count(), 1, "a run neither inserts nor removes");
    assert_eq!(cache.handles(client(1)), vec![handle]);
}

#[test]
fn a_run_never_parses_so_a_registered_entry_is_the_one_it_runs() {
    let cache = Arc::new(AstCache::with_capacity(10_000));
    let handle = handle_of_registration("return 1 + x;", client(1), &cache);
    let before = cache.get(client(1), handle).expect("the entry");
    cached(
        &cached_payload(handle, vec![("x", int(2))], None),
        client(1),
        &cache,
    )
    .unwrap();
    let after = cache.get(client(1), handle).expect("still the entry");
    assert!(Arc::ptr_eq(&before, &after), "the same cached Arc<Program>");
    assert_eq!(*after, hexput_parser::parse("return 1 + x;").unwrap());
}

#[test]
fn a_mutated_object_does_not_bleed_into_the_next_run() {
    let cache = Arc::new(AstCache::with_capacity(10_000));
    let handle = handle_of_registration("o.n = o.n + 1; return o.n;", client(1), &cache);
    for _ in 0..2 {
        let reply = cached(
            &cached_payload(handle, vec![("o", map(vec![("n", int(1))]))], None),
            client(1),
            &cache,
        )
        .unwrap();
        assert_eq!(cached_value(&reply), int(2));
    }
}

#[test]
fn top_level_state_does_not_bleed_into_the_next_run() {
    let cache = Arc::new(AstCache::with_capacity(10_000));
    let handle = handle_of_registration("let c = 0; c = c + x; return c;", client(1), &cache);
    for _ in 0..2 {
        let reply = cached(
            &cached_payload(handle, vec![("x", int(5))], None),
            client(1),
            &cache,
        )
        .unwrap();
        assert_eq!(cached_value(&reply), int(5));
    }
}

#[test]
fn a_cached_run_replies_with_the_same_bytes_as_a_direct_execution() {
    let source = "let t = x * 2; return t;";
    let cache = Arc::new(AstCache::with_capacity(10_000));
    let handle = handle_of_registration(source, client(1), &cache);
    let cached_reply = cached(
        &cached_payload(handle, vec![("x", int(3))], None),
        client(1),
        &cache,
    )
    .unwrap();
    let direct_reply = direct_execution(&payload(source, vec![("x", int(3))])).unwrap();
    assert_eq!(cached_reply, direct_reply);
    let encode_result = |reply: Value| {
        encode(&Envelope::new(CorrelationId(1), MessageType::Result, reply)).unwrap()
    };
    assert_eq!(encode_result(cached_reply), encode_result(direct_reply));
}

#[test]
fn an_unknown_handle_is_answered_with_the_re_register_error_and_nothing_runs() {
    let cache = Arc::new(AstCache::with_capacity(10_000));
    handle_of_registration("return 1;", client(1), &cache);
    let unknown: hexput_shared::ids::ScriptHandle = "00000000000000ff".parse().unwrap();
    let body = *cached(&cached_payload(unknown, vec![], None), client(1), &cache)
        .expect_err("an unknown handle");
    assert_eq!(body.code, "protocol.unknown_script_handle");
    assert_eq!(body.category, "protocol");
    assert_eq!(body.severity, "error");
    assert!(body.span.is_none(), "a request error, not a Script error");
    assert!(
        body.message.contains("00000000000000ff"),
        "{}",
        body.message
    );
    assert!(body.message.contains("CodeRegister"), "{}", body.message);
    assert_eq!(cache.entry_count(), 1);
}

#[test]
fn handle_zero_is_well_formed_but_never_issued_so_it_is_unknown() {
    // Handles count from 1, so `0000000000000000` parses yet can name no entry: it is the
    // re-register error, never a malformed request.
    let cache = Arc::new(AstCache::with_capacity(10_000));
    handle_of_registration("return 1;", client(1), &cache);
    let body = *cached(
        &map(vec![
            ("handle", s("0000000000000000")),
            ("variables", map(vec![])),
        ]),
        client(1),
        &cache,
    )
    .expect_err("handle zero");
    assert_eq!(
        body.code, "protocol.unknown_script_handle",
        "{}",
        body.message
    );
    assert!(
        body.message.contains("0000000000000000"),
        "{}",
        body.message
    );
}

#[test]
fn another_sessions_handle_gets_the_same_error_shape_as_a_handle_never_issued() {
    let cache = Arc::new(AstCache::with_capacity(10_000));
    let theirs = handle_of_registration("return 1;", client(1), &cache);
    let foreign = *cached(&cached_payload(theirs, vec![], None), client(2), &cache)
        .expect_err("another Session's handle");
    let never = *cached(
        &cached_payload("00000000000000ff".parse().unwrap(), vec![], None),
        client(2),
        &cache,
    )
    .expect_err("a handle never issued");
    assert_eq!(foreign.code, "protocol.unknown_script_handle");
    assert_eq!(
        (&foreign.severity, &foreign.category, &foreign.span),
        (&never.severity, &never.category, &never.span)
    );
    assert_eq!(
        foreign.message.replace(&theirs.to_string(), "HANDLE"),
        never.message.replace("00000000000000ff", "HANDLE"),
        "the message names only the handle asked for"
    );
    assert_eq!(cache.entry_count(), 1);
}

#[test]
fn a_malformed_payload_is_refused_naming_the_key_and_runs_nothing() {
    let cache = Arc::new(AstCache::with_capacity(10_000));
    let handle = handle_of_registration("return 1;", client(1), &cache);
    let text = handle.to_string();
    let cases: Vec<(Value, &[&str])> = vec![
        (
            Value::Nil,
            &[
                "the `CachedExecutionStart` payload",
                "`handle`",
                "`variables`",
            ],
        ),
        (
            map(vec![]),
            &[
                "the `CachedExecutionStart` payload",
                "`handle`",
                "`variables`",
            ],
        ),
        (
            map(vec![("handle", int(1)), ("variables", map(vec![]))]),
            &["`handle`"],
        ),
        (
            map(vec![("handle", s("ABC")), ("variables", map(vec![]))]),
            &["`handle`", "ABC"],
        ),
        // A handle parses strictly: uppercase, padding or one character too many is a malformed
        // request (`invalid_payload`), never an unknown handle to re-register.
        (
            map(vec![
                ("handle", s("00000000000000FF")),
                ("variables", map(vec![])),
            ]),
            &["`handle`", "00000000000000FF"],
        ),
        (
            map(vec![
                ("handle", s(&format!(" {text} "))),
                ("variables", map(vec![])),
            ]),
            &["`handle`"],
        ),
        (
            map(vec![
                ("handle", s(&format!("{text}0"))),
                ("variables", map(vec![])),
            ]),
            &["`handle`"],
        ),
        (
            map(vec![("handle", s(&text))]),
            &["the `CachedExecutionStart` payload", "missing `variables`"],
        ),
        (
            map(vec![("handle", s(&text)), ("variables", int(1))]),
            &["`variables`"],
        ),
        (
            map(vec![
                ("handle", s(&text)),
                ("variables", map(vec![])),
                ("source", s("1;")),
            ]),
            &["the `CachedExecutionStart` payload", "unknown key `source`"],
        ),
        (
            map(vec![
                ("handle", s(&text)),
                ("variables", map(vec![])),
                ("x", int(1)),
            ]),
            &["the `CachedExecutionStart` payload", "unknown key `x`"],
        ),
        (
            map(vec![
                ("handle", s(&text)),
                ("variables", map(vec![])),
                ("overrides", map(vec![("check", s("warn"))])),
            ]),
            &["`overrides.check`", "CodeRegister"],
        ),
        // Any `check` is refused, `off` included: the pass never runs here, so no value of it
        // can mean anything on this request.
        (
            map(vec![
                ("handle", s(&text)),
                ("variables", map(vec![])),
                ("overrides", map(vec![("check", s("off"))])),
            ]),
            &["`overrides.check`", "CodeRegister"],
        ),
        (
            Value::Map(vec![
                (s("handle"), s(&text)),
                (s("handle"), s(&text)),
                (s("variables"), map(vec![])),
            ]),
            &[
                "the `CachedExecutionStart` payload",
                "repeats the key `handle`",
            ],
        ),
    ];
    for (payload, mentions) in cases {
        let body = *cached(&payload, client(1), &cache).expect_err("refused");
        assert_eq!(body.code, "protocol.invalid_payload", "{payload:?}");
        assert!(
            mentions.iter().all(|m| body.message.contains(m)),
            "{payload:?} refused as {:?}, naming {mentions:?}",
            body.message
        );
    }
    assert_eq!(cache.entry_count(), 1);
}

#[test]
fn an_invalid_payload_takes_precedence_over_an_unknown_handle() {
    let cache = Arc::new(AstCache::with_capacity(10_000));
    let body = *cached(
        &map(vec![
            ("handle", s("00000000000000ff")),
            ("variables", map(vec![])),
            ("source", s("1;")),
        ]),
        client(1),
        &cache,
    )
    .expect_err("refused");
    assert_eq!(body.code, "protocol.invalid_payload");
}

#[test]
fn an_overlong_handle_is_echoed_bounded_in_its_refusal() {
    let cache = Arc::new(AstCache::with_capacity(10_000));
    let long = "A".repeat(4096);
    let body = *cached(
        &map(vec![("handle", s(&long)), ("variables", map(vec![]))]),
        client(1),
        &cache,
    )
    .expect_err("refused");
    assert_eq!(body.code, "protocol.invalid_payload");
    assert!(body.message.contains("`handle`"), "{}", body.message);
    assert!(
        body.message.len() < 1024,
        "bounded, not {} bytes",
        body.message.len()
    );
}

#[test]
fn a_registration_with_findings_runs_with_a_reply_of_exactly_value() {
    use hexput_port::CheckMode;
    let cache = Arc::new(AstCache::with_capacity(10_000));
    let reply = registered(
        &registration("let unused = 1; return x;", Some(vec!["x"])),
        client(1),
        &cache,
        vec![],
        checked(CheckMode::Warn),
    )
    .unwrap();
    assert_eq!(reply_keys(&reply), ["handle", "findings"]);
    let handle = handle_in(&reply);
    let reply = cached(
        &cached_payload(handle, vec![("x", int(2))], None),
        client(1),
        &cache,
    )
    .unwrap();
    assert_eq!(
        reply_keys(&reply),
        ["value"],
        "never `findings` on a Cached Execution"
    );
    assert_eq!(cached_value(&reply), int(2));
}

#[test]
fn the_check_is_not_run_at_a_cached_execution_whatever_the_live_mode() {
    use hexput_port::CheckMode;
    // Registered with the check off, so nothing is rejected. The error finding (`z` is never
    // bound) sits on a path that never runs, so only the check could object.
    let source = "if (false) { return z; }; return 1;";
    let cache = Arc::new(AstCache::with_capacity(10_000));
    let handle = handle_of_registration(source, client(1), &cache);
    let reply = cached_with(
        &cached_payload(handle, vec![], None),
        client(1),
        &cache,
        vec![],
        checked(CheckMode::Error),
    )
    .unwrap();
    assert_eq!(cached_value(&reply), int(1));
    // Contrast: Direct Execution under the same mode rejects the same Script before it runs.
    let body =
        *direct_execution_configured(&payload(source, vec![]), vec![], checked(CheckMode::Error))
            .expect_err("Direct Execution checks");
    assert_eq!(body.code, "reference.undeclared_identifier");
}

#[test]
fn registered_variable_names_are_neither_enforced_nor_stored() {
    let cache = Arc::new(AstCache::with_capacity(10_000));
    let handle = registered(
        &registration("return x;", Some(vec!["x"])),
        client(1),
        &cache,
        vec![],
        Settings::new(),
    )
    .map(|reply| handle_in(&reply))
    .unwrap();
    let body = *cached(&cached_payload(handle, vec![], None), client(1), &cache)
        .expect_err("x is not supplied");
    assert_eq!(body.code, "reference.undeclared_identifier");
    let reply = cached(
        &cached_payload(handle, vec![("x", int(3)), ("extra", int(1))], None),
        client(1),
        &cache,
    )
    .unwrap();
    assert_eq!(
        cached_value(&reply),
        int(3),
        "an extra variable simply binds"
    );
}

#[test]
fn a_starting_variable_the_script_redeclares_is_a_duplicate_declaration() {
    let cache = Arc::new(AstCache::with_capacity(10_000));
    let handle = handle_of_registration("let x = 1; return x;", client(1), &cache);
    let body = *cached(
        &cached_payload(handle, vec![("x", int(3))], None),
        client(1),
        &cache,
    )
    .expect_err("redeclared");
    assert_eq!(body.code, "syntax.duplicate_declaration");
}

#[test]
fn a_live_config_limit_applies_to_a_cached_run() {
    let cache = Arc::new(AstCache::with_capacity(10_000));
    let handle = handle_of_registration("return getOrder(1);", client(1), &cache);
    let body = *cached_with(
        &cached_payload(handle, vec![], None),
        client(1),
        &cache,
        get_order().into_iter().map(Registration::from).collect(),
        setting(Setting::RpcCalls, 0),
    )
    .expect_err("no host calls allowed");
    assert_eq!(body.code, "budget.rpc_calls_exceeded");
}

#[test]
fn a_live_feature_toggle_refuses_a_construct_at_a_cached_run() {
    let cache = Arc::new(AstCache::with_capacity(10_000));
    let handle = handle_of_registration(COUNTED_LOOP, client(1), &cache);
    let mut loops_off = Settings::new();
    loops_off.set_feature(hexput_port::Feature::Loops, false);
    let body = *cached_with(
        &cached_payload(handle, vec![], None),
        client(1),
        &cache,
        vec![],
        loops_off,
    )
    .expect_err("loops are disabled");
    assert_eq!(body.code, "policy.construct_disabled");
}

#[test]
fn overrides_apply_to_one_run_alone() {
    let cache = Arc::new(AstCache::with_capacity(10_000));
    let looping = handle_of_registration(COUNTED_LOOP, client(1), &cache);
    let loops_off = map(vec![(
        "features",
        map(vec![("loops", Value::Boolean(false))]),
    )]);
    let body = *cached(
        &cached_payload(looping, vec![], Some(loops_off)),
        client(1),
        &cache,
    )
    .expect_err("the override disables loops for this run");
    assert_eq!(body.code, "policy.construct_disabled");
    let reply = cached(&cached_payload(looping, vec![], None), client(1), &cache).unwrap();
    assert_eq!(
        cached_value(&reply),
        int(2),
        "the next run is back under the Config"
    );

    let long = handle_of_registration("return \"a long enough result\";", client(1), &cache);
    let tight = map(vec![("budget", map(vec![("output_size_bytes", int(8))]))]);
    let body = *cached(
        &cached_payload(long, vec![], Some(tight)),
        client(1),
        &cache,
    )
    .expect_err("the output budget is 8 bytes for this run");
    assert_eq!(body.code, "budget.output_size_exceeded");
    let reply = cached(&cached_payload(long, vec![], None), client(1), &cache).unwrap();
    assert_eq!(cached_value(&reply), s("a long enough result"));

    let bounded = handle_of_registration(
        "let n = 0; while (n < limit) { n = n + 1; }; return n;",
        client(1),
        &cache,
    );
    let one_ms = map(vec![("budget", map(vec![("cpu_time_ms", int(1))]))]);
    let body = *cached(
        &cached_payload(bounded, vec![("limit", int(1_000_000_000))], Some(one_ms)),
        client(1),
        &cache,
    )
    .expect_err("the CPU budget is 1 ms for this run");
    assert_eq!(body.code, "budget.cpu_time_exceeded");
    let reply = cached(
        &cached_payload(bounded, vec![("limit", int(50_000))], None),
        client(1),
        &cache,
    )
    .unwrap();
    assert_eq!(
        cached_value(&reply),
        int(50_000),
        "the next run is back under the Config's CPU budget"
    );
}

#[test]
fn a_holder_the_script_writes_comes_back_as_a_modification_as_on_direct_execution() {
    let source = "n = v; return 0;";
    let cache = Arc::new(AstCache::with_capacity(10_000));
    let handle = handle_of_registration(source, client(1), &cache);
    let n = map(vec![
        ("__secret", map(vec![("ref", s("r1"))])),
        ("value", int(1)),
    ]);
    let reply = cached(
        &cached_payload(handle, vec![("n", n.clone()), ("v", int(7))], None),
        client(1),
        &cache,
    )
    .unwrap();
    assert_eq!(reply_keys(&reply), ["value", "modifications"]);
    assert_eq!(
        reply.as_map().unwrap()[1].1,
        Value::Array(vec![map(vec![("ref", s("r1")), ("value", int(7))])])
    );
    let direct = direct_execution(&payload(source, vec![("n", n), ("v", int(7))])).unwrap();
    assert_eq!(reply, direct);
}

#[test]
fn a_blanket_granted_call_is_made_and_an_unregistered_one_is_refused() {
    let cache = Arc::new(AstCache::with_capacity(10_000));
    let granted = handle_of_registration("return f(1);", client(1), &cache);
    let ungranted = handle_of_registration("return g(1);", client(1), &cache);
    let registrations =
        || -> Vec<Registration> { vec![Registration::from(("f".to_owned(), true))] };
    // The call table is gone, so a call that is sent fails with `host.no_reply`: it passed the
    // capability decision and reached the wire.
    let body = *cached_with(
        &cached_payload(granted, vec![], None),
        client(1),
        &cache,
        registrations(),
        Settings::new(),
    )
    .expect_err("no connection to answer the call");
    assert_eq!(body.code, "host.no_reply");
    let body = *cached_with(
        &cached_payload(ungranted, vec![], None),
        client(1),
        &cache,
        registrations(),
        Settings::new(),
    )
    .expect_err("g is not registered");
    assert_eq!(body.code, "capability.unknown_function");
}

// --- Story 4.3: bound the cache and clean it up with the Session ---

/// A `CodeUnregister` payload removing `handle`.
fn unregister_payload(handle: hexput_shared::ids::ScriptHandle) -> Value {
    map(vec![("handle", s(&handle.to_string()))])
}

/// `code_unregister` for `client` against `cache`.
fn unregister(
    payload: &Value,
    client: hexput_shared::ids::ClientId,
    cache: &AstCache,
) -> Result<Value, Box<ErrorBody>> {
    hexput_script::code_unregister(payload, client, cache)
}

/// Register `count` distinct Scripts for `client` into `cache`, returning their handles in order.
fn register_many(
    count: usize,
    client: hexput_shared::ids::ClientId,
    cache: &Arc<AstCache>,
) -> Vec<hexput_shared::ids::ScriptHandle> {
    (0..count)
        .map(|n| {
            handle_in(
                &registered(
                    &registration(&format!("return {n};"), None),
                    client,
                    cache,
                    vec![],
                    Settings::new(),
                )
                .unwrap(),
            )
        })
        .collect()
}

#[test]
fn the_cache_stays_within_its_capacity_and_counts_each_eviction() {
    let cache = Arc::new(AstCache::with_capacity(2));
    register_many(5, client(1), &cache);
    assert!(cache.entry_count() <= 2, "{}", cache.entry_count());
    assert_eq!(cache.entry_count() + cache.evictions(), 5);
}

#[test]
fn an_evicted_handle_is_unknown_to_a_cached_run_and_the_kept_one_still_runs() {
    let cache = Arc::new(AstCache::with_capacity(1));
    let handles = register_many(2, client(1), &cache);
    let kept = cache.handles(client(1));
    assert_eq!(kept.len(), 1, "{kept:?}");
    let evicted = *handles
        .iter()
        .find(|h| !kept.contains(h))
        .expect("one of them is gone");
    let body = *cached(&cached_payload(evicted, vec![], None), client(1), &cache)
        .expect_err("an evicted handle is refused");
    assert_eq!(body.code, "protocol.unknown_script_handle");
    let reply = cached(&cached_payload(kept[0], vec![], None), client(1), &cache).unwrap();
    // Each Script returns its own registration index.
    let index = handles.iter().position(|h| *h == kept[0]).unwrap();
    assert_eq!(cached_value(&reply), int(index as i64));
}

#[test]
fn a_removed_or_evicted_program_is_released_at_once() {
    let cache = Arc::new(AstCache::with_capacity(1));
    let first = handle_of_registration("return 1;", client(1), &cache);
    let weak = Arc::downgrade(&cache.get(client(1), first).unwrap());
    handle_of_registration("return 2;", client(1), &cache);
    // Whichever of the two the cache kept (or whether it refused the newer one at admission), the
    // first program is alive exactly when the cache still holds its handle.
    let kept = cache.handles(client(1));
    assert_eq!(kept.len(), 1, "{kept:?}");
    assert_eq!(
        weak.upgrade().is_some(),
        kept.contains(&first),
        "an evicted program is released by the cache"
    );
}

#[test]
fn a_removed_handle_is_unknown_and_its_program_is_released_at_once() {
    let cache = Arc::new(AstCache::with_capacity(10_000));
    let handle = handle_of_registration("return 1;", client(1), &cache);
    let weak = Arc::downgrade(&cache.get(client(1), handle).unwrap());
    let reply = unregister(&unregister_payload(handle), client(1), &cache).unwrap();
    // Checked before anything else touches the cache: `entry_count` and `handles` run moka's
    // maintenance themselves, which would hide a removal that left the program in its write log.
    assert!(
        weak.upgrade().is_none(),
        "the cache keeps no reference to the program"
    );
    assert_eq!(reply, Value::Map(vec![]), "Result {{}}");
    assert!(!cache.handles(client(1)).contains(&handle));
    assert_eq!(cache.entry_count(), 0);
    let body = *cached(&cached_payload(handle, vec![], None), client(1), &cache)
        .expect_err("the handle is gone");
    assert_eq!(body.code, "protocol.unknown_script_handle");
}

#[test]
fn unregistering_twice_or_another_sessions_or_a_never_issued_handle_is_unknown() {
    let cache = Arc::new(AstCache::with_capacity(10_000));
    let a = handle_of_registration("return 1;", client(1), &cache);
    let b = handle_of_registration("return 2;", client(2), &cache);

    // B cannot remove A's handle, and A's entry is untouched.
    let body = *unregister(&unregister_payload(a), client(2), &cache)
        .expect_err("a foreign handle is unknown here");
    assert_eq!(body.code, "protocol.unknown_script_handle");
    assert_eq!(cache.handles(client(1)), vec![a]);

    unregister(&unregister_payload(a), client(1), &cache).unwrap();
    let again = *unregister(&unregister_payload(a), client(1), &cache)
        .expect_err("a second removal finds nothing");
    assert_eq!(again.code, "protocol.unknown_script_handle");

    let never: hexput_shared::ids::ScriptHandle = "00000000000000ff".parse().unwrap();
    let never = *unregister(&unregister_payload(never), client(1), &cache)
        .expect_err("a never-issued handle is unknown");
    assert_eq!(never.code, "protocol.unknown_script_handle");
    assert_eq!(cache.handles(client(2)), vec![b], "B's entry is untouched");
}

#[test]
fn a_malformed_unregister_names_its_key_and_removes_nothing() {
    let cache = Arc::new(AstCache::with_capacity(10_000));
    let handle = handle_of_registration("return 1;", client(1), &cache);
    let text = handle.to_string();
    let cases: Vec<(Value, &str)> = vec![
        (Value::Nil, "handle"),
        (map(vec![]), "handle"),
        (map(vec![("handle", Value::Nil)]), "handle"),
        (map(vec![("handle", Value::from(1))]), "handle"),
        (map(vec![("handle", s("ABC"))]), "handle"),
        (map(vec![("handle", s(&text)), ("x", Value::from(1))]), "x"),
        (
            Value::Map(vec![(s("handle"), s(&text)), (s("handle"), s(&text))]),
            "handle",
        ),
        // A refused payload is refused before the handle is looked up.
        (
            map(vec![
                ("handle", s("00000000000000ff")),
                ("x", Value::from(1)),
            ]),
            "x",
        ),
    ];
    for (payload, key) in cases {
        let body = *unregister(&payload, client(1), &cache).expect_err("refused");
        assert_eq!(body.code, "protocol.invalid_payload", "{payload:?}");
        assert!(
            body.message.contains(&format!("`{key}`")),
            "{payload:?}: {}",
            body.message
        );
    }
    assert_eq!(cache.entry_count(), 1, "nothing was removed");
    assert_eq!(cache.handles(client(1)), vec![handle]);
}

#[test]
fn a_removal_is_not_an_eviction_and_a_teardown_is_not_one_either() {
    let cache = Arc::new(AstCache::with_capacity(10));
    let a = handle_of_registration("return 1;", client(1), &cache);
    handle_of_registration("return 2;", client(1), &cache);
    handle_of_registration("return 3;", client(2), &cache);
    unregister(&unregister_payload(a), client(1), &cache).unwrap();
    assert_eq!(cache.evictions(), 0, "an unregister is not an eviction");
    cache.close(client(1));
    assert_eq!(cache.evictions(), 0, "a teardown is not an eviction");
    assert_eq!(cache.entry_count(), 1);
}

#[test]
fn a_session_teardown_releases_its_entries_and_no_other_sessions() {
    let cache = Arc::new(AstCache::with_capacity(10_000));
    let ours = register_many(2, client(1), &cache);
    let theirs = handle_of_registration("return 3;", client(2), &cache);
    let released: Vec<_> = ours
        .iter()
        .map(|handle| Arc::downgrade(&cache.get(client(1), *handle).unwrap()))
        .collect();
    cache.close(client(1));
    // Before anything else runs moka's maintenance: the close itself released the programs.
    assert!(
        released.iter().all(|weak| weak.upgrade().is_none()),
        "the cache keeps no reference to a released program"
    );
    assert!(cache.handles(client(1)).is_empty());
    assert_eq!(cache.handles(client(2)), vec![theirs]);
    assert_eq!(cache.entry_count(), 1);
}

#[test]
fn a_registration_for_a_session_never_opened_or_already_ended_inserts_nothing() {
    let cache = Arc::new(AstCache::with_capacity(10_000));
    // Never opened.
    let body = *register_unopened(
        &registration("return 1;", None),
        client(1),
        &cache,
        vec![],
        Settings::new(),
    )
    .expect_err("an unopened Session refuses");
    assert_eq!(body.code, "protocol.init_not_completed");
    assert!(
        body.message.contains("nothing was registered"),
        "{}",
        body.message
    );
    // Opened, then ended: a registration still in flight finds it closed.
    cache.open(client(1));
    cache.close(client(1));
    let body = *register_unopened(
        &registration("return 1;", None),
        client(1),
        &cache,
        vec![],
        Settings::new(),
    )
    .expect_err("an ended Session refuses");
    assert_eq!(body.code, "protocol.init_not_completed");
    assert_eq!(cache.entry_count(), 0);
    assert_eq!(cache.evictions(), 0);
}
