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
        "protocol.not_implemented",
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
