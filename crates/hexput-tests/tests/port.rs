//! Story 2.2: the wire codec — framing, envelope decoding, and the protocol error responses.

use hexput_port::{
    CorrelationId, Envelope, ErrorBody, FrameDecoder, LENGTH_PREFIX_LEN, MAX_FRAME_LEN,
    MAX_NESTING_DEPTH, MessageType, ProtocolCode, ProtocolFailure, Value, WireSpan, decode, encode,
    encode_frame, error_response,
};

// --- helpers ---

fn s(text: &str) -> Value {
    Value::from(text)
}

fn map(pairs: Vec<(&str, Value)>) -> Value {
    Value::Map(pairs.into_iter().map(|(k, v)| (s(k), v)).collect())
}

/// MessagePack bytes of an arbitrary value — including values that are not envelopes.
fn raw(value: &Value) -> Vec<u8> {
    let mut out = Vec::new();
    rmpv::encode::write_value(&mut out, value).unwrap();
    out
}

fn request(id: u64, payload: Value) -> Envelope<Value> {
    Envelope::new(CorrelationId(id), MessageType::ExecutionStart, payload)
}

fn frame(envelope: &Envelope<Value>) -> Vec<u8> {
    encode_frame(&encode(envelope).unwrap()).unwrap()
}

fn reject(bytes: &[u8]) -> ProtocolFailure {
    decode(bytes).expect_err("the frame should be rejected")
}

/// The failure's error response, decoded back as a Backend would see it.
fn response_body(failure: &ProtocolFailure) -> (Option<CorrelationId>, ErrorBody) {
    let response = decode(&encode(&failure.to_response()).unwrap()).unwrap();
    assert_eq!(response.message_type, MessageType::Error);
    let body: ErrorBody = rmpv::ext::from_value(response.payload).unwrap();
    (response.id, body)
}

fn nested_payload(levels: usize) -> Value {
    let mut v = Value::Nil;
    for _ in 0..levels {
        v = Value::Array(vec![v]);
    }
    v
}

// --- round trips ---

#[test]
fn envelope_round_trips_through_encode_frame_and_decode() {
    let payload = map(vec![
        ("script", s("return a + 1;")),
        (
            "variables",
            map(vec![
                ("a", Value::from(41)),
                (
                    "list",
                    Value::Array(vec![Value::from(-1), Value::F64(2.5), Value::Nil]),
                ),
                ("flag", Value::Boolean(true)),
                ("bytes", Value::Binary(vec![0, 1, 2])),
                ("ext", Value::Ext(7, vec![9, 9])),
            ]),
        ),
    ]);
    for message_type in MessageType::ALL {
        let sent = Envelope::new(CorrelationId(u64::MAX), *message_type, payload.clone());
        let mut decoder = FrameDecoder::new();
        decoder.push(&frame(&sent));
        let body = decoder.next_frame().unwrap().unwrap();
        assert_eq!(decode(&body).unwrap(), sent);
        assert_eq!(decoder.next_frame().unwrap(), None);
        assert_eq!(decoder.buffered_len(), 0);
    }
}

#[test]
fn envelope_is_a_self_describing_map_with_named_fields() {
    let bytes = encode(&request(7, Value::from(1))).unwrap();
    let value = rmpv::decode::read_value(&mut bytes.as_slice()).unwrap();
    assert_eq!(
        value,
        map(vec![
            ("id", Value::from(7)),
            ("type", s("ExecutionStart")),
            ("payload", Value::from(1)),
        ])
    );
}

#[test]
fn absent_payload_reads_as_nil() {
    let bytes = raw(&map(vec![("id", Value::from(3)), ("type", s("Init"))]));
    let envelope = decode(&bytes).unwrap();
    assert_eq!(
        envelope,
        Envelope::new(CorrelationId(3), MessageType::Init, Value::Nil)
    );
}

#[test]
fn out_of_order_responses_match_their_requests_by_id() {
    let requests = [request(1, s("slow")), request(2, s("fast"))];
    // Responses are written 2 then 1.
    let mut stream = Vec::new();
    for req in requests.iter().rev() {
        let id = req.id.unwrap();
        let response = Envelope::new(id, MessageType::Result, req.payload.clone());
        stream.extend(frame(&response));
    }
    let mut decoder = FrameDecoder::new();
    decoder.push(&stream);
    let mut seen = Vec::new();
    while let Some(body) = decoder.next_frame().unwrap() {
        let response = decode(&body).unwrap();
        let req = requests.iter().find(|r| r.id == response.id).unwrap();
        assert_eq!(
            response.payload, req.payload,
            "response carries its request's id"
        );
        seen.push(response.id.unwrap().get());
    }
    assert_eq!(seen, [2, 1], "never reordered");
}

