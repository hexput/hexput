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
