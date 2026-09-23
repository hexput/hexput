#[test]
fn syntax_codes_are_stable_and_distinct() {
    use hexput_shared::diagnostics::Code;
    assert_eq!(Code::EXPECTED_SYNTAX.as_str(), "syntax.expected_syntax");
    assert_eq!(
        Code::INVALID_ASSIGNMENT_TARGET.as_str(),
        "syntax.invalid_assignment_target"
    );
    assert_eq!(
        Code::DUPLICATE_DECLARATION.as_str(),
        "syntax.duplicate_declaration"
    );
}

#[test]
fn loop_context_code_is_stable() {
    assert_eq!(
        hexput_shared::diagnostics::Code::LOOP_CONTROL_OUTSIDE_LOOP.as_str(),
        "syntax.loop_control_outside_loop"
    );
}

#[test]
fn duplicate_object_key_code_is_stable() {
    assert_eq!(
        hexput_shared::diagnostics::Code::DUPLICATE_OBJECT_KEY.as_str(),
        "syntax.duplicate_object_key"
    );
}

#[test]
fn runtime_codes_are_stable_and_distinct() {
    use hexput_shared::diagnostics::Code;
    let codes = [
        (Code::OPERAND_MISMATCH, "type.operand_mismatch"),
        (Code::INVALID_INDEX, "type.invalid_index"),
        (
            Code::INVALID_PROPERTY_ACCESS,
            "type.invalid_property_access",
        ),
        (Code::CYCLIC_RESULT, "type.cyclic_result"),
        (
            Code::UNDECLARED_IDENTIFIER,
            "reference.undeclared_identifier",
        ),
        (
            Code::UNDECLARED_ASSIGNMENT,
            "reference.undeclared_assignment",
        ),
        (Code::NULL_ACCESS, "reference.null_access"),
        (Code::INDEX_OUT_OF_RANGE, "reference.index_out_of_range"),
        (Code::DIVISION_BY_ZERO, "arithmetic.division_by_zero"),
        (Code::NON_FINITE, "arithmetic.non_finite"),
        (Code::NOT_CALLABLE, "type.not_callable"),
        (Code::FUNCTION_RESULT, "type.function_result"),
        (Code::ARGUMENT_COUNT, "arity.argument_count"),
        (Code::CALL_DEPTH_EXCEEDED, "depth.call_depth_exceeded"),
        (Code::COLLECTION_MUTATED, "reference.collection_mutated"),
    ];
    for (i, (code, text)) in codes.iter().enumerate() {
        assert_eq!(code.as_str(), *text);
        for (other, _) in &codes[i + 1..] {
            assert_ne!(code, other);
        }
    }
}

#[test]
fn finding_codes_are_stable_and_distinct() {
    use hexput_shared::diagnostics::Code;
    let codes = [
        (Code::UNREACHABLE_CODE, "syntax.unreachable_code"),
        (Code::UNUSED_VARIABLE, "reference.unused_variable"),
        (Code::UNKNOWN_FUNCTION, "capability.unknown_function"),
        (Code::CONSTRUCT_DISABLED, "policy.construct_disabled"),
    ];
    for (i, (code, text)) in codes.iter().enumerate() {
        assert_eq!(code.as_str(), *text);
        for (other, _) in &codes[i + 1..] {
            assert_ne!(code, other);
        }
    }
}

/// `Code::ALL` is what ties the hand-written enumerations above to the producer sweeps in
/// `tests/{lexer,parser,interpreter,check}.rs`.
///
/// What this catches: a code added to `Code::ALL` but not to this file's list, or to this list
/// but not to `ALL`. What it does **not** catch: a `pub const` code added to neither, since both
/// sides of the comparison are still hand-maintained. Closing that needs a macro declaring the
/// constants and `ALL` together; until then this is a guard against half-updating, not against
/// forgetting entirely.
#[test]
fn every_code_is_enumerated_exactly_once() {
    use hexput_shared::diagnostics::Code;
    let enumerated = [
        "syntax.expected_syntax",
        "syntax.invalid_assignment_target",
        "syntax.duplicate_declaration",
        "syntax.loop_control_outside_loop",
        "syntax.duplicate_object_key",
        "lex.unterminated_string",
        "lex.unterminated_comment",
        "lex.unknown_character",
        "lex.non_ascii_identifier",
        "lex.invalid_escape",
        "lex.invalid_unicode_escape",
        "lex.invalid_number",
        "type.operand_mismatch",
        "type.invalid_index",
        "type.invalid_property_access",
        "type.cyclic_result",
        "reference.undeclared_identifier",
        "reference.undeclared_assignment",
        "reference.null_access",
        "reference.index_out_of_range",
        "arithmetic.division_by_zero",
        "arithmetic.non_finite",
        "type.not_callable",
        "type.function_result",
        "arity.argument_count",
        "depth.call_depth_exceeded",
        "reference.collection_mutated",
        "syntax.unreachable_code",
        "reference.unused_variable",
        "capability.unknown_function",
        "policy.construct_disabled",
    ];

    let mut all: Vec<&str> = Code::ALL.iter().map(|code| code.as_str()).collect();
    let mut expected: Vec<&str> = enumerated.to_vec();
    all.sort_unstable();
    expected.sort_unstable();
    assert_eq!(
        all, expected,
        "a code was added to one list and not the other"
    );

    let mut seen = all.clone();
    seen.dedup();
    assert_eq!(seen, all, "`Code::ALL` lists a code twice");

    // Every code's string is `category.name`, and the category is one the §7 set names.
    let categories = [
        "syntax",
        "lex",
        "type",
        "reference",
        "arity",
        "arithmetic",
        "depth",
        "capability",
        "budget",
        "policy",
    ];
    for code in Code::ALL {
        let (prefix, rest) = code
            .as_str()
            .split_once('.')
            .unwrap_or_else(|| panic!("`{code}` is not `category.name`"));
        assert!(categories.contains(&prefix), "`{code}` has no §7 category");
        assert!(!rest.is_empty(), "`{code}` has an empty name");
    }
}