#[test]
fn duplicate_ids_are_not_deduplicated() {
    let mut decoder = FrameDecoder::new();
    decoder.push(&frame(&request(5, s("a"))));
    decoder.push(&frame(&request(5, s("b"))));
    let first = decode(&decoder.next_frame().unwrap().unwrap()).unwrap();
    let second = decode(&decoder.next_frame().unwrap().unwrap()).unwrap();
    assert_eq!((first.payload, second.payload), (s("a"), s("b")));
}

// --- framing ---

#[test]
fn frames_split_byte_by_byte_or_coalesced_decode_the_same() {
    let sent: Vec<_> = (0..5).map(|i| request(i, Value::from(i * 10))).collect();
    let stream: Vec<u8> = sent.iter().flat_map(frame).collect();

    // Byte by byte.
    let mut decoder = FrameDecoder::new();
    let mut got = Vec::new();
    for byte in &stream {
        decoder.push(std::slice::from_ref(byte));
        while let Some(body) = decoder.next_frame().unwrap() {
            got.push(decode(&body).unwrap());
        }
    }
    assert_eq!(got, sent);

    // All in one chunk.
    let mut decoder = FrameDecoder::new();
    decoder.push(&stream);
    let mut got = Vec::new();
    while let Some(body) = decoder.next_frame().unwrap() {
        got.push(decode(&body).unwrap());
    }
    assert_eq!(got, sent);

    // Uneven chunks that straddle frame boundaries.
    let mut decoder = FrameDecoder::new();
    let mut got = Vec::new();
    for chunk in stream.chunks(7) {
        decoder.push(chunk);
        while let Some(body) = decoder.next_frame().unwrap() {
            got.push(decode(&body).unwrap());
        }
    }
    assert_eq!(got, sent);
}

#[test]
fn length_prefix_is_four_big_endian_bytes() {
    let framed = encode_frame(b"abc").unwrap();
    assert_eq!(LENGTH_PREFIX_LEN, 4);
    assert_eq!(framed, [0, 0, 0, 3, b'a', b'b', b'c']);
}

#[test]
fn empty_frame_is_malformed() {
    let mut decoder = FrameDecoder::new();
    decoder.push(&[0, 0, 0, 0]);
    let body = decoder.next_frame().unwrap().unwrap();
    assert!(body.is_empty());
    let failure = reject(&body);
    assert_eq!(failure.error.code, ProtocolCode::MalformedFrame);
    assert_eq!(failure.id, None);
}

#[test]
fn oversized_length_prefix_poisons_the_decoder() {
    let mut decoder = FrameDecoder::new();
    let claimed = u32::try_from(MAX_FRAME_LEN + 1).unwrap();
    decoder.push(&claimed.to_be_bytes());
    let failure = decoder.next_frame().unwrap_err();
    assert_eq!(failure.error.code, ProtocolCode::FrameTooLarge);
    assert!(failure.error.is_fatal());
    assert_eq!(failure.id, None);
    assert!(decoder.is_poisoned());

    // The stream cannot resync: a valid frame after it is never returned.
    decoder.push(&frame(&request(1, Value::Nil)));
    assert_eq!(
        decoder.buffered_len(),
        0,
        "bytes after poisoning are discarded"
    );
    assert_eq!(
        decoder.next_frame().unwrap_err().error.code,
        ProtocolCode::FrameTooLarge
    );

    let (id, body) = response_body(&failure);
    assert_eq!(id, None);
    assert_eq!(body.code, "protocol.frame_too_large");
}

#[test]
fn maximum_length_prefix_is_accepted_and_waits_for_its_bytes() {
    let mut decoder = FrameDecoder::new();
    decoder.push(&u32::try_from(MAX_FRAME_LEN).unwrap().to_be_bytes());
    assert_eq!(decoder.next_frame().unwrap(), None);
    assert!(!decoder.is_poisoned());
    // Only the prefix is held — nothing was reserved for the claimed length.
    assert_eq!(decoder.buffered_len(), LENGTH_PREFIX_LEN);
}

#[test]
fn encode_frame_refuses_an_oversized_body() {
    let body = vec![0_u8; MAX_FRAME_LEN + 1];
    assert!(matches!(
        encode_frame(&body),
        Err(hexput_port::EncodeError::FrameTooLarge { len }) if len == MAX_FRAME_LEN + 1
    ));
    assert!(encode_frame(&vec![0; MAX_FRAME_LEN]).is_ok());
}

// --- malformed and truncated content ---

