//! Story 2.6: Direct Execution — the `ExecutionStart` payload, the wire↔value conversion both
//! ways, and the result guards.

use hexput_port::{
    CorrelationId, Envelope, ErrorBody, MAX_FRAME_LEN, MAX_NESTING_DEPTH, MessageType, Setting,
    Settings, Value, decode, encode, encode_frame,
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
    direct_execution_configured(payload, registrations, Settings::new())
}

/// [`direct_execution_registering`] for a Session whose Config sets `settings` (Story 3.7).
fn direct_execution_configured(
    payload: &Value,
    registrations: Vec<(String, bool)>,
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