// --- Story 2.2: Client ID and the wire envelope ---

#[test]
fn client_id_text_form_is_32_lowercase_hex_and_round_trips() {
    use hexput_shared::ids::ClientId;
    let bytes = [
        0x00, 0x01, 0x23, 0x45, 0x67, 0x89, 0xab, 0xcd, 0xef, 0x10, 0x32, 0x54, 0x76, 0x98, 0xba,
        0xfe,
    ];
    let id = ClientId::from_bytes(bytes);
    let text = id.to_string();
    assert_eq!(text, "000123456789abcdef1032547698bafe");
    assert_eq!(text.len(), ClientId::TEXT_LEN);
    assert_eq!(text.parse::<ClientId>().unwrap(), id);
    assert_eq!(id.as_bytes(), &bytes);
}

#[test]
fn client_id_rejects_malformed_text() {
    use hexput_shared::ids::ClientId;
    let good = "000123456789abcdef1032547698bafe";
    let cases = [
        String::new(),
        good[..31].to_owned(),
        format!("{good}0"),
        good.to_uppercase(),
        format!("{}g", &good[..31]),
        format!("-{}", &good[..31]),
        format!("{}é", &good[..30]),
        format!(" {}", &good[..31]),
    ];
    for case in cases {
        let err = case.parse::<ClientId>().unwrap_err();
        assert!(err.to_string().contains("32 lowercase hex"), "{case:?}");
    }
}

#[test]
fn client_id_serializes_as_a_string() {
    use hexput_shared::ids::ClientId;
    let id = ClientId::from_bytes([0xab; 16]);
    let bytes = rmp_serde::to_vec(&id).unwrap();
    let value = rmpv::decode::read_value(&mut bytes.as_slice()).unwrap();
    assert_eq!(value.as_str(), Some("abababababababababababababababab"));
    assert_eq!(rmp_serde::from_slice::<ClientId>(&bytes).unwrap(), id);

    let malformed = rmp_serde::to_vec("not-a-client-id").unwrap();
    assert!(rmp_serde::from_slice::<ClientId>(&malformed).is_err());
    let integer = rmp_serde::to_vec(&7_u64).unwrap();
    assert!(rmp_serde::from_slice::<ClientId>(&integer).is_err());
}

#[test]
fn message_types_spell_pascal_case_and_parse_exactly() {
    use hexput_shared::wire::MessageType;
    let spellings: Vec<_> = MessageType::ALL.iter().map(|t| t.as_str()).collect();
    assert_eq!(spellings, ["Init", "ExecutionStart", "Result", "Error"]);
    for t in MessageType::ALL {
        assert_eq!(t.as_str().parse::<MessageType>().unwrap(), *t);
        assert_eq!(t.to_string(), t.as_str());
    }
    let err = "Frobnicate".parse::<MessageType>().unwrap_err();
    assert_eq!(err.0, "Frobnicate");
    assert!("executionstart".parse::<MessageType>().is_err());
}

#[test]
fn typed_envelope_encodes_as_a_named_map() {
    use hexput_shared::wire::{CorrelationId, Envelope, MessageType};

    #[derive(serde::Serialize)]
    struct Payload {
        script: &'static str,
    }

    let envelope = Envelope::new(
        CorrelationId(9),
        MessageType::ExecutionStart,
        Payload { script: "1" },
    );
    let bytes = rmp_serde::to_vec_named(&envelope).unwrap();
    let value = rmpv::decode::read_value(&mut bytes.as_slice()).unwrap();
    let s = rmpv::Value::from;
    assert_eq!(
        value,
        rmpv::Value::Map(vec![
            (s("id"), rmpv::Value::from(9)),
            (s("type"), s("ExecutionStart")),
            (s("payload"), rmpv::Value::Map(vec![(s("script"), s("1"))])),
        ])
    );
}