#[test]
fn garbage_is_malformed_with_a_nil_id() {
    // 0xc1 is the one byte MessagePack reserves and never uses.
    for bytes in [&[0xc1][..], &[0xc1, 0x00, 0x01]] {
        let failure = reject(bytes);
        assert_eq!(failure.error.code, ProtocolCode::MalformedFrame);
        assert_eq!(failure.id, None);
    }
}

#[test]
fn trailing_bytes_after_a_value_are_malformed() {
    let mut bytes = encode(&request(9, Value::Nil)).unwrap();
    bytes.push(0xc0);
    let failure = reject(&bytes);
    assert_eq!(failure.error.code, ProtocolCode::MalformedFrame);
    assert_eq!(failure.id, None);
    assert!(failure.error.message.contains("trailing"));
}

#[test]
fn a_value_that_ends_early_is_truncated() {
    let bytes = encode(&request(9, s("hello world"))).unwrap();
    for cut in 1..bytes.len() {
        let failure = reject(&bytes[..cut]);
        assert_eq!(
            failure.error.code,
            ProtocolCode::TruncatedFrame,
            "cut at {cut} of {}",
            bytes.len()
        );
        assert_eq!(failure.id, None);
    }
}

#[test]
fn huge_claimed_counts_are_truncated_without_allocating_for_them() {
    // array32 / map32 / str32 / bin32 headers claiming 2^32 - 1 elements or bytes, with no data.
    for marker in [0xdd_u8, 0xdf, 0xdb, 0xc6] {
        let bytes = [marker, 0xff, 0xff, 0xff, 0xff];
        let failure = reject(&bytes);
        assert_eq!(
            failure.error.code,
            ProtocolCode::TruncatedFrame,
            "marker {marker:#x}"
        );
    }
    // The same inside a well-formed envelope prefix.
    let mut bytes = raw(&map(vec![("id", Value::from(1)), ("type", s("Init"))]));
    bytes[0] = 0x83; // claim a third entry
    bytes.extend(raw(&s("payload")));
    bytes.extend([0xdd, 0xff, 0xff, 0xff, 0xff]);
    assert_eq!(reject(&bytes).error.code, ProtocolCode::TruncatedFrame);
}

#[test]
fn nesting_past_the_limit_is_malformed_without_overflowing_the_stack() {
    // The envelope map is one level, so the payload may nest MAX_NESTING_DEPTH - 1 deep.
    let at_limit = request(1, nested_payload(MAX_NESTING_DEPTH - 1));
    assert_eq!(decode(&encode(&at_limit).unwrap()).unwrap(), at_limit);

    let past = raw(&map(vec![
        ("id", Value::from(1)),
        ("type", s("Init")),
        ("payload", nested_payload(MAX_NESTING_DEPTH)),
    ]));
    let failure = reject(&past);
    assert_eq!(failure.error.code, ProtocolCode::MalformedFrame);
    assert!(failure.error.message.contains("nesting"));

    // A frame of nothing but array headers, far deeper than any stack could recurse.
    let deep = vec![0x91_u8; MAX_FRAME_LEN / 16];
    assert_eq!(reject(&deep).error.code, ProtocolCode::MalformedFrame);
}

// --- invalid envelopes ---

#[test]
fn a_value_that_is_not_a_map_is_an_invalid_envelope() {
    for value in [
        Value::Nil,
        Value::from(1),
        s("Init"),
        Value::Array(vec![Value::from(1)]),
    ] {
        let failure = reject(&raw(&value));
        assert_eq!(failure.error.code, ProtocolCode::InvalidEnvelope);
        assert_eq!(failure.id, None);
    }
}

#[test]
fn missing_or_non_integer_id_is_an_invalid_envelope_with_a_nil_id() {
    let cases = [
        map(vec![("type", s("Init"))]),
        map(vec![("id", s("1")), ("type", s("Init"))]),
        map(vec![("id", Value::from(-1)), ("type", s("Init"))]),
        map(vec![("id", Value::F64(1.0)), ("type", s("Init"))]),
        map(vec![("id", Value::Nil), ("type", s("Init"))]),
    ];
    for case in cases {
        let failure = reject(&raw(&case));
        assert_eq!(failure.error.code, ProtocolCode::InvalidEnvelope, "{case}");
        assert_eq!(failure.id, None, "{case}");
    }
}

#[test]
fn missing_or_non_string_type_is_an_invalid_envelope_echoing_the_id() {
    let cases = [
        map(vec![("id", Value::from(42))]),
        map(vec![("id", Value::from(42)), ("type", Value::from(1))]),
        map(vec![("id", Value::from(42)), ("type", Value::Nil)]),
    ];
    for case in cases {
        let failure = reject(&raw(&case));
        assert_eq!(failure.error.code, ProtocolCode::InvalidEnvelope, "{case}");
        assert_eq!(failure.id, Some(CorrelationId(42)), "{case}");
        let (id, body) = response_body(&failure);
        assert_eq!(id, Some(CorrelationId(42)));
        assert_eq!(body.code, "protocol.invalid_envelope");
    }
}

#[test]
fn unknown_or_repeated_fields_are_an_invalid_envelope() {
    let unknown = map(vec![
        ("id", Value::from(8)),
        ("type", s("Init")),
        ("extra", Value::Nil),
    ]);
    let failure_unknown = reject(&raw(&unknown));
    assert_eq!(failure_unknown.error.code, ProtocolCode::InvalidEnvelope);
    assert_eq!(failure_unknown.id, Some(CorrelationId(8)));
    assert!(failure_unknown.error.message.contains("extra"));

    let non_string_key = Value::Map(vec![
        (s("id"), Value::from(8)),
        (s("type"), s("Init")),
        (Value::from(1), Value::Nil),
    ]);
    assert_eq!(reject(&raw(&non_string_key)).id, Some(CorrelationId(8)));

    let repeated_type = map(vec![
        ("id", Value::from(8)),
        ("type", s("Init")),
        ("type", s("Result")),
    ]);
    let f = reject(&raw(&repeated_type));
    assert_eq!(f.error.code, ProtocolCode::InvalidEnvelope);
    assert_eq!(f.id, Some(CorrelationId(8)));

    // A repeated id is ambiguous, so neither copy is echoed.
    let repeated_id = map(vec![
        ("id", Value::from(8)),
        ("id", Value::from(9)),
        ("type", s("Init")),
    ]);
    let f = reject(&raw(&repeated_id));
    assert_eq!(f.error.code, ProtocolCode::InvalidEnvelope);
    assert_eq!(f.id, None);
}

#[test]
fn unknown_type_echoes_the_id_and_names_the_type() {
    let bytes = raw(&map(vec![
        ("id", Value::from(77)),
        ("type", s("Frobnicate")),
    ]));
    let failure = reject(&bytes);
    assert_eq!(failure.error.code, ProtocolCode::UnknownMessageType);
    assert_eq!(failure.id, Some(CorrelationId(77)));
    assert!(failure.error.message.contains("Frobnicate"));

    let (id, body) = response_body(&failure);
    assert_eq!(id, Some(CorrelationId(77)));
    assert_eq!(
        body,
        ErrorBody {
            severity: "error".into(),
            category: "protocol".into(),
            code: "protocol.unknown_message_type".into(),
            message: failure.error.message.clone(),
            span: None,
            findings: Vec::new(),
        }
    );

    // Type names are case-sensitive.
    let bytes = raw(&map(vec![("id", Value::from(1)), ("type", s("init"))]));
    assert_eq!(
        decode(&bytes).unwrap_err().error.code,
        ProtocolCode::UnknownMessageType
    );
}

#[test]
fn nil_id_decodes_only_on_an_error_response() {
    let body = ErrorBody::from(&hexput_port::ProtocolError::new(
        ProtocolCode::MalformedFrame,
        "x",
    ));
    let response = error_response(None, &body);
    let bytes = encode(&response).unwrap();
    let value = rmpv::decode::read_value(&mut bytes.as_slice()).unwrap();
    assert_eq!(value.as_map().unwrap()[0], (s("id"), Value::Nil));
    assert_eq!(decode(&bytes).unwrap(), response);
}

// --- error shapes ---

#[test]
fn protocol_codes_are_stable_and_in_the_protocol_category() {
    let expected = [
        "protocol.malformed_frame",
        "protocol.truncated_frame",
        "protocol.invalid_envelope",
        "protocol.unknown_message_type",
        "protocol.frame_too_large",
        "protocol.init_not_completed",
        "protocol.unexpected_message",
        "protocol.invalid_payload",
        "protocol.already_initialized",
        "protocol.result_too_deep",
        "protocol.response_too_large",
    ];
    let actual: Vec<_> = ProtocolCode::ALL.iter().map(|c| c.as_str()).collect();
    assert_eq!(actual, expected);
    for code in ProtocolCode::ALL {
        assert!(code.as_str().starts_with("protocol."));
        let fatal = hexput_port::ProtocolError::new(*code, "").is_fatal();
        assert_eq!(fatal, *code == ProtocolCode::FrameTooLarge, "{code}");
    }
}

#[test]
fn error_body_from_a_parser_diagnostic_round_trips_with_its_span() {
    let diagnostic = hexput_parser::parse("let x = ;\nlet y = 2;").unwrap_err();
    let body = ErrorBody::from(&diagnostic);
    assert_eq!(body.category, diagnostic.category.as_str());
    assert_eq!(body.code, diagnostic.code.as_str());
    assert_eq!(body.severity, "error");
    assert_eq!(body.message, diagnostic.message);
    let span = body.span.unwrap();
    assert_eq!(
        span,
        WireSpan {
            offset: diagnostic.span.offset as u64,
            len: diagnostic.span.len as u64,
            line: diagnostic.span.line as u64,
            column: diagnostic.span.column as u64,
        }
    );

    let response = error_response(Some(CorrelationId(12)), &body);
    let mut decoder = FrameDecoder::new();
    decoder.push(&frame(&response));
    let decoded = decode(&decoder.next_frame().unwrap().unwrap()).unwrap();
    assert_eq!(decoded.id, Some(CorrelationId(12)));
    assert_eq!(decoded.message_type, MessageType::Error);
    let back: ErrorBody = rmpv::ext::from_value(decoded.payload).unwrap();
    assert_eq!(back, body);
}

#[test]
fn error_body_from_a_runtime_diagnostic_keeps_its_category() {
    let program = hexput_parser::parse("return 1 / 0;").unwrap();
    let diagnostic = hexput_interpreter::evaluate(&program).unwrap_err();
    let body = ErrorBody::from(&diagnostic);
    assert_eq!(body.category, "arithmetic");
    assert_eq!(body.code, "arithmetic.division_by_zero");
    assert!(body.span.is_some());
}

#[test]
fn every_message_type_round_trips_with_its_as_str_spelling() {
    for message_type in MessageType::ALL {
        let bytes = encode(&Envelope::new(CorrelationId(1), *message_type, Value::Nil)).unwrap();
        let value = rmpv::decode::read_value(&mut bytes.as_slice()).unwrap();
        let type_field = value
            .as_map()
            .unwrap()
            .iter()
            .find(|(k, _)| k.as_str() == Some("type"));
        assert_eq!(type_field.unwrap().1.as_str(), Some(message_type.as_str()));
        assert_eq!(decode(&bytes).unwrap().message_type, *message_type);
    }
}

#[test]
fn protocol_error_payload_is_exactly_the_five_keys_with_a_nil_span() {
    let failure = reject(&[0xc1]);
    let payload = failure.to_response().payload;
    let bytes = raw(&payload);
    let value = rmpv::decode::read_value(&mut bytes.as_slice()).unwrap();
    let pairs = value.as_map().unwrap();
    let keys: Vec<_> = pairs.iter().map(|(k, _)| k.as_str().unwrap()).collect();
    assert_eq!(keys, ["severity", "category", "code", "message", "span"]);
    assert_eq!(pairs[0].1, s("error"));
    assert_eq!(pairs[1].1, s("protocol"));
    assert_eq!(pairs[2].1, s("protocol.malformed_frame"));
    assert_eq!(pairs[4].1, Value::Nil);
}

#[test]
fn error_body_from_a_warning_finding_keeps_warning_severity() {
    use hexput_check::{Environment, Policy, check};
    let program = hexput_parser::parse("let unused = 1;").unwrap();
    let findings = check(&program, &Environment::new(), &Policy::new());
    let warning = findings
        .diagnostics()
        .iter()
        .find(|d| d.code.as_str() == "reference.unused_variable")
        .expect("an unused-local finding");
    let body = ErrorBody::from(warning);
    assert_eq!(body.severity, "warning");
    assert_eq!(body.code, "reference.unused_variable");
    assert!(body.span.is_some());
}

// --- Story 3.7: the one settings decoder ---

mod settings {
    use hexput_port::{MAX_FRAME_LEN, Setting, Settings, Value, decode_settings};

    use super::{map, s};

    fn int(n: i64) -> Value {
        Value::from(n)
    }

    fn refusal(value: &Value, prefix: &str) -> String {
        decode_settings(value, prefix).expect_err("refused")
    }

    #[test]
    fn an_empty_map_sets_nothing() {
        assert_eq!(decode_settings(&map(vec![]), "config"), Ok(Settings::new()));
        assert_eq!(
            decode_settings(&map(vec![("budget", map(vec![]))]), "config"),
            Ok(Settings::new())
        );
    }

    #[test]
    fn every_setting_decodes_at_its_path() {
        let value = map(vec![
            (
                "budget",
                map(vec![
                    ("cpu_time_ms", int(2_000)),
                    ("memory_bytes", int(1024)),
                    ("allocations", int(0)),
                    ("rpc_calls", int(2)),
                    ("output_size_bytes", Value::from(MAX_FRAME_LEN as u64)),
                    ("side_effects", int(7)),
                ]),
            ),
            ("argument_depth", int(64)),
            ("authorization_timeout_ms", int(100)),
        ]);
        let settings = decode_settings(&value, "overrides").unwrap();
        let decoded: Vec<_> = Setting::ALL.iter().map(|s| settings.get(*s)).collect();
        assert_eq!(
            decoded,
            [
                Some(2_000),
                Some(1024),
                Some(0),
                Some(2),
                Some(MAX_FRAME_LEN as u64),
                Some(7),
                Some(64),
                Some(100),
            ]
        );
    }

    #[test]
    fn every_refusal_names_the_full_path() {
        let budget = |key: &str, value: Value| map(vec![("budget", map(vec![(key, value)]))]);
        let cases: Vec<(Value, &str, &str)> = vec![
            (
                budget("rpc_calls", int(200_000)),
                "config",
                "`config.budget.rpc_calls` must be an integer from 0 to 100000; found 200000",
            ),
            (
                budget("cpu_time_ms", int(0)),
                "overrides",
                "`overrides.budget.cpu_time_ms` must be an integer from 1 to 60000; found 0",
            ),
            (
                budget("memory_bytes", int(1 << 40)),
                "config",
                "`config.budget.memory_bytes` must be an integer from 1024 to 1073741824; found \
                 1099511627776",
            ),
            (
                budget("rpc_calls", s("5")),
                "config",
                "`config.budget.rpc_calls` must be an integer from 0 to 100000; found a string",
            ),
            (
                budget("rpc_calls", Value::F64(1.0)),
                "config",
                "`config.budget.rpc_calls` must be an integer from 0 to 100000; found a float",
            ),
            (
                budget("rpc_calls", Value::F32(1.0)),
                "config",
                "`config.budget.rpc_calls` must be an integer from 0 to 100000; found a float",
            ),
            (
                budget("rpc_calls", int(-1)),
                "config",
                "`config.budget.rpc_calls` must be an integer from 0 to 100000; found -1",
            ),
            (
                budget("rpc_calls", Value::Nil),
                "config",
                "`config.budget.rpc_calls` must be an integer from 0 to 100000; found nil",
            ),
            (
                budget("cpu", int(1)),
                "config",
                "`config.budget.cpu` is not a known setting",
            ),
            (
                map(vec![("speed", int(1))]),
                "config",
                "`config.speed` is not a known setting",
            ),
            (
                map(vec![("argument_depth", int(65))]),
                "overrides",
                "`overrides.argument_depth` must be an integer from 1 to 64; found 65",
            ),
            (
                map(vec![("authorization_timeout_ms", Value::Boolean(true))]),
                "config",
                "`config.authorization_timeout_ms` must be an integer from 1 to 60000; found a \
                 boolean",
            ),
            (
                map(vec![("budget", int(1))]),
                "config",
                "`config.budget` is not a map",
            ),
            (
                Value::Array(vec![]),
                "overrides",
                "`overrides` is not a map",
            ),
            (
                map(vec![("budget", Value::Map(vec![(int(1), int(1))]))]),
                "config",
                "`config.budget` has a key that is not a string",
            ),
            (
                map(vec![(
                    "budget",
                    map(vec![("rpc_calls", int(1)), ("rpc_calls", int(2))]),
                )]),
                "config",
                "`config.budget` repeats the key `rpc_calls`",
            ),
            (
                map(vec![("budget", map(vec![])), ("budget", map(vec![]))]),
                "config",
                "`config` repeats the key `budget`",
            ),
            // A dotted key is not a way to name a nested setting.
            (
                map(vec![("budget.rpc_calls", int(1))]),
                "config",
                "`config.budget.rpc_calls` is not a known setting",
            ),
            // A setting's name is not a group, nor a group's name a setting.
            (
                map(vec![("rpc_calls", int(1))]),
                "config",
                "`config.rpc_calls` is not a known setting",
            ),
            (
                budget("argument_depth", int(1)),
                "config",
                "`config.budget.argument_depth` is not a known setting",
            ),
        ];
        for (value, prefix, expected) in cases {
            assert_eq!(refusal(&value, prefix), expected, "{value}");
        }
    }

    #[test]
    fn a_refusal_echoes_a_long_key_bounded() {
        let long = "k".repeat(10_000);
        let message = refusal(&map(vec![(long.as_str(), int(1))]), "config");
        assert!(message.len() < 200, "{message}");
        assert!(message.contains('…'), "{message}");
    }
}

// --- Story 3.9: feature toggles in the same decoder ---

mod features {
    use hexput_port::{Feature, Features, Settings, Value, decode_settings};

    use super::{map, s};

    fn features(value: Value) -> Value {
        map(vec![("features", value)])
    }

    fn refusal(value: &Value, prefix: &str) -> String {
        decode_settings(value, prefix).expect_err("refused")
    }

    #[test]
    fn every_toggle_decodes_and_an_unset_one_is_enabled() {
        let settings = decode_settings(&features(map(vec![])), "config").unwrap();
        assert_eq!(settings, Settings::new());
        assert_eq!(settings.features(), Features::ALL_ENABLED);
        for feature in Feature::ALL {
            let value = features(map(vec![(feature.as_str(), Value::Boolean(false))]));
            let settings = decode_settings(&value, "config").unwrap();
            assert_eq!(settings.feature(*feature), Some(false));
            for other in Feature::ALL {
                assert_eq!(settings.features().is_enabled(*other), other != feature);
            }
        }
        let value = features(map(vec![("loops", Value::Boolean(true))]));
        let settings = decode_settings(&value, "overrides").unwrap();
        assert_eq!(settings.feature(Feature::Loops), Some(true));
        assert_eq!(settings.features(), Features::ALL_ENABLED);
    }

    #[test]
    fn toggles_decode_beside_limits() {
        let value = map(vec![
            ("budget", map(vec![("rpc_calls", Value::from(3))])),
            ("features", map(vec![("rpc_calls", Value::Boolean(false))])),
        ]);
        let settings = decode_settings(&value, "config").unwrap();
        assert_eq!(settings.get(hexput_port::Setting::RpcCalls), Some(3));
        assert!(!settings.features().is_enabled(Feature::RpcCalls));
    }

    #[test]
    fn an_override_overlays_toggle_by_toggle_and_changes_nothing_beneath() {
        let config = decode_settings(
            &features(map(vec![
                ("loops", Value::Boolean(false)),
                ("callbacks", Value::Boolean(false)),
            ])),
            "config",
        )
        .unwrap();
        let overrides = decode_settings(
            &features(map(vec![
                ("loops", Value::Boolean(true)),
                ("array_literals", Value::Boolean(false)),
            ])),
            "overrides",
        )
        .unwrap();
        let before = config;
        let effective = config.overlay(&overrides).features();
        assert!(effective.is_enabled(Feature::Loops));
        assert!(!effective.is_enabled(Feature::Callbacks));
        assert!(!effective.is_enabled(Feature::ArrayLiterals));
        assert!(effective.is_enabled(Feature::Conditionals));
        assert_eq!(config, before);
        assert!(!config.features().is_enabled(Feature::Loops));
    }

    #[test]
    fn an_unknown_toggle_is_refused_naming_the_closed_set() {
        for name in [
            "variables",
            "return",
            "operators",
            "global_variables",
            "property_access",
        ] {
            let value = features(map(vec![(name, Value::Boolean(false))]));
            assert_eq!(
                refusal(&value, "config"),
                format!(
                    "`config.features.{name}` is not a known feature toggle (the toggles are \
                     loops, conditionals, callbacks, object_literals, array_literals, rpc_calls)"
                )
            );
        }
    }

    #[test]
    fn a_non_boolean_is_refused_naming_the_path() {
        let cases = [
            (Value::from(0), "0"),
            (s("no"), "a string"),
            (Value::Nil, "nil"),
            (map(vec![]), "a map"),
        ];
        for (value, found) in cases {
            let value = features(map(vec![("loops", value)]));
            assert_eq!(
                refusal(&value, "overrides"),
                format!("`overrides.features.loops` must be a boolean; found {found}")
            );
        }
    }

    #[test]
    fn malformed_features_maps_are_refused() {
        assert_eq!(
            refusal(&features(Value::Boolean(false)), "config"),
            "`config.features` is not a map"
        );
        assert_eq!(
            refusal(
                &features(Value::Map(vec![(Value::from(1), Value::Boolean(true))])),
                "config"
            ),
            "`config.features` has a key that is not a string"
        );
        assert_eq!(
            refusal(
                &features(map(vec![
                    ("loops", Value::Boolean(true)),
                    ("loops", Value::Boolean(false)),
                ])),
                "config"
            ),
            "`config.features` repeats the key `loops`"
        );
        assert_eq!(
            refusal(
                &map(vec![("features", map(vec![])), ("features", map(vec![]))]),
                "config"
            ),
            "`config` repeats the key `features`"
        );
        // Only the root holds toggles, and a dotted key never names one.
        assert_eq!(
            refusal(&map(vec![("budget", features(map(vec![])))]), "config"),
            "`config.budget.features` is not a known setting"
        );
        assert_eq!(
            refusal(
                &map(vec![("features.loops", Value::Boolean(false))]),
                "config"
            ),
            "`config.features.loops` is not a known setting"
        );
    }
}

// --- Story 3.10: the check mode in the same decoder, and findings on the error body ---

mod check_mode {
    use hexput_port::{
        CheckMode, ErrorBody, ProtocolCode, ProtocolError, Settings, Value, WireSpan,
        decode_settings, findings_value,
    };

    use super::{map, s};

    fn refusal(value: &Value, prefix: &str) -> String {
        decode_settings(value, prefix).expect_err("refused")
    }

    #[test]
    fn each_mode_decodes_and_absent_is_off() {
        assert_eq!(
            decode_settings(&map(vec![]), "config").unwrap().check(),
            CheckMode::Off
        );
        for mode in CheckMode::ALL {
            let settings =
                decode_settings(&map(vec![("check", s(mode.as_str()))]), "overrides").unwrap();
            assert_eq!(settings.check_setting(), Some(*mode));
        }
        let mixed = decode_settings(
            &map(vec![
                ("check", s("warn")),
                ("argument_depth", Value::from(3)),
            ]),
            "config",
        )
        .unwrap();
        assert_eq!(mixed.check(), CheckMode::Warn);
        assert_ne!(mixed, Settings::new());
    }

    #[test]
    fn anything_but_a_mode_is_refused_naming_the_path() {
        let expected = |prefix: &str, found: &str| {
            format!("`{prefix}.check` must be one of \"off\", \"warn\", \"error\"; found {found}")
        };
        let cases = [
            (s("strict"), "\"strict\""),
            (s("Off"), "\"Off\""),
            (Value::from(1), "1"),
            (Value::Boolean(true), "a boolean"),
            (Value::Nil, "nil"),
            (Value::F64(1.0), "a float"),
            (map(vec![]), "a map"),
        ];
        for (value, found) in cases {
            for prefix in ["config", "overrides"] {
                assert_eq!(
                    refusal(&map(vec![("check", value.clone())]), prefix),
                    expected(prefix, found)
                );
            }
        }
        // A string that is not UTF-8 is named, never echoed.
        let invalid =
            hexput_port::rmpv::decode::read_value(&mut &[0xa2_u8, 0xff, 0xfe][..]).unwrap();
        assert!(matches!(&invalid, Value::String(text) if text.as_str().is_none()));
        assert_eq!(
            refusal(&map(vec![("check", invalid)]), "config"),
            expected("config", "a string that is not valid UTF-8")
        );
        // A long string is echoed bounded.
        let long = "x".repeat(10_000);
        let message = refusal(&map(vec![("check", s(&long))]), "config");
        assert!(message.len() < 200 && message.contains('…'), "{message}");
        assert_eq!(
            refusal(
                &map(vec![("check", s("off")), ("check", s("off"))]),
                "config"
            ),
            "`config` repeats the key `check`"
        );
        // Only the root holds the mode.
        assert_eq!(
            refusal(
                &map(vec![("budget", map(vec![("check", s("off"))]))]),
                "config"
            ),
            "`config.budget.check` is not a known setting"
        );
    }

    fn keys(value: &Value) -> Vec<String> {
        let Value::Map(fields) = value else {
            panic!("a map")
        };
        fields
            .iter()
            .map(|(k, _)| k.as_str().unwrap().to_owned())
            .collect()
    }

    #[test]
    fn a_body_carries_findings_only_when_it_has_some() {
        let plain = ErrorBody::from(&ProtocolError::new(ProtocolCode::InvalidPayload, "x"));
        assert!(plain.findings.is_empty());
        assert_eq!(
            keys(&plain.to_value()),
            ["severity", "category", "code", "message", "span"]
        );
        let decoded: ErrorBody = hexput_port::rmpv::ext::from_value(plain.to_value()).unwrap();
        assert_eq!(decoded, plain, "an absent `findings` decodes as none");

        let finding = ErrorBody {
            severity: "warning".into(),
            category: "reference".into(),
            code: "reference.unused_variable".into(),
            message: "unused".into(),
            span: Some(WireSpan {
                offset: 4,
                len: 1,
                line: 1,
                column: 5,
            }),
            findings: Vec::new(),
        };
        let mut rejection = finding.clone();
        rejection.severity = "error".into();
        rejection.findings = vec![finding.clone(), finding.clone()];
        let value = rejection.to_value();
        assert_eq!(
            keys(&value),
            [
                "severity", "category", "code", "message", "span", "findings"
            ]
        );
        let decoded: ErrorBody = hexput_port::rmpv::ext::from_value(value).unwrap();
        assert_eq!(decoded, rejection);
        assert_eq!(
            findings_value(&rejection.findings),
            Value::Array(vec![finding.to_value(), finding.to_value()])
        );
    }
}
