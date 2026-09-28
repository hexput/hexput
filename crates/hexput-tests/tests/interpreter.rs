use hexput_ast::{Category, Code, Diagnostic};
use hexput_interpreter::{
    Array, BUILTINS, Execution, Object, Value, evaluate, evaluate_with_variables,
};
use hexput_parser::parse;

fn eval(source: &str) -> Result<Value, Diagnostic> {
    let program = parse(source).unwrap_or_else(|e| panic!("`{source}` should parse: {e}"));
    evaluate(&program)
}

fn ok(source: &str) -> Value {
    eval(source).unwrap_or_else(|e| panic!("`{source}` should evaluate: {e}"))
}

fn err(source: &str) -> Diagnostic {
    match eval(source) {
        Ok(v) => panic!("`{source}` should fail, got {v:?}"),
        Err(e) => e,
    }
}

fn num(source: &str) -> f64 {
    ok(source)
        .as_number()
        .unwrap_or_else(|| panic!("`{source}` should yield a number"))
}

fn text(source: &str) -> String {
    ok(source)
        .as_str()
        .unwrap_or_else(|| panic!("`{source}` should yield a string"))
        .to_owned()
}

fn boolean(source: &str) -> bool {
    ok(source)
        .as_bool()
        .unwrap_or_else(|| panic!("`{source}` should yield a bool"))
}

/// Assert category, code, and that the span covers exactly the source text `spanned`.
fn assert_error(source: &str, category: Category, code: Code, spanned: &str) -> Diagnostic {
    let e = err(source);
    assert_eq!(e.category, category, "{source}: {e}");
    assert_eq!(e.code, code, "{source}: {e}");
    assert_eq!(&source[e.span.range()], spanned, "{source}: {e}");
    e
}

#[test]
fn values_are_send_and_sync() {
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<Value>();
}

#[test]
fn mixed_addition() {
    assert_eq!(text(r#"return "Total: " + 5;"#), "Total: 5");
    assert_eq!(text(r#"return "10" + 5;"#), "105");
    assert_eq!(num("return true + 1;"), 2.0);
    assert_eq!(text(r#"return "Order " + null;"#), "Order null");
    assert_eq!(text(r#"return "a" + true + false;"#), "atruefalse");
    assert_eq!(num("return null + 1;"), 1.0);
    assert_eq!(num("return 0.1 + 0.2;"), 0.1 + 0.2);
}

#[test]
fn arithmetic_coercion() {
    assert_eq!(num(r#"return "10" - 1;"#), 9.0);
    assert_eq!(num("return true * 3;"), 3.0);
    assert_eq!(num(r#"return " 2 " * 2;"#), 4.0);
    assert_eq!(num(r#"return "-3" - 1;"#), -4.0);
    assert_eq!(num(r#"return "+3" - 1;"#), 2.0);
    assert_eq!(num(r#"return "\n1.5e2\t" / 3;"#), 50.0);
    assert_eq!(num("return 7 % 3;"), 1.0);
    assert_eq!(num("return -7 % 3;"), -1.0);
    assert_eq!(num("return -true;"), -1.0);
    assert_eq!(num(r#"return -"4";"#), -4.0);
    assert_eq!(num("return 2 + 3 * 4 - 6 / 2;"), 11.0);
}

#[test]
fn bad_conversions_are_type_errors_naming_both_types() {
    let e = assert_error(
        r#"return "abc" * 2;"#,
        Category::Type,
        Code::OPERAND_MISMATCH,
        r#""abc""#,
    );
    assert!(e.message.contains("string") && e.message.contains("number"));
    let e = assert_error(
        "return [] - 1;",
        Category::Type,
        Code::OPERAND_MISMATCH,
        "[]",
    );
    assert!(e.message.contains("array") && e.message.contains("number"));
    let e = assert_error(
        r#"return "x" + [1];"#,
        Category::Type,
        Code::OPERAND_MISMATCH,
        "[1]",
    );
    assert!(e.message.contains("string") && e.message.contains("array"));
    let e = assert_error("return -{};", Category::Type, Code::OPERAND_MISMATCH, "{}");
    assert!(e.message.contains("object"));
    assert_error(
        r#"return {} + "x";"#,
        Category::Type,
        Code::OPERAND_MISMATCH,
        "{}",
    );
    assert_error(
        "return 1 + [];",
        Category::Type,
        Code::OPERAND_MISMATCH,
        "[]",
    );
    for bad in [
        "", " ", "NaN", "Infinity", "0x10", ".5", "5.", "1e", "--1", "- 1", "1e400",
    ] {
        let source = format!("return {bad:?} * 1;");
        let e = err(&source);
        assert_eq!(e.code, Code::OPERAND_MISMATCH, "{source}");
        assert_eq!(e.category, Category::Type, "{source}");
    }
}

#[test]
fn non_finite_results_are_arithmetic_errors() {
    assert_error(
        "return 1 / 0;",
        Category::Arithmetic,
        Code::DIVISION_BY_ZERO,
        "0",
    );
    assert_error(
        "return 0 % 0;",
        Category::Arithmetic,
        Code::DIVISION_BY_ZERO,
        "0",
    );
    let e = err(r#"return 1 / "0";"#);
    assert_eq!(e.code, Code::DIVISION_BY_ZERO);
    let e = err("return 1 / -0;");
    assert_eq!(e.code, Code::DIVISION_BY_ZERO);
    let source = "return 1e308 * 10;";
    let e = err(source);
    assert_eq!(e.category, Category::Arithmetic);
    assert_eq!(e.code, Code::NON_FINITE);
    assert_eq!(&source[e.span.range()], "*");
    assert_eq!(err("return 1e308 + 1e308;").code, Code::NON_FINITE);
    assert_eq!(err("return -1e308 - 1e308;").code, Code::NON_FINITE);
    assert_eq!(err("return 1e308 / 1e-308;").code, Code::NON_FINITE);
}

#[test]
fn equality() {
    assert!(!boolean("return 0 == false;"));
    assert!(!boolean(r#"return "" == false;"#));
    assert!(!boolean("return [] == false;"));
    assert!(boolean(r#"return "5" == 5;"#));
    assert!(boolean(r#"return 5 == " 5 ";"#));
    assert!(!boolean(r#"return "a" == 1;"#));
    assert!(boolean(r#"return "a" != 1;"#));
    assert!(boolean("return null == null;"));
    assert!(!boolean("return null == 0;"));
    assert!(!boolean(r#"return null == "";"#));
    assert!(boolean("let a = []; let b = a; return a == b;"));
    assert!(boolean("let a = [1]; return a == a;"));
    assert!(!boolean("return [] == [];"));
    assert!(!boolean("return {} == {};"));
    assert!(boolean("return [] != [];"));
    assert!(boolean(r#"return "x" == "x";"#));
    assert!(boolean("return true == true;"));
    assert!(boolean("return 0 == -0;"));
    assert!(!boolean(r#"return "1" == true;"#));
}

#[test]
fn ordering() {
    assert!(boolean(r#"return "b" > "a";"#));
    assert!(!boolean(r#"return "10" < 9;"#));
    assert!(boolean(r#"return "10" < "9";"#));
    assert!(boolean("return null < 1;"));
    assert!(boolean("return true >= 1;"));
    assert!(boolean("return 2 <= 2;"));
    assert!(boolean(r#"return "a" < "é";"#));
    let e = assert_error(
        r#"return "x" < 1;"#,
        Category::Type,
        Code::OPERAND_MISMATCH,
        r#""x""#,
    );
    assert!(e.message.contains("string") && e.message.contains("number"));
    assert_error(
        "return 1 < [];",
        Category::Type,
        Code::OPERAND_MISMATCH,
        "[]",
    );
}

#[test]
fn logic_short_circuits_and_returns_operands() {
    assert_eq!(text(r#"return null || "u";"#), "u");
    assert_eq!(text(r#"return "a" || nope;"#), "a");
    assert_eq!(num("return 0 && x_undeclared;"), 0.0);
    assert!(ok("return [] && nope;").as_array().is_some());
    assert_eq!(num("return 1 && 2;"), 2.0);
    assert_eq!(num("return 0 || 0 || 3;"), 3.0);
    assert!(ok("return {} || null;").is_null());
    assert!(boolean(r#"return !"";"#));
    assert!(!boolean(r#"return !"a";"#));
    assert!(boolean("return ![];"));
    assert!(!boolean("return ![0];"));
    assert!(boolean("return !{};"));
    assert!(boolean("return !null;"));
    assert!(boolean("return !0;"));
    assert!(!boolean("return !-1;"));
    // The decided side never runs; the undecided side does.
    assert_error(
        "return 1 && nope;",
        Category::Reference,
        Code::UNDECLARED_IDENTIFIER,
        "nope",
    );
    assert_error(
        "return 0 || nope;",
        Category::Reference,
        Code::UNDECLARED_IDENTIFIER,
        "nope",
    );
}

#[test]
fn operands_evaluate_left_to_right() {
    assert_error(
        "return first + second;",
        Category::Reference,
        Code::UNDECLARED_IDENTIFIER,
        "first",
    );
    assert_error(
        "return [a1, a2];",
        Category::Reference,
        Code::UNDECLARED_IDENTIFIER,
        "a1",
    );
    assert_error(
        "return {x: b1, y: b2};",
        Category::Reference,
        Code::UNDECLARED_IDENTIFIER,
        "b1",
    );
    // Assignment: receiver, then key, then value.
    assert_error(
        "u[k] = v;",
        Category::Reference,
        Code::UNDECLARED_IDENTIFIER,
        "u",
    );
    assert_error(
        "let o = {}; o[k] = v;",
        Category::Reference,
        Code::UNDECLARED_IDENTIFIER,
        "k",
    );
    assert_error(
        "let o = {}; o.p = v;",
        Category::Reference,
        Code::UNDECLARED_IDENTIFIER,
        "v",
    );
}

#[test]
fn shadowing_reads_inner_and_restores_outer() {
    let source = "let x = 1; let seen = 0; { let x = 2; seen = x; }; return [x, seen];";
    let result = ok(source);
    let items = result.as_array().expect("array").to_vec();
    assert_eq!(items[0].as_number(), Some(1.0));
    assert_eq!(items[1].as_number(), Some(2.0));
    // Assignment without `let` reaches the outer binding.
    assert_eq!(num("let x = 1; { x = 5; }; return x;"), 5.0);
    // An inner initializer reads the outer binding it shadows.
    assert_eq!(num("let x = 1; { let x = x + 1; return x; }"), 2.0);
    // Inner declarations do not leak out of their block.
    assert_error(
        "{ let inner = 1; }; return inner;",
        Category::Reference,
        Code::UNDECLARED_IDENTIFIER,
        "inner",
    );
}

#[test]
fn undeclared_names_are_reference_errors() {
    let e = assert_error(
        "return nope;",
        Category::Reference,
        Code::UNDECLARED_IDENTIFIER,
        "nope",
    );
    assert!(e.message.contains("nope"));
    let e = assert_error(
        "nope = 1;",
        Category::Reference,
        Code::UNDECLARED_ASSIGNMENT,
        "nope",
    );
    assert!(e.message.contains("nope"));
}

#[test]
fn null_access_is_a_reference_error_naming_the_null_link() {
    let e = assert_error(
        "let o = {c: null}; return o.c.name;",
        Category::Reference,
        Code::NULL_ACCESS,
        ".name",
    );
    assert!(e.message.contains("`c`"), "{}", e.message);
    let e = assert_error(
        "let a = null; return a.b;",
        Category::Reference,
        Code::NULL_ACCESS,
        ".b",
    );
    assert!(e.message.contains("`a`"));
    // The key is evaluated first (Story 3.11: `null["__secret"]` reads as null whatever spells the
    // key), so an undeclared key is its own error and a declared one reaches the null access.
    let e = assert_error(
        "let a = null; let nope = 1; return a[nope];",
        Category::Reference,
        Code::NULL_ACCESS,
        "[nope]",
    );
    assert!(e.message.contains("`a`"));
    assert_error(
        "let a = null; return a[nope];",
        Category::Reference,
        Code::UNDECLARED_IDENTIFIER,
        "nope",
    );
    let e = assert_error(
        "let o = {c: null}; o.c.name = 1;",
        Category::Reference,
        Code::NULL_ACCESS,
        ".name",
    );
    assert!(e.message.contains("`c`"));
    assert_error(
        "let o = {c: null}; o.c[0] = 1;",
        Category::Reference,
        Code::NULL_ACCESS,
        "[0]",
    );
    // A null mid-way through an assignment target's receiver: `?.` is not allowed there, so
    // the message must not suggest it.
    let e = assert_error(
        "let o = {c: null}; o.c.d.e = 1;",
        Category::Reference,
        Code::NULL_ACCESS,
        ".d",
    );
    assert!(e.message.contains("`c`"), "{}", e.message);
    assert!(!e.message.contains("?."), "{}", e.message);
    let e = assert_error(
        "let o = {c: null}; o.c[0].e = 1;",
        Category::Reference,
        Code::NULL_ACCESS,
        "[0]",
    );
    assert!(!e.message.contains("?."), "{}", e.message);
    // Plain reads still suggest it.
    let e = err("let o = {c: null}; return o.c.d;");
    assert!(e.message.contains("?."), "{}", e.message);
    assert_error(
        "return null.x;",
        Category::Reference,
        Code::NULL_ACCESS,
        ".x",
    );
}

#[test]
fn absent_data_reads_null_and_writes_create() {
    assert!(ok("let o = {}; return o.missing;").is_null());
    assert!(ok("return [1][5];").is_null());
    assert!(ok("return [1][-1];").is_null());
    assert!(ok("return [1][0.5];").is_null());
    assert!(ok(r#"return {a: 1}["b"];"#).is_null());
    assert_eq!(num(r#"return {a: 1}["a"];"#), 1.0);
    let result = ok("let o = {a: 1, b: 2}; o.k = 1; o.a = 3; o[\"z\"] = 4; return o;");
    let entries = result.as_object().expect("object").entries();
    let keys: Vec<&str> = entries.iter().map(|(k, _)| k.as_ref()).collect();
    assert_eq!(keys, ["a", "b", "k", "z"]);
    assert_eq!(entries[0].1.as_number(), Some(3.0));
    assert_eq!(entries[2].1.as_number(), Some(1.0));
    // Every literal value pairs with its own key.
    let items = ok("let o = {a: 1, b: 2, c: 3}; return [o.a, o.b, o.c];")
        .as_array()
        .expect("array")
        .to_vec();
    let numbers: Vec<f64> = items.iter().filter_map(Value::as_number).collect();
    assert_eq!(numbers, [1.0, 2.0, 3.0]);
    // Collections are shared by reference.
    assert_eq!(num("let a = {n: 1}; let b = a; b.n = 2; return a.n;"), 2.0);
    assert_eq!(num("let o = {p: {q: 1}}; o.p.q = 7; return o.p.q;"), 7.0);
}

#[test]
fn array_writes() {
    let items = ok("let a = [1, 2]; a[0] = 9; a[2] = 3; return a;")
        .as_array()
        .expect("array")
        .to_vec();
    let numbers: Vec<f64> = items.iter().filter_map(Value::as_number).collect();
    assert_eq!(numbers, [9.0, 2.0, 3.0]);
    for (source, spanned) in [
        ("let a = [1]; a[3] = 0;", "3"),
        ("let a = [1]; a[-1] = 0;", "-1"),
        ("let a = [1]; a[0.5] = 0;", "0.5"),
        ("let a = []; a[1] = 0;", "1"),
    ] {
        assert_error(
            source,
            Category::Reference,
            Code::INDEX_OUT_OF_RANGE,
            spanned,
        );
    }
    assert_error(
        r#"let a = []; a["0"] = 1;"#,
        Category::Type,
        Code::INVALID_INDEX,
        r#""0""#,
    );
    assert_error(
        "let o = {}; o[0] = 1;",
        Category::Type,
        Code::INVALID_INDEX,
        "0",
    );
    assert_error(
        "let s = \"ab\"; s[0] = 1;",
        Category::Type,
        Code::INVALID_INDEX,
        "[0]",
    );
    assert_error(
        "let n = 1; n.x = 1;",
        Category::Type,
        Code::INVALID_PROPERTY_ACCESS,
        ".x",
    );
    assert_error(
        "let a = []; a.x = 1;",
        Category::Type,
        Code::INVALID_PROPERTY_ACCESS,
        ".x",
    );
}

#[test]
fn optional_access() {
    assert!(ok("let o = {c: null}; return o.c?.name;").is_null());
    assert!(ok("let a = null; return a?.b.c.d;").is_null());
    assert!(ok("let a = null; return a?.[f];").is_null());
    assert!(ok("let a = null; return a?.[f].g[h];").is_null());
    assert_eq!(num("let a = {b: {c: 4}}; return a?.b.c;"), 4.0);
    assert_eq!(num("let a = [5]; return a?.[0];"), 5.0);
    // Grouping ends the chain the `?.` short-circuits.
    assert_error(
        "let a = null; return (a?.b).c;",
        Category::Reference,
        Code::NULL_ACCESS,
        ".c",
    );
    // `?.` suppresses only null, never type errors.
    assert_error(
        "return 1?.x;",
        Category::Type,
        Code::INVALID_PROPERTY_ACCESS,
        "?.x",
    );
    assert_error(
        "return [1]?.[\"0\"];",
        Category::Type,
        Code::INVALID_INDEX,
        "\"0\"",
    );
    // A non-null link after `?.` still raises on a later null.
    assert_error(
        "let a = {b: null}; return a?.b.c;",
        Category::Reference,
        Code::NULL_ACCESS,
        ".c",
    );
}

#[test]
fn index_and_property_types() {
    assert_error(
        r#"return [1]["0"];"#,
        Category::Type,
        Code::INVALID_INDEX,
        r#""0""#,
    );
    assert_error(
        r#"return "ab"[0];"#,
        Category::Type,
        Code::INVALID_INDEX,
        "[0]",
    );
    assert_error(
        "return {a: 1}[0];",
        Category::Type,
        Code::INVALID_INDEX,
        "0",
    );
    assert_error(
        "return true[0];",
        Category::Type,
        Code::INVALID_INDEX,
        "[0]",
    );
    for (source, receiver) in [
        ("let n = 5; return n.x;", "number"),
        ("return true.x;", "bool"),
        (r#"return "s".length;"#, "string"),
        ("return [1].length;", "array"),
    ] {
        let e = err(source);
        assert_eq!(e.category, Category::Type, "{source}");
        assert_eq!(e.code, Code::INVALID_PROPERTY_ACCESS, "{source}");
        assert!(e.message.contains(receiver), "{source}: {}", e.message);
    }
    assert_eq!(num("return [[1, 2], [3]][0][1];"), 2.0);
    assert_eq!(num("return {a: [1, {b: 6}]}.a[1].b;"), 6.0);
}

#[test]
fn script_result() {
    assert!(ok("let x = 1;").is_null());
    assert!(ok("").is_null());
    assert!(ok("return;").is_null());
    assert!(ok("let x = 1; return").is_null());
    assert_eq!(num("{ { return 3; nope; }; nope; }; return 4;"), 3.0);
    assert_eq!(num("let x = 1; { x = 2; return x; }"), 2.0);
    assert_eq!(num("return 1; return 2;"), 1.0);
}

#[test]
fn number_to_string_is_javascript_style() {
    for (literal, expected) in [
        ("5", "5"),
        ("2.5", "2.5"),
        ("-0", "0"),
        ("-12", "-12"),
        ("(0.1 + 0.2)", "0.30000000000000004"),
        ("1e21", "1e+21"),
        ("1.5e21", "1.5e+21"),
        ("123456789012345680000", "123456789012345680000"),
        ("1e-7", "1e-7"),
        ("-1.5e-7", "-1.5e-7"),
        ("0.000001", "0.000001"),
        ("0.00001234", "0.00001234"),
        ("1e300", "1e+300"),
        ("100", "100"),
        ("(1 / 3)", "0.3333333333333333"),
    ] {
        assert_eq!(
            text(&format!(r#"return "" + {literal};"#)),
            expected,
            "{literal}"
        );
    }
}

#[test]
fn only_the_taken_branch_runs_and_each_body_is_its_own_scope() {
    let chain = |first: &str, second: &str| {
        format!(
            "let seen = 0; if ({first}) {{ seen = 1; }} else if ({second}) {{ seen = 2; }} \
             else {{ seen = 3; }}; return seen;"
        )
    };
    assert_eq!(num(&chain("1", "1")), 1.0);
    assert_eq!(num(&chain("0", "1")), 2.0);
    assert_eq!(num(&chain("0", "0")), 3.0);
    // The untaken branches are never evaluated — neither their bodies nor later conditions.
    assert_eq!(
        num("let x = 0; if (1) { x = 1; } else { nope; }; return x;"),
        1.0
    );
    assert_eq!(
        num("let x = 0; if (1) { x = 1; } else if (nope) { }; return x;"),
        1.0
    );
    // No `else`, condition false: nothing happens at all.
    assert!(ok("if (0) { nope; };").is_null());
    // Each body is its own scope: a declaration inside does not leak, and it may shadow.
    assert_error(
        "if (1) { let inner = 1; }; return inner;",
        Category::Reference,
        Code::UNDECLARED_IDENTIFIER,
        "inner",
    );
    assert_eq!(
        num("let x = 1; if (1) { let x = 2; } else { }; return x;"),
        1.0
    );
}

#[test]
fn conditions_follow_truthiness() {
    for falsy in ["null", "false", "0", r#""""#, "[]", "{}", "-0"] {
        let source = format!("let t = 0; if ({falsy}) {{ t = 1; }}; return t;");
        assert_eq!(num(&source), 0.0, "{falsy} should be falsy");
        let looped = format!("let n = 0; while ({falsy}) {{ n = n + 1; break; }}; return n;");
        assert_eq!(num(&looped), 0.0, "{falsy} should be falsy");
    }
    for truthy in ["true", "1", r#""a""#, r#""0""#, "[0]", "{a: 0}", "-1"] {
        let source = format!("let t = 0; if ({truthy}) {{ t = 1; }}; return t;");
        assert_eq!(num(&source), 1.0, "{truthy} should be truthy");
    }
}

#[test]
fn while_accumulates_into_an_outer_binding() {
    assert_eq!(
        num("let i = 0; let sum = 0; while (i < 5) { sum = sum + i; i = i + 1; }; return sum;"),
        10.0
    );
    // A condition that is false from the start never runs the body.
    assert_eq!(num("let n = 0; while (0) { n = 1; }; return n;"), 0.0);
    // The body's scope is fresh each turn: `let` inside it does not collide with itself.
    assert_eq!(
        num(
            "let i = 0; let last = 0; while (i < 3) { let doubled = i * 2; last = doubled; i = i + 1; }; return last;"
        ),
        4.0
    );
}

#[test]
fn break_and_continue_affect_the_innermost_loop_only() {
    assert_eq!(
        num("let i = 0; while (1) { i = i + 1; if (i == 3) { break; }; }; return i;"),
        3.0
    );
    // `continue` re-tests the condition, so the counter must advance before it.
    assert_eq!(
        num(
            "let i = 0; let odd = 0; while (i < 6) { i = i + 1; if (i % 2 == 0) { continue; }; odd = odd + 1; }; return odd;"
        ),
        3.0
    );
    assert_eq!(
        num("let n = 0; for (x in [1, 2, 3, 4]) { if (x == 3) { break; }; n = n + x; }; return n;"),
        3.0
    );
    assert_eq!(
        num("let n = 0; for (x in [1, 2, 3]) { if (x == 2) { continue; }; n = n + x; }; return n;"),
        4.0
    );
    // From inside nested blocks, still the innermost loop.
    assert_eq!(
        num(
            "let n = 0; for (x in [1, 2, 3]) { { { if (x == 2) { continue; }; }; }; n = n + x; }; return n;"
        ),
        4.0
    );
    // Nested loops: the inner `break` leaves only the inner loop running.
    assert_eq!(
        num(
            "let n = 0; for (x in [1, 2]) { for (y in [10, 20]) { break; }; n = n + x; }; return n;"
        ),
        3.0
    );
    assert_eq!(
        num(
            "let n = 0; for (x in [1, 2]) { let i = 0; while (i < 3) { i = i + 1; if (i == 2) { break; }; n = n + 1; }; }; return n;"
        ),
        2.0
    );
}

#[test]
fn for_walks_arrays_in_order_and_objects_by_key() {
    // Elements arrive in order.
    assert_eq!(
        text(r#"let out = ""; for (x in [1, 2, 3]) { out = out + x; }; return out;"#),
        "123"
    );
    // Object keys arrive as strings, in insertion order, including one added before the loop.
    assert_eq!(
        text(
            r#"let o = {b: 1, a: 2}; o.c = 3; let out = ""; for (k in o) { out = out + k; }; return out;"#
        ),
        "bac"
    );
    assert!(boolean(
        r#"let out = null; for (k in {n: 1}) { out = k; }; return out == "n";"#
    ));
    // An empty collection never runs the body.
    assert_eq!(num("let n = 0; for (x in []) { n = 1; }; return n;"), 0.0);
    assert_eq!(num("let n = 0; for (x in {}) { n = 1; }; return n;"), 0.0);
    // The binding is fresh per iteration: assigning it does not disturb the walk.
    assert_eq!(
        text(r#"let out = ""; for (x in [1, 2]) { x = 9; out = out + x; }; return out;"#),
        "99"
    );
    // A non-collection iterable is a type error on the iterable expression.
    for (source, spanned) in [
        ("for (x in 1) {}", "1"),
        (r#"for (x in "ab") {}"#, r#""ab""#),
        ("for (x in null) {}", "null"),
        ("let f = fn() {}; for (x in f) {}", "f"),
    ] {
        assert_error(source, Category::Type, Code::OPERAND_MISMATCH, spanned);
    }
}

#[test]
fn mutating_the_iterated_collection_is_a_reference_error_on_the_loop() {
    for source in [
        "let a = [1, 2]; for (x in a) { a[0] = 1; };",
        "let a = [1]; for (x in a) { a[1] = 2; };",
        "let o = {a: 1}; for (k in o) { o.b = 2; };",
        "let o = {a: 1}; for (k in o) { o.a = 2; };",
        // Detected even when the mutation happens on the loop's last turn.
        "let a = [1]; for (x in a) { a[0] = 7; };",
    ] {
        assert_error(source, Category::Reference, Code::COLLECTION_MUTATED, "for");
    }
    // The guard fires when the loop advances, so mutating and leaving the loop in the same
    // iteration never reaches a check. That is the defined behaviour, not an oversight.
    assert_eq!(
        num("let a = [1, 2]; for (x in a) { a[0] = 9; break; }; return a[0];"),
        9.0
    );
    assert_eq!(
        num("let a = [1, 2]; for (x in a) { a[0] = 9; return a[0]; }; return 0;"),
        9.0
    );
    assert_eq!(
        num("fn f(o) { for (k in o) { o.b = 1; return 5; }; return 0; }; return f({a: 0});"),
        5.0
    );
    // A collection nested inside the iterated one is untouched by the guard.
    assert_eq!(
        num("let a = [[0], [0]]; for (x in a) { x[0] = 5; }; return a[1][0];"),
        5.0
    );
    // So is an unrelated collection, and so is rebinding the variable that named it.
    assert_eq!(
        num("let a = [1, 2]; let b = []; for (x in a) { b[0] = x; }; return b[0];"),
        2.0
    );
    assert_eq!(
        num("let a = [1, 2]; let n = 0; for (x in a) { a = [9]; n = n + x; }; return n;"),
        3.0
    );
}

#[test]
fn calls_bind_parameters_per_invocation() {
    assert_eq!(
        num("fn add(a, b) { return a + b; }; return add(1, 2);"),
        3.0
    );
    // A body that reaches its end without `return` yields null.
    assert!(ok("fn nothing() { }; return nothing();").is_null());
    assert!(ok("fn bare() { return; }; return bare();").is_null());
    // Repeated calls do not leak bindings into each other, and parameters shadow outer names.
    assert_eq!(
        text(r#"fn tag(x) { let local = "-"; return x + local; }; return tag("a") + tag("b");"#),
        "a-b-"
    );
    assert_eq!(
        num("let x = 1; fn shadow(x) { return x; }; let got = shadow(9); return got + x;"),
        10.0
    );
    // Parameters do not leak out of the call.
    assert_error(
        "fn f(p) { return p; }; f(1); return p;",
        Category::Reference,
        Code::UNDECLARED_IDENTIFIER,
        "p",
    );
    // Recursion keeps one set of bindings per invocation.
    assert_eq!(
        num("fn fact(n) { if (n <= 1) { return 1; }; return n * fact(n - 1); }; return fact(6);"),
        720.0
    );
    assert_eq!(
        num(
            "fn fib(n) { if (n < 2) { return n; }; return fib(n - 1) + fib(n - 2); }; return fib(15);"
        ),
        610.0
    );
    // Named functions hoist within their block, so declaration order does not matter.
    assert_eq!(num("return first(3); fn first(n) { return n * 2; }"), 6.0);
    assert_eq!(
        num(
            "fn even(n) { if (n == 0) { return 1; }; return odd(n - 1); }; fn odd(n) { if (n == 0) { return 0; }; return even(n - 1); }; return even(8);"
        ),
        1.0
    );
    // Arguments are evaluated left to right, after the callee.
    assert_error(
        "fn f(a, b) { return a; }; return f(x1, x2);",
        Category::Reference,
        Code::UNDECLARED_IDENTIFIER,
        "x1",
    );
    // Story 3.1: calling an undeclared name is a host call, whose arguments are evaluated before
    // anything is decided about the call itself.
    assert_error(
        "return missingFn(x1);",
        Category::Reference,
        Code::UNDECLARED_IDENTIFIER,
        "x1",
    );
    assert_error(
        "return missingFn(1);",
        Category::Capability,
        Code::UNKNOWN_FUNCTION,
        "missingFn(1)",
    );
}

#[test]
fn functions_are_values_and_callbacks_work() {
    assert_eq!(
        num("fn apply(f, v) { return f(v); }; return apply(fn(x) { return x * 2; }, 21);"),
        42.0
    );
    // Held in a binding, an array, and an object, then called from there.
    assert_eq!(num("let f = fn(x) { return x + 1; }; return f(1);"), 2.0);
    assert_eq!(num("let a = [fn() { return 7; }]; return a[0]();"), 7.0);
    assert_eq!(num("let o = {m: fn() { return 8; }}; return o.m();"), 8.0);
    // A call's result continues the access chain it sits in.
    assert_eq!(
        num("fn make() { return {v: [0, 9]}; }; return make().v[1];"),
        9.0
    );
    assert_eq!(
        num("fn outer() { return fn(x) { return x - 1; }; }; return outer()(4);"),
        3.0
    );
    // A function returned from a call, stored, then invoked later.
    assert_eq!(
        num(
            "fn adder(n) { return fn(x) { return x + n; }; }; let add5 = adder(5); return add5(2);"
        ),
        7.0
    );
    // Truthiness and identity of function values.
    assert!(boolean("let f = fn() {}; return !!f;"));
    assert!(boolean("let f = fn() {}; let g = f; return f == g;"));
    assert!(!boolean("return fn() {} == fn() {};"));
    assert_eq!(text("let f = fn() {}; return \"\" + (f == f);"), "true");
}

#[test]
fn closures_capture_by_reference() {
    // The callback sees a mutation made after it was created.
    assert_eq!(
        num("let n = 1; let get = fn() { return n; }; n = 42; return get();"),
        42.0
    );
    // And a mutation the callback itself makes is visible outside.
    assert_eq!(
        num("let n = 0; let bump = fn() { n = n + 1; }; bump(); bump(); return n;"),
        2.0
    );
    // The captured scope is the defining one, never the caller's.
    assert_error(
        "let f = fn() { return caller_local; }; fn run(g) { let caller_local = 1; return g(); }; return run(f);",
        Category::Reference,
        Code::UNDECLARED_IDENTIFIER,
        "caller_local",
    );
    // A closure made in a block outlives the block.
    assert_eq!(
        num("let get = null; { let hidden = 5; get = fn() { return hidden; }; }; return get();"),
        5.0
    );
    // The binding lives in a strict, non-root *ancestor* of the closure's own scope, and that
    // ancestor exits while the closure is still callable: the whole parent chain must be
    // retained, not just the defining scope.
    assert_eq!(
        num("let g = null; { let a = 1; { g = fn() { return a; }; }; }; return g();"),
        1.0
    );
    // The same, with sibling blocks afterwards: a wrongly freed ancestor slot would be handed
    // straight back out to them, and the capture would read their bindings or fail to resolve.
    assert_eq!(
        num(
            "let g = null; { let a = 2; { g = fn() { return a; }; }; }; { let b = 3; { let c = 4; }; }; { let d = 5; }; return g();"
        ),
        2.0
    );
    // Each loop iteration gets a fresh scope, so each closure captures its own value.
    assert_eq!(
        text(
            r#"let fs = []; for (x in [1, 2, 3]) { fs[x - 1] = fn() { return x; }; }; return "" + fs[0]() + fs[1]() + fs[2]();"#
        ),
        "123"
    );
    assert_eq!(
        text(
            r#"let fs = []; let i = 0; while (i < 3) { let captured = i; fs[i] = fn() { return captured; }; i = i + 1; }; return "" + fs[0]() + fs[1]() + fs[2]();"#
        ),
        "012"
    );
}

#[test]
fn a_function_in_an_operand_position_is_a_type_error() {
    let with = |rest: &str| format!("let f = fn() {{}}; return {rest};");
    for (rest, code, spanned) in [
        ("f + 1", Code::OPERAND_MISMATCH, "f"),
        (r#""x" + f"#, Code::OPERAND_MISMATCH, "f"),
        ("-f", Code::OPERAND_MISMATCH, "f"),
        ("f < f", Code::OPERAND_MISMATCH, "f"),
        ("f * 2", Code::OPERAND_MISMATCH, "f"),
        ("f.x", Code::INVALID_PROPERTY_ACCESS, ".x"),
        ("f[0]", Code::INVALID_INDEX, "[0]"),
        ("[1][f]", Code::INVALID_INDEX, "f"),
    ] {
        let e = assert_error(&with(rest), Category::Type, code, spanned);
        assert!(e.message.contains("function"), "{rest}: {e}");
    }
    // Identity comparison is not a conversion, so it still works on function values.
    assert!(boolean("let f = fn() {}; return f == f;"));
}

#[test]
fn wrong_arity_is_an_arity_error_on_the_call() {
    for (source, spanned) in [
        ("fn add(a, b) { return a + b; }; return add(1);", "(1)"),
        (
            "fn add(a, b) { return a + b; }; return add(1, 2, 3);",
            "(1, 2, 3)",
        ),
        ("fn none() { }; return none(1);", "(1)"),
        ("let f = fn(a) { }; return f();", "()"),
    ] {
        let e = assert_error(source, Category::Arity, Code::ARGUMENT_COUNT, spanned);
        assert!(e.message.contains("argument"), "{source}: {e}");
    }
}

#[test]
fn calling_a_non_function_is_a_type_error() {
    for (source, spanned) in [
        ("let x = 1; x();", "()"),
        ("let o = {}; o.missing();", "()"),
        (r#"let s = "a"; s();"#, "()"),
        ("let a = []; a[0]();", "()"),
        ("null();", "()"),
    ] {
        let e = assert_error(source, Category::Type, Code::NOT_CALLABLE, spanned);
        assert!(e.message.contains("only functions"), "{source}: {e}");
    }
    // `?.` short-circuits the chain before the call is ever reached.
    assert!(ok("let a = null; return a?.b();").is_null());
}

#[test]
fn unbounded_recursion_ends_in_a_depth_error() {
    for source in [
        "fn f() { return f(); }; return f();",
        "fn a() { return b(); }; fn b() { return a(); }; return a();",
        "let f = null; f = fn() { return f(); }; return f();",
        // Recursion through a callback argument is bounded the same way.
        "fn run(g) { return g(g); }; return run(fn(g) { return g(g); });",
    ] {
        let e = err(source);
        assert_eq!(e.category, Category::Depth, "{source}: {e}");
        assert_eq!(e.code, Code::CALL_DEPTH_EXCEEDED, "{source}: {e}");
        assert!(
            e.message
                .contains(&hexput_interpreter::CALL_DEPTH_LIMIT.to_string()),
            "{source}: {e}"
        );
    }
    // The boundary is exact. `down(n)` holds n + 1 calls open at once — the tail call is made
    // while the caller's frame is still live — so the deepest argument that fits the limit is
    // CALL_DEPTH_LIMIT - 1, and one more is the first to fail.
    assert_eq!(
        num(&countdown(hexput_interpreter::CALL_DEPTH_LIMIT - 1)),
        0.0
    );
    assert_error(
        &countdown(hexput_interpreter::CALL_DEPTH_LIMIT),
        Category::Depth,
        Code::CALL_DEPTH_EXCEEDED,
        "(n - 1)",
    );
}

/// `return down(n);` over a countdown that recurses exactly `n + 1` times.
fn countdown(n: usize) -> String {
    format!("fn down(n) {{ if (n == 0) {{ return 0; }}; return down(n - 1); }}; return down({n});")
}

#[test]
fn return_returns_from_the_innermost_function() {
    // From inside a loop inside a function: the function returns, the Script keeps going.
    assert_eq!(
        num(
            "fn first(items) { for (x in items) { return x; }; return -1; }; let got = first([7, 8]); return got + 1;"
        ),
        8.0
    );
    assert_eq!(
        num(
            "fn count() { let i = 0; while (1) { i = i + 1; if (i == 4) { return i; }; }; }; return count();"
        ),
        4.0
    );
    // A `return` in a nested block of a function body still returns from the function.
    assert_eq!(
        num("fn f() { { { return 2; }; }; return 3; }; return f();"),
        2.0
    );
    // Top-level `return` inside control flow ends the Script.
    assert_eq!(num("if (1) { return 5; }; return 6;"), 5.0);
    assert_eq!(num("for (x in [9]) { return x; }; return 0;"), 9.0);
    assert_eq!(num("while (1) { return 4; }; return 0;"), 4.0);
    // Statements after a call's `return` never run.
    assert_eq!(num("fn f() { return 1; nope; }; return f();"), 1.0);
}

#[test]
fn returning_a_function_is_a_type_error_on_the_returned_expression() {
    for (source, spanned) in [
        ("return fn(x) { return x; };", "fn(x) { return x; }"),
        ("fn f() { }; return f;", "f"),
        ("let f = fn() {}; return [f];", "[f]"),
        ("let f = fn() {}; return {a: {b: f}};", "{a: {b: f}}"),
        ("fn make() { return fn() {}; }; return make();", "make()"),
    ] {
        let e = assert_error(source, Category::Type, Code::FUNCTION_RESULT, spanned);
        assert!(e.message.contains("function"), "{source}: {e}");
    }
    // A function the Script holds but does not return is fine.
    assert_eq!(num("let f = fn() { return 1; }; return f();"), 1.0);
    assert_eq!(num("let a = [fn() {}]; return 2;"), 2.0);
}

#[test]
fn control_flow_and_calls_are_stack_safe() {
    let n = 12_000;
    let nested_if = format!(
        "let x = 0; {}x = 1;{} return x;",
        "if (1) { ".repeat(n),
        "};".repeat(n)
    );
    assert_eq!(num(&nested_if), 1.0);
    let nested_else = format!(
        "let x = 0; {}x = 1;{} return x;",
        "if (0) { } else { ".repeat(n),
        "};".repeat(n)
    );
    assert_eq!(num(&nested_else), 1.0);
    // Every level breaks out of its own loop, so the nesting is depth, not an endless outer turn.
    let nested_while = format!(
        "let x = 0; {}x = x + 1;{} return x;",
        "while (1) { ".repeat(n),
        "break; };".repeat(n)
    );
    assert_eq!(num(&nested_while), 1.0);
    // Long-running loops: iteration count is not depth.
    let iterations = 100_000;
    let counting = format!("let i = 0; while (i < {iterations}) {{ i = i + 1; }}; return i;");
    assert_eq!(num(&counting), f64::from(iterations));
    let skipping = format!(
        "let i = 0; let n = 0; while (i < {iterations}) {{ i = i + 1; if (i % 2) {{ continue; }}; n = n + 1; }}; return n;"
    );
    assert_eq!(num(&skipping), f64::from(iterations / 2));
    // A `for` over a long array, built by appending.
    let walking = format!(
        "let a = []; let i = 0; while (i < {iterations}) {{ a[i] = i; i = i + 1; }}; let sum = 0; for (x in a) {{ sum = sum + 1; }}; return sum;"
    );
    assert_eq!(num(&walking), f64::from(iterations));
    // Calls nested as deeply as the limit allows: n + 1 are open at once, so the deepest
    // argument that fits is CALL_DEPTH_LIMIT - 1.
    let deepest = hexput_interpreter::CALL_DEPTH_LIMIT - 1;
    let recursive = format!(
        "fn down(n) {{ if (n == 0) {{ return 0; }}; return 1 + down(n - 1); }}; return down({deepest});"
    );
    assert_eq!(num(&recursive), deepest as f64);
    // Many sequential calls: depth is restored after each one returns.
    let sequential = format!(
        "fn one() {{ return 1; }}; let n = 0; let i = 0; while (i < {iterations}) {{ n = n + one(); i = i + 1; }}; return n;"
    );
    assert_eq!(num(&sequential), f64::from(iterations));
    // Errors from deep inside nested control flow still come back as diagnostics.
    let failing = format!("{}nope;{} ", "if (1) { ".repeat(n), "};".repeat(n));
    assert_eq!(err(&failing).code, Code::UNDECLARED_IDENTIFIER);
}

#[test]
fn control_flow_tokens_do_not_panic() {
    let atoms = [
        "if", "while", "for", "in", "fn", "return", "break", "continue", "(", ")", "{", "}", "x",
        "1", ";", ",",
    ];
    for a in atoms {
        for b in atoms {
            for c in atoms {
                for d in atoms {
                    if let Ok(program) = parse(&format!("{a} {b} {c} {d}")) {
                        let _ = evaluate(&program);
                    }
                }
            }
        }
    }
}

#[test]
fn deep_input_is_stack_safe() {
    let n = 12_000;
    let groups = format!("return {}1{};", "(".repeat(n), ")".repeat(n));
    assert_eq!(num(&groups), 1.0);
    let sum = format!("return 1{};", "+1".repeat(n));
    assert_eq!(num(&sum), (n + 1) as f64);
    let right = format!("return {}1{};", "1+(".repeat(n), ")".repeat(n));
    assert_eq!(num(&right), (n + 1) as f64);
    let nots = format!("return {}0;", "!".repeat(n));
    assert!(!boolean(&nots)); // an even count of `!` on falsy 0
    let blocks = format!("let x = 7; {}return x;{}", "{".repeat(n), "}".repeat(n));
    assert_eq!(num(&blocks), 7.0);
    let shadowed = format!("{}return 1;{}", "{let x = 1;".repeat(n), "}".repeat(n));
    assert_eq!(num(&shadowed), 1.0);
    let failing = format!("{}nope;{}", "{let x = 1;".repeat(n), "}".repeat(n));
    assert_eq!(err(&failing).code, Code::UNDECLARED_IDENTIFIER);
    let nested_object = format!(
        "let o = {}null{}; return o{};",
        "{b:".repeat(n),
        "}".repeat(n),
        ".b".repeat(n)
    );
    assert!(ok(&nested_object).is_null());
    let too_far = format!(
        "let o = {}null{}; return o{};",
        "{b:".repeat(n),
        "}".repeat(n),
        ".b".repeat(n + 1)
    );
    assert_eq!(err(&too_far).code, Code::NULL_ACCESS);
    let nested_array = format!("return {}{};", "[".repeat(n), "]".repeat(n));
    let value = ok(&nested_array);
    assert_eq!(
        value.as_array().map(hexput_interpreter::Array::len),
        Some(1)
    );
    drop(value);
    let indices = format!("let a = [0]; return {}0{};", "a[".repeat(n), "]".repeat(n));
    assert_eq!(num(&indices), 0.0);
    let optional = format!("let a = null; return a?.b{};", ".c".repeat(n));
    assert!(ok(&optional).is_null());
}

#[test]
fn short_token_combinations_do_not_panic() {
    let atoms = [
        "let", "x", "1", "\"s\"", "[", "]", "{", "}", ".", "?.", "+", "/", "=", ";", "null",
        "return",
    ];
    for a in atoms {
        for b in atoms {
            for c in atoms {
                for d in atoms {
                    if let Ok(program) = parse(&format!("{a} {b} {c} {d}")) {
                        let _ = evaluate(&program);
                    }
                }
            }
        }
    }
}

/// The numbers of a detached array, in order.
fn numbers(value: &Value) -> Vec<f64> {
    value
        .as_array()
        .expect("array")
        .to_vec()
        .iter()
        .filter_map(Value::as_number)
        .collect()
}

#[test]
fn cycles_that_are_not_returned_are_fine() {
    assert_eq!(num("let a = []; a[0] = a; return 1;"), 1.0);
    assert_eq!(num("let o = {}; o.me = o; return 2;"), 2.0);
    assert!(boolean(
        "let a = []; let o = {a: a}; a[0] = o; return a[0].a == a;"
    ));
    // A cycle elsewhere in the heap does not taint an acyclic result.
    let result = ok("let a = []; a[0] = a; let b = [1, 2]; return b;");
    assert_eq!(numbers(&result), [1.0, 2.0]);
    // Returning an element that is not itself on a cycle is fine.
    assert_eq!(num("let a = [5]; a[1] = a; return a[0];"), 5.0);
}

#[test]
fn returning_a_cycle_is_a_type_error_on_the_returned_expression() {
    for (source, spanned) in [
        ("let a = []; a[0] = a; return a;", "a"),
        ("let a = []; a[0] = a; return [1, a];", "[1, a]"),
        ("let o = {}; o.me = o; return o;", "o"),
        ("let o = {}; o.me = o; return {x: {y: o}};", "{x: {y: o}}"),
        ("let a = []; let o = {a: a}; a[0] = o; return (a);", "(a)"),
        (
            "let a = []; let b = [a]; let c = [b]; a[0] = c; { return [[c]]; }",
            "[[c]]",
        ),
    ] {
        let e = assert_error(source, Category::Type, Code::CYCLIC_RESULT, spanned);
        assert!(e.message.contains("refers back to itself"), "{source}: {e}");
    }
    // The cycle sits below the returned root: the message must not claim the root itself is
    // self-containing, only that it contains such a value.
    let e = err("let a = []; a[0] = a; return [1, a];");
    assert_eq!(
        e.message,
        "cannot return this array: it contains a value that refers back to itself, and a \
         Script result must be a finite tree of values"
    );
}

#[test]
fn shared_results_detach_as_copies() {
    let result = ok("let x = [1]; return [x, x];");
    let items = result.as_array().expect("array").to_vec();
    assert_eq!(items.len(), 2);
    for item in &items {
        assert_eq!(numbers(item), [1.0]);
    }
    // Detaching reads the state at `return`, through every alias.
    let result = ok("let x = [1]; let y = {p: x, q: [x]}; x[1] = 2; return y;");
    let object = result.as_object().expect("object");
    assert_eq!(numbers(object.get("p").expect("p")), [1.0, 2.0]);
    let q = object.get("q").expect("q").as_array().expect("array");
    assert_eq!(numbers(q.get(0).expect("q[0]")), [1.0, 2.0]);
    // Identity is still observable inside the execution.
    assert!(boolean("let x = [1]; let y = [x, x]; return y[0] == y[1];"));
    // Shared structure is detached once: 2^40 paths, 41 collections.
    let doubling = format!("let a = [];{} return a;", " a = [a, a];".repeat(40));
    let mut value = ok(&doubling);
    let mut depth = 0;
    while let Some(array) = value.as_array() {
        assert_eq!(array.len(), if depth == 40 { 0 } else { 2 });
        let Some(next) = array.get(1).cloned() else {
            break;
        };
        value = next;
        depth += 1;
    }
    assert_eq!(depth, 40);
}

#[test]
fn deep_results_detach_and_drop_without_recursion() {
    let n = 12_000;
    let arrays = format!("return {}1{};", "[".repeat(n), "]".repeat(n));
    let mut value = ok(&arrays);
    let mut depth = 0;
    loop {
        let Some(next) = value.as_array().and_then(|a| a.get(0)).cloned() else {
            break;
        };
        value = next;
        depth += 1;
    }
    assert_eq!(depth, n);
    assert_eq!(value.as_number(), Some(1.0));
    let objects = format!("return {}null{};", "{b:".repeat(n), "}".repeat(n));
    drop(ok(&objects));
    // Built by assignment, so the heap holds it as n separate collections.
    let built = format!(
        "let o = {{}}; let top = o;{} return top;",
        " o.b = {}; o = o.b;".repeat(n)
    );
    let result = ok(&built);
    assert_eq!(
        result.as_object().map(hexput_interpreter::Object::len),
        Some(1)
    );
    drop(result);
    // A cycle at the bottom of a deep structure is still found, without recursion.
    let deep_cycle = format!(
        "let o = {{}}; let top = o;{} o.b = top; return top;",
        " o.b = {}; o = o.b;".repeat(n)
    );
    assert_eq!(err(&deep_cycle).code, Code::CYCLIC_RESULT);
}

#[test]
fn failures_after_allocating_report_the_unchanged_diagnostic() {
    assert_error(
        "let a = [1]; a[1] = a; let o = {a: a}; return nope;",
        Category::Reference,
        Code::UNDECLARED_IDENTIFIER,
        "nope",
    );
    assert_error(
        "let a = [[1], {}]; a[5] = 0;",
        Category::Reference,
        Code::INDEX_OUT_OF_RANGE,
        "5",
    );
}

#[test]
fn many_blocks_evaluate() {
    let n = 12_000;
    let siblings = format!(
        "let x = 0;{} return x;",
        " { let y = x; x = y + 1; };".repeat(n)
    );
    assert_eq!(num(&siblings), n as f64);
    let nested = format!(
        "let x = 0; {}{} return x;",
        "{ x = x + 1; ".repeat(n),
        "};".repeat(n)
    );
    assert_eq!(num(&nested), n as f64);
}

// --- Story 1.8: the producer sweep ---

/// Every runtime code, reached from real source, with the text its span must slice out.
///
/// Story 1.8's acceptance is that a span points at the offending construct, and the only proof
/// of that is slicing the original source with it. A code added to the interpreter belongs here
/// — `tests/shared.rs` pins the full list of code strings.
#[test]
fn every_runtime_code_spans_the_offending_source() {
    let deep = countdown(hexput_interpreter::CALL_DEPTH_LIMIT);
    let cases: [(&str, Category, Code, &str); 18] = [
        (
            r#"return "abc" * 2;"#,
            Category::Type,
            Code::OPERAND_MISMATCH,
            r#""abc""#,
        ),
        (
            "let o = {}; o[0] = 1;",
            Category::Type,
            Code::INVALID_INDEX,
            "0",
        ),
        (
            "let n = 1; n.x = 1;",
            Category::Type,
            Code::INVALID_PROPERTY_ACCESS,
            ".x",
        ),
        (
            "let a = []; a[0] = a; return a;",
            Category::Type,
            Code::CYCLIC_RESULT,
            "a",
        ),
        ("let x = 1; x();", Category::Type, Code::NOT_CALLABLE, "()"),
        (
            "return fn(x) { return x; };",
            Category::Type,
            Code::FUNCTION_RESULT,
            "fn(x) { return x; }",
        ),
        (
            "return nope;",
            Category::Reference,
            Code::UNDECLARED_IDENTIFIER,
            "nope",
        ),
        (
            "nope = 1;",
            Category::Reference,
            Code::UNDECLARED_ASSIGNMENT,
            "nope",
        ),
        (
            "let a = null; return a.b;",
            Category::Reference,
            Code::NULL_ACCESS,
            ".b",
        ),
        (
            "let a = []; a[1] = 0;",
            Category::Reference,
            Code::INDEX_OUT_OF_RANGE,
            "1",
        ),
        (
            "let a = [1]; for (x in a) { a[1] = 2; };",
            Category::Reference,
            Code::COLLECTION_MUTATED,
            "for",
        ),
        (
            "return 1 / 0;",
            Category::Arithmetic,
            Code::DIVISION_BY_ZERO,
            "0",
        ),
        (
            "return 1e308 * 10;",
            Category::Arithmetic,
            Code::NON_FINITE,
            "*",
        ),
        (
            "fn none() { }; return none(1);",
            Category::Arity,
            Code::ARGUMENT_COUNT,
            "(1)",
        ),
        (
            deep.as_str(),
            Category::Depth,
            Code::CALL_DEPTH_EXCEEDED,
            "(n - 1)",
        ),
        // Story 3.1: a host call's arguments, and — with no host — the call itself.
        (
            "return send(1, [fn() {}]);",
            Category::Type,
            Code::FUNCTION_ARGUMENT,
            "[fn() {}]",
        ),
        (
            "let a = [1]; a[1] = a; return send(a);",
            Category::Type,
            Code::CYCLIC_ARGUMENT,
            "a",
        ),
        (
            "let x = 1;\nreturn getOrder(x).total;",
            Category::Capability,
            Code::UNKNOWN_FUNCTION,
            "getOrder(x)",
        ),
    ];
    for (source, category, code, offending) in cases {
        let d = assert_error(source, category, code, offending);
        assert_eq!(
            hexput_tests::line_and_column(source, d.span.offset),
            (d.span.line, d.span.column),
            "{source}: {d}"
        );
    }
    for (i, (_, _, code, _)) in cases.iter().enumerate() {
        for (_, _, other, _) in &cases[i + 1..] {
            assert_ne!(code, other, "the sweep must reach each code once");
        }
    }
}

/// A span that crosses lines still slices the offending construct, and its recorded line and
/// column are those of where the construct *opened* — a string literal legally spans lines.
#[test]
fn a_multi_line_construct_keeps_its_opening_location() {
    let source = "let x = 1;\nreturn \"a\nb\" * 2;";
    let d = assert_error(source, Category::Type, Code::OPERAND_MISMATCH, "\"a\nb\"");
    assert_eq!((d.span.line, d.span.column), (2, 8));
}

// --- starting variables (Story 1.9) ---

/// Evaluate `source` with caller-supplied starting variables bound in its root scope.
fn eval_with(source: &str, variables: Vec<(&str, Value)>) -> Result<Value, Diagnostic> {
    let program = parse(source).unwrap_or_else(|e| panic!("`{source}` should parse: {e}"));
    evaluate_with_variables(&program, variables)
}

fn ok_with(source: &str, variables: Vec<(&str, Value)>) -> Value {
    eval_with(source, variables).unwrap_or_else(|e| panic!("`{source}` should evaluate: {e}"))
}

#[test]
fn no_starting_variables_is_exactly_evaluate() {
    assert_eq!(
        ok_with("return 2 + 3;", Vec::new()).as_number(),
        ok("return 2 + 3;").as_number()
    );
    // An empty set does not declare anything either: an unbound name is still undeclared.
    let e = eval_with("return n;", Vec::new()).expect_err("`n` is not declared");
    assert_eq!(e.code, Code::UNDECLARED_IDENTIFIER);
}

#[test]
fn each_of_the_six_types_binds_and_reads_back() {
    assert!(ok_with("return v;", vec![("v", Value::Null)]).is_null());
    assert_eq!(
        ok_with("return v;", vec![("v", Value::Bool(true))]).as_bool(),
        Some(true)
    );
    assert_eq!(
        ok_with("return v;", vec![("v", Value::Number(2.5))]).as_number(),
        Some(2.5)
    );
    assert_eq!(
        ok_with("return v;", vec![("v", Value::String("hi".into()))]).as_str(),
        Some("hi")
    );

    let array = Value::Array(Array::from_values(vec![
        Value::Number(1.0),
        Value::String("x".into()),
    ]));
    let out = ok_with("return v;", vec![("v", array)]);
    let out = out.as_array().expect("an array comes back an array");
    assert_eq!(out.len(), 2);
    assert_eq!(out.get(0).and_then(Value::as_number), Some(1.0));
    assert_eq!(out.get(1).and_then(Value::as_str), Some("x"));

    let object = Value::Object(Object::from_entries([
        ("a", Value::Number(1.0)),
        ("b", Value::Bool(false)),
    ]));
    let out = ok_with("return v;", vec![("v", object)]);
    let out = out.as_object().expect("an object comes back an object");
    assert_eq!(out.len(), 2);
    assert_eq!(out.get("a").and_then(Value::as_number), Some(1.0));
    // Insertion order is part of the type (§3), so it survives the round trip.
    assert_eq!(
        out.entries()
            .iter()
            .map(|(k, _)| k.to_string())
            .collect::<Vec<_>>(),
        ["a", "b"]
    );
}

#[test]
fn a_bound_collection_is_a_live_collection_inside_the_execution() {
    let array = Value::Array(Array::from_values(vec![Value::Number(1.0)]));
    // Appending at the length is the one legal out-of-range write (§7), so the binding really is
    // the execution's own array, not a frozen copy.
    let out = ok_with("v[1] = 2; return v[0] + v[1];", vec![("v", array)]);
    assert_eq!(out.as_number(), Some(3.0));

    let object = Value::Object(Object::from_entries([("a", Value::Number(1.0))]));
    let out = ok_with("v.b = 2; return v.a + v.b;", vec![("v", object)]);
    assert_eq!(out.as_number(), Some(3.0));
}

#[test]
fn a_nested_starting_variable_reads_through() {
    let inner = Value::Array(Array::from_values(vec![Value::Number(7.0)]));
    let outer = Value::Object(Object::from_entries([("xs", inner)]));
    assert_eq!(
        ok_with("return v.xs[0];", vec![("v", outer)]).as_number(),
        Some(7.0)
    );
}

#[test]
fn a_starting_variable_behaves_like_a_top_level_binding() {
    // Assignable — it is a binding, not a constant.
    assert_eq!(
        ok_with("n = n + 1; return n;", vec![("n", Value::Number(1.0))]).as_number(),
        Some(2.0)
    );
    // Shadowable by an inner block, with the outer binding intact afterwards (§5).
    assert_eq!(
        ok_with("{ let n = 9; }; return n;", vec![("n", Value::Number(1.0))]).as_number(),
        Some(1.0)
    );
    // Captured by reference by a closure, like any other binding (§6).
    assert_eq!(
        ok_with(
            "let f = fn() { return n; }; n = 5; return f();",
            vec![("n", Value::Number(1.0))]
        )
        .as_number(),
        Some(5.0)
    );
}

#[test]
fn a_name_the_scripts_own_top_level_declares_is_reported_never_discarded() {
    // §5: `let`, named functions and parameters share one block namespace, and a starting
    // variable is a top-level binding — so either of these really is a redeclaration in one
    // block. Binding one and silently dropping the other is what this rejects.
    for (source, declaration) in [
        ("let n = 3; return n;", "n = 3"),
        ("fn n() { return 3; }; return n();", "n()"),
    ] {
        let e = eval_with(source, vec![("n", Value::Number(9.0))])
            .expect_err("a starting variable the script redeclares");
        assert_eq!(e.category, Category::Syntax, "{source}: {e}");
        assert_eq!(e.code, Code::DUPLICATE_DECLARATION, "{source}: {e}");
        // Spanned on the colliding declaration's name.
        assert_eq!(&source[e.span.range()], "n", "{source}: {e}");
        assert!(source.contains(declaration));
    }
    // A name the script declares in an *inner* block is ordinary shadowing, not a collision.
    assert_eq!(
        ok_with("{ let n = 3; }; return n;", vec![("n", Value::Number(9.0))]).as_number(),
        Some(9.0)
    );
}

#[test]
fn a_repeated_name_keeps_the_last_value() {
    // Rejecting a repeat belongs to the caller, which knows where the two came from; the
    // interpreter binds in order.
    assert_eq!(
        ok_with(
            "return n;",
            vec![("n", Value::Number(1.0)), ("n", Value::Number(2.0))]
        )
        .as_number(),
        Some(2.0)
    );
}

#[test]
fn a_starting_variable_may_be_returned_unchanged() {
    let object = Value::Object(Object::from_entries([(
        "xs",
        Value::Array(Array::from_values(vec![Value::Number(1.0)])),
    )]));
    let out = ok_with("return v;", vec![("v", object)]);
    assert_eq!(
        out.as_object()
            .and_then(|o| o.get("xs"))
            .and_then(Value::as_array)
            .map(Array::len),
        Some(1)
    );
}

#[test]
fn an_adversarially_nested_object_starting_variable_does_not_overflow_the_host_stack() {
    // The object path is a different walk from the array path in both flat traversals: keys are
    // carried separately in `Heap::attach`, and separately again on the way back out.
    let mut value = Value::Object(Object::from_entries(Vec::<(&str, Value)>::new()));
    for _ in 0..20_000 {
        value = Value::Object(Object::from_entries([("inner", value)]));
    }
    let out = ok_with("return v;", vec![("v", value)]);
    let mut depth = 0;
    let mut level = out;
    while let Some(inner) = level.as_object().and_then(|o| o.get("inner")).cloned() {
        depth += 1;
        level = inner;
    }
    assert_eq!(depth, 20_000);
}

#[test]
fn an_adversarially_nested_starting_variable_does_not_overflow_the_host_stack() {
    let mut value = Value::Array(Array::from_values(Vec::new()));
    for _ in 0..20_000 {
        value = Value::Array(Array::from_values(vec![value]));
    }
    // Attaching it into the heap, reading through it, and detaching it again are all flat walks.
    let out = ok_with("return v;", vec![("v", value)]);
    let mut depth = 0;
    let mut level = out;
    while let Some(array) = level.as_array().map(|a| a.to_vec()) {
        match array.into_iter().next() {
            Some(inner) => {
                depth += 1;
                level = inner;
            }
            None => break,
        }
    }
    assert_eq!(depth, 20_000);
}

// --- Story 3.1: host calls suspend a resumable execution ---

mod host_calls {
    use std::sync::Arc;

    use hexput_interpreter::{Execution, Outcome, Value};

    fn start(source: &str, variables: Vec<(&str, Value)>) -> Execution {
        let program = Arc::new(hexput_parser::parse(source).unwrap());
        Execution::with_variables(program, variables).unwrap()
    }

    /// Run `execution`, answering each host call with `answer(name, arguments)`; the calls made,
    /// in order, and the result.
    fn drive(
        execution: Execution,
        answer: impl Fn(&str, &[Value]) -> Value,
    ) -> (Vec<(String, Vec<Value>)>, Value) {
        let mut calls = Vec::new();
        let mut execution = execution;
        loop {
            match execution.run().unwrap() {
                Outcome::Finished(result) => return (calls, result.value),
                Outcome::HostCall(call) => {
                    let arguments: Vec<Value> =
                        call.arguments().iter().map(|a| a.value.clone()).collect();
                    let value = answer(call.name(), &arguments);
                    calls.push((call.name().to_owned(), arguments));
                    execution = call.resume(&value);
                }
                Outcome::Paused(_) | Outcome::OutOfMemory(_) | Outcome::AllocationsExceeded(_) => {
                    panic!("an unmetered execution never stops at a meter")
                }
            }
        }
    }

    #[test]
    fn an_execution_is_send_and_static() {
        fn assert_send_static<T: Send + 'static>() {}
        assert_send_static::<Execution>();
        assert_send_static::<hexput_interpreter::HostCall>();
    }

    #[test]
    fn a_host_call_suspends_and_resumes_with_the_value_given() {
        let source = "let x = 7;\nreturn getOrder(x, \"a\").total + 1;";
        let execution = start(source, vec![]);
        let Outcome::HostCall(call) = execution.run().unwrap() else {
            panic!("the Script calls the host");
        };
        assert_eq!(call.name(), "getOrder");
        let arguments = call.arguments();
        assert_eq!(arguments.len(), 2);
        assert_eq!(arguments[0].value.as_number(), Some(7.0));
        assert_eq!(&source[arguments[0].span.range()], "x");
        assert_eq!(arguments[1].value.as_str(), Some("a"));
        assert_eq!(&source[call.span().range()], "getOrder(x, \"a\")");
        let order = Value::Object(hexput_interpreter::Object::from_entries([(
            "total",
            Value::Number(3.0),
        )]));
        let Outcome::Finished(result) = call.resume(&order).run().unwrap() else {
            panic!("one call only");
        };
        assert_eq!(result.value.as_number(), Some(4.0));
    }

    #[test]
    fn calls_inside_functions_loops_and_assignment_targets_resume_in_place() {
        let source = "
            fn twice(n) { return double(n) + double(n); };
            let total = 0;
            for (i in [1, 2]) { total = total + twice(i); };
            let seen = getBox();
            getBox().n = 5;
            return [total, seen.n];
        ";
        let boxed = Value::Object(hexput_interpreter::Object::from_entries([(
            "n",
            Value::Number(1.0),
        )]));
        let (calls, result) = drive(start(source, vec![]), |name, arguments| match name {
            "double" => Value::Number(arguments[0].as_number().unwrap() * 2.0),
            _ => boxed.clone(),
        });
        let names: Vec<&str> = calls.iter().map(|(name, _)| name.as_str()).collect();
        assert_eq!(
            names,
            ["double", "double", "double", "double", "getBox", "getBox"]
        );
        let items: Vec<f64> = result
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_number().unwrap())
            .collect();
        // Each `getBox()` value is its own copy, so writing the second leaves the first alone.
        assert_eq!(items, [12.0, 1.0]);
    }

    #[test]
    fn a_local_binding_shadows_the_host_and_a_host_name_is_not_a_value() {
        let (calls, result) = drive(
            start("fn f(x) { return x; }; return f(1);", vec![]),
            |_, _| Value::Null,
        );
        assert!(calls.is_empty());
        assert_eq!(result.as_number(), Some(1.0));
        let (calls, result) = drive(
            start("return f;", vec![("f", Value::Number(2.0))]),
            |_, _| Value::Null,
        );
        assert!(calls.is_empty());
        assert_eq!(result.as_number(), Some(2.0));
        let error = start("let g = f; return g;", vec![])
            .run()
            .err()
            .expect("a host name is not a value");
        assert_eq!(error.code.as_str(), "reference.undeclared_identifier");
    }

    #[test]
    fn with_variables_rejects_a_starting_variable_the_script_declares() {
        let program = Arc::new(hexput_parser::parse("let a = 1; return a;").unwrap());
        let error = Execution::with_variables(program, [("a", Value::Null)])
            .err()
            .expect("a duplicate declaration");
        assert_eq!(error.code.as_str(), "syntax.duplicate_declaration");
    }
}

// --- Story 3.4: everything a Script can reach, enumerated ---

#[test]
fn the_language_has_no_builtins() {
    // No standard library at all in v2 (LANGUAGE-REFERENCE §11): a name added here widens the
    // trust boundary and needs its own spec.
    assert!(BUILTINS.is_empty(), "builtins: {BUILTINS:?}");
}

#[test]
fn a_fresh_execution_binds_its_starting_variables_its_functions_and_nothing_else() {
    let source = "fn helper() { return 1; }; let later = 2; return helper() + later + input;";
    let program = std::sync::Arc::new(parse(source).unwrap());
    let execution =
        Execution::with_variables(program, vec![("input", Value::Number(1.0))]).unwrap();
    let mut expected: Vec<String> = BUILTINS.iter().map(|b| (*b).to_owned()).collect();
    // Starting variables and hoisted top-level functions; `let later` binds only when it runs.
    expected.extend(["helper".to_owned(), "input".to_owned()]);
    expected.sort();
    assert_eq!(execution.root_names(), expected);
}

#[test]
fn with_nothing_given_and_nothing_declared_the_root_scope_is_empty() {
    let program = std::sync::Arc::new(parse("return 1;").unwrap());
    let execution = Execution::with_variables(program, Vec::<(&str, Value)>::new()).unwrap();
    assert!(
        execution.root_names().is_empty(),
        "{:?}",
        execution.root_names()
    );
}

#[test]
fn module_and_import_syntax_does_not_exist() {
    for source in [
        "import fs;",
        "import { readFile } from \"fs\";",
        "let fs = require \"fs\";",
        "export let x = 1;",
    ] {
        let error = parse(source).expect_err(source);
        assert_eq!(error.category.as_str(), "syntax", "`{source}`: {error:?}");
    }
}

#[test]
fn root_names_stay_the_root_scope_after_a_host_call_inside_a_function() {
    use hexput_interpreter::Outcome;
    let source = "fn inner(y) { let local = y; return host(local); }; return inner(1);";
    let program = std::sync::Arc::new(parse(source).unwrap());
    let execution =
        Execution::with_variables(program, vec![("input", Value::Number(1.0))]).unwrap();
    let Ok(Outcome::HostCall(call)) = execution.run() else {
        panic!("the Script stops at `host(local)`");
    };
    let resumed = call.resume(&Value::Null);
    // Not `y`/`local` of the function it stopped in: the root scope.
    assert_eq!(resumed.root_names(), ["inner", "input"]);
}

// --- Story 3.5: the interpreter meters, and only meters ---

mod metering {
    use std::num::NonZeroU64;
    use std::sync::Arc;

    use hexput_interpreter::{Execution, Meter, Outcome, Value, evaluate};

    fn start(source: &str, meter: Meter) -> Execution {
        let program = Arc::new(hexput_parser::parse(source).unwrap());
        Execution::with_variables(program, Vec::<(&str, Value)>::new())
            .unwrap()
            .metered(meter)
    }

    fn slices(steps: u64) -> Meter {
        Meter {
            slice: NonZeroU64::new(steps),
            ..Meter::UNMETERED
        }
    }

    fn ceiling(bytes: usize) -> Meter {
        Meter {
            memory_ceiling: Some(bytes),
            ..Meter::UNMETERED
        }
    }

    /// Run to the end, resuming every pause; the number of pauses and the result.
    fn run_through_pauses(execution: Execution) -> (usize, Value) {
        let mut pauses = 0;
        let mut execution = execution;
        loop {
            match execution.run().unwrap() {
                Outcome::Paused(paused) => {
                    pauses += 1;
                    execution = paused.resume();
                }
                Outcome::Finished(result) => return (pauses, result.value),
                _ => panic!("no host call and no ceiling here"),
            }
        }
    }

    const COUNT_TO_A_THOUSAND: &str = "let n = 0; let total = 0; while (n < 1000) { total = total + n; n = n + 1; }; return total;";

    #[test]
    fn a_sliced_execution_pauses_and_carries_on_to_the_same_result() {
        let (pauses, result) = run_through_pauses(start(COUNT_TO_A_THOUSAND, slices(100)));
        assert!(pauses > 10, "{pauses} pauses");
        assert_eq!(result.as_number(), Some(499_500.0));
        let (pauses, result) = run_through_pauses(start(COUNT_TO_A_THOUSAND, Meter::UNMETERED));
        assert_eq!(pauses, 0);
        assert_eq!(result.as_number(), Some(499_500.0));
    }

    #[test]
    fn a_runaway_loop_pauses_forever_inside_the_loop() {
        let source = "let x = 0;\nwhile (true) { x = x + 1; }";
        let mut execution = start(source, slices(1_000));
        for _ in 0..50 {
            let Outcome::Paused(paused) = execution.run().unwrap() else {
                panic!("a runaway never finishes");
            };
            let span = paused.span();
            assert!(span.offset >= source.find("while").unwrap(), "{span:?}");
            execution = paused.resume();
        }
    }

    #[test]
    fn long_string_work_ends_a_slice_sooner() {
        // Comparing two 64 KiB strings reads 128 KiB: 512 units, not one step.
        let source = "let s = \"xxxxxxxxxxxxxxxx\"; let i = 0; while (i < 12) { s = s + s; i = i + 1; };\n\
                      let n = 0; while (n < 100) { let same = s == s; n = n + 1; }; return n;";
        let cheap = run_through_pauses(start(COUNT_TO_A_THOUSAND, slices(2_000))).0;
        let costly = run_through_pauses(start(source, slices(2_000))).0;
        assert!(
            costly > 20,
            "{costly} pauses (a thousand cheap steps take {cheap})"
        );
    }

    #[test]
    fn a_growing_string_stops_at_the_ceiling_before_it_allocates_past_it() {
        let source = "let s = \"x\";\nwhile (true) { s = s + s; }";
        let limit = 1024 * 1024;
        let Outcome::OutOfMemory(stopped) = start(source, ceiling(limit)).run().unwrap() else {
            panic!("the string outgrows the ceiling");
        };
        assert_eq!(&source[stopped.span().range()], "+");
        assert!(
            stopped.used() <= limit,
            "stopped before allocating: {}",
            stopped.used()
        );
        assert!(stopped.used() > limit / 4, "{}", stopped.used());
    }

    #[test]
    fn a_growing_collection_stops_at_the_ceiling() {
        let source = "let a = [];\nlet n = 0;\nwhile (true) { a[n] = [n]; n = n + 1; }";
        let limit = 256 * 1024;
        let Outcome::OutOfMemory(stopped) = start(source, ceiling(limit)).run().unwrap() else {
            panic!("the collection outgrows the ceiling");
        };
        assert!(stopped.used() > limit, "{}", stopped.used());
        // One step's own allocation past it at most: an element and a one-element array.
        assert!(stopped.used() < limit + 1024, "{}", stopped.used());
        let line = source.lines().nth(2).unwrap();
        assert_eq!(stopped.span().line, 3, "{:?} in `{line}`", stopped.span());
    }

    #[test]
    fn keys_a_for_loop_hands_out_count_in_full_while_they_live() {
        // Each key handed out is a string value that can outlive its object: collecting a 64 KiB
        // key again and again must reach a 1 MiB ceiling, however the key's bytes are shared.
        let source = "let s = \"xxxxxxxxxxxxxxxx\"; let i = 0; while (i < 12) { s = s + s; i = i + 1; };\n\
                      let o = {}; o[s] = 1; let a = []; let n = 0;\n\
                      while (true) { for (k in o) { a[n] = k; n = n + 1; }; }";
        let Outcome::OutOfMemory(stopped) = start(source, ceiling(1024 * 1024)).run().unwrap()
        else {
            panic!("the collected keys outgrow the ceiling");
        };
        assert_eq!(stopped.span().line, 3, "{:?}", stopped.span());
    }

    #[test]
    fn a_concatenation_that_cannot_convert_is_a_type_error_even_near_the_ceiling() {
        // 64 KiB built under a 160 KiB ceiling, where `s + s` would cross it but `s + []` is a
        // `type` error, not the ceiling.
        let build =
            "let s = \"xxxxxxxxxxxxxxxx\"; let i = 0; while (i < 12) { s = s + s; i = i + 1; };\n";
        let crossing = start(&format!("{build}return s + s;"), ceiling(160 * 1024));
        assert!(matches!(crossing.run(), Ok(Outcome::OutOfMemory(_))));
        let error = start(&format!("{build}return s + [];"), ceiling(160 * 1024))
            .run()
            .err()
            .expect("a type error");
        assert_eq!(error.code.as_str(), "type.operand_mismatch");
    }

    #[test]
    fn a_string_shared_many_times_counts_once() {
        // 64 KiB, held by a thousand elements: well under a 1 MiB ceiling unless counted per use.
        let source = "let s = \"xxxxxxxxxxxxxxxx\"; let i = 0; while (i < 12) { s = s + s; i = i + 1; };\n\
                      let a = []; let n = 0; while (n < 1000) { a[n] = s; n = n + 1; }; return n;";
        let execution = start(source, ceiling(1024 * 1024));
        let Outcome::Finished(result) = execution.run().unwrap() else {
            panic!("sharing a string allocates nothing");
        };
        assert_eq!(result.value.as_number(), Some(1000.0));
    }

    #[test]
    fn memory_that_is_let_go_stops_counting() {
        // Each iteration makes a 64 KiB string and drops the last: it never holds much at once.
        let source = "let s = \"xxxxxxxxxxxxxxxx\"; let i = 0; while (i < 12) { s = s + s; i = i + 1; };\n\
                      let n = 0; let t = \"\"; while (n < 500) { t = s + n; n = n + 1; }; return n;";
        let execution = start(source, ceiling(512 * 1024));
        let Outcome::Finished(result) = execution.run().unwrap() else {
            panic!("only one copy lives at a time");
        };
        assert_eq!(result.value.as_number(), Some(500.0));
    }

    #[test]
    fn memory_used_counts_starting_variables() {
        let program = Arc::new(hexput_parser::parse("return 1;").unwrap());
        let empty = Execution::with_variables(program.clone(), Vec::<(&str, Value)>::new())
            .unwrap()
            .memory_used();
        let big = Value::String(Arc::from("y".repeat(100_000)));
        let loaded = Execution::with_variables(program, vec![("big", big)])
            .unwrap()
            .memory_used();
        assert!(loaded >= empty + 100_000, "{empty} -> {loaded}");
    }

    #[test]
    fn a_starting_variable_over_the_ceiling_stops_before_anything_runs() {
        let program = Arc::new(hexput_parser::parse("return 1;").unwrap());
        let big = Value::String(Arc::from("y".repeat(100_000)));
        let execution = Execution::with_variables(program, vec![("big", big)])
            .unwrap()
            .metered(ceiling(10_000));
        assert!(matches!(execution.run(), Ok(Outcome::OutOfMemory(_))));
    }

    #[test]
    fn evaluate_is_unmetered() {
        let program = hexput_parser::parse(
            "let s = \"x\"; let i = 0; while (i < 22) { s = s + s; i = i + 1; }; return i;",
        )
        .unwrap();
        // Four million bytes and a few hundred thousand steps: no slice, no ceiling.
        assert_eq!(evaluate(&program).unwrap().as_number(), Some(22.0));
    }
}

// --- Story 3.6: the interpreter counts allocations, and stops at a ceiling it is handed ---

mod allocations {
    use std::sync::Arc;

    use hexput_interpreter::{Execution, Meter, Outcome, Value};

    fn start(source: &str, meter: Meter) -> Execution {
        let program = Arc::new(hexput_parser::parse(source).unwrap());
        Execution::with_variables(program, Vec::<(&str, Value)>::new())
            .unwrap()
            .metered(meter)
    }

    fn ceiling(count: u64) -> Meter {
        Meter {
            allocation_ceiling: Some(count),
            ..Meter::UNMETERED
        }
    }

    /// How many allocations `source` makes before it calls `done()`, its last statement.
    fn counted(source: &str) -> u64 {
        match start(&format!("{source}\ndone();"), Meter::UNMETERED)
            .run()
            .unwrap()
        {
            Outcome::HostCall(call) => {
                assert_eq!(call.name(), "done");
                call.resume(&Value::Null).allocations()
            }
            _ => panic!("the Script ends by calling `done`"),
        }
    }

    /// Story 3.6's fixed Script: strings, a concatenation converting a number, an array grown to
    /// length 9, an object literal grown by one key, the keys a `for … in` hands out, and an
    /// object literal result.
    pub const FIXED: &str = "let greeting = \"hello\";\n\
                             let name = greeting + \", \" + 42;\n\
                             let a = [];\n\
                             let i = 0;\n\
                             while (i < 9) { a[i] = i; i = i + 1; };\n\
                             let o = { x: 1, y: \"z\" };\n\
                             o.w = 2;\n\
                             for (k in o) { let seen = k; };\n\
                             let r = { name: name, n: i };";

    #[test]
    fn the_fixed_script_makes_exactly_seventeen_allocations() {
        // "hello" 1; ", " 1, `greeting + ", "` 1, `+ 42` 1 and its conversion 1; `[]` 1; growth
        // to 2, 3, 5 and 9 — 4; the object literal 1 and "z" 1; `o.w` takes 2 keys to 3 — 1; three
        // keys handed out — 3; the result object 1.
        assert_eq!(counted(FIXED), 17);
    }

    #[test]
    fn each_construction_counts_once() {
        assert_eq!(counted("let s = \"a\";"), 1);
        assert_eq!(
            counted("let s = \"a\" + \"b\";"),
            3,
            "two literals, one concatenation"
        );
        assert_eq!(counted("let s = \"a\" + 1;"), 3, "and one conversion");
        assert_eq!(counted("let s = null + \"a\";"), 3);
        assert_eq!(
            counted("let a = [1, 2, 3];"),
            1,
            "a literal is one allocation"
        );
        assert_eq!(counted("let a = [[], []];"), 3);
        assert_eq!(counted("let o = { a: 1, b: 2 };"), 1);
        assert_eq!(counted("let o = { a: \"x\" };"), 2);
        assert_eq!(
            counted("let o = { a: 1, b: 2 }; for (k in o) { let seen = k; };"),
            3
        );
        assert_eq!(
            counted("let a = [1, 2]; for (v in a) { let seen = v; };"),
            1
        );
    }

    #[test]
    fn scalars_rebindings_and_copies_do_not_count() {
        assert_eq!(counted("let n = 1 + 2; let b = !n; let m = n; n = 7;"), 0);
        assert_eq!(
            counted("let s = \"a\"; let t = s; t = s; let u = [s, s];"),
            2
        );
        assert_eq!(
            counted("fn f(x) { return x; }; let y = f(1); let z = f(2);"),
            0
        );
    }

    #[test]
    fn a_growth_is_an_append_past_a_power_of_two() {
        // An array appended from empty to length n: each append from length 1, 2, 4, 8, … grows.
        for (length, growths) in [
            (1, 0),
            (2, 1),
            (3, 2),
            (4, 2),
            (5, 3),
            (8, 3),
            (9, 4),
            (17, 5),
        ] {
            let source =
                format!("let a = []; let i = 0; while (i < {length}) {{ a[i] = i; i = i + 1; }};");
            assert_eq!(counted(&source), 1 + growths, "array to length {length}");
        }
        // Replacing an element is no growth.
        assert_eq!(counted("let a = [1, 2]; a[0] = 5; a[1] = 6;"), 1);
        // An object grows by new keys alone, the same way.
        assert_eq!(
            counted("let o = {}; o.a = 1; o.b = 2; o.c = 3; o.a = 4;"),
            3
        );
        assert_eq!(
            counted("let o = {}; o[\"a\"] = 1;"),
            2,
            "the key's literal counts"
        );
    }

    #[test]
    fn starting_variables_and_host_call_values_do_not_count() {
        let program = Arc::new(hexput_parser::parse("let x = get(); done();").unwrap());
        let big = Value::Array(hexput_interpreter::Array::from_values(vec![
            Value::String(Arc::from("s")),
            Value::Null,
        ]));
        let execution = Execution::with_variables(program, vec![("input", big.clone())]).unwrap();
        assert_eq!(execution.allocations(), 0);
        let Outcome::HostCall(call) = execution.run().unwrap() else {
            panic!("calls `get`");
        };
        let resumed = call.resume(&big);
        assert_eq!(resumed.allocations(), 0);
        let Outcome::HostCall(done) = resumed.run().unwrap() else {
            panic!("calls `done`");
        };
        assert_eq!(done.resume(&Value::Null).allocations(), 0);
    }

    #[test]
    fn the_ceiling_is_the_last_allocation_allowed() {
        let source = format!("{FIXED}\nreturn 1;");
        assert!(matches!(
            start(&source, ceiling(17)).run(),
            Ok(Outcome::Finished(_))
        ));
        let Outcome::AllocationsExceeded(stopped) = start(&source, ceiling(16)).run().unwrap()
        else {
            panic!("the seventeenth allocation crosses a ceiling of sixteen");
        };
        assert_eq!(stopped.count(), 17);
        assert_eq!(&source[stopped.span().range()], "{ name: name, n: i }");
    }

    #[test]
    fn churning_short_lived_strings_stops_at_the_ceiling_with_little_memory() {
        let source = "let n = 0;\nwhile (true) { let t = \"x\" + n; n = n + 1; }";
        let Outcome::AllocationsExceeded(stopped) = start(source, ceiling(10_000)).run().unwrap()
        else {
            panic!("the churn outgrows the ceiling");
        };
        assert!(
            stopped.count() > 10_000 && stopped.count() <= 10_003,
            "{}",
            stopped.count()
        );
        assert_eq!(stopped.span().line, 2, "{:?}", stopped.span());
    }

    #[test]
    fn the_allocation_ceiling_stands_alone() {
        // Far past any memory ceiling's worth of allocations, but no memory held: only the
        // allocation ceiling stops it, and a memory ceiling alone never does.
        let source = "let n = 0;\nwhile (n < 20000) { let t = \"x\" + n; n = n + 1; };\nreturn n;";
        let memory_only = Meter {
            memory_ceiling: Some(64 * 1024),
            ..Meter::UNMETERED
        };
        assert!(matches!(
            start(source, memory_only).run(),
            Ok(Outcome::Finished(_))
        ));
        assert!(matches!(
            start(source, ceiling(1_000)).run(),
            Ok(Outcome::AllocationsExceeded(_))
        ));
    }
}

// --- Story 3.9: switching language constructs off by policy ---

mod features {
    use std::sync::Arc;

    use hexput_ast::{Category, Code, Diagnostic};
    use hexput_interpreter::{Execution, Feature, Features, Outcome, Value};
    use hexput_parser::parse;

    /// Run `source` under `features`, answering every host call with `null`. The result, and the
    /// names of the host calls the Script reached.
    fn run(source: &str, features: Features) -> (Result<Value, Diagnostic>, Vec<String>) {
        let program = Arc::new(parse(source).unwrap_or_else(|e| panic!("`{source}`: {e}")));
        let mut execution = Execution::with_variables(program, Vec::<(&str, Value)>::new())
            .unwrap()
            .with_features(features);
        let mut calls = Vec::new();
        loop {
            match execution.run() {
                Err(error) => return (Err(error), calls),
                Ok(Outcome::Finished(value)) => return (Ok(value.value), calls),
                Ok(Outcome::HostCall(call)) => {
                    calls.push(call.name().to_owned());
                    execution = call.resume(&Value::Null);
                }
                Ok(Outcome::Paused(paused)) => execution = paused.resume(),
                Ok(_) => panic!("`{source}` stopped at a ceiling it does not have"),
            }
        }
    }

    fn without(feature: Feature) -> Features {
        Features::ALL_ENABLED.with(feature, false)
    }

    /// Assert `source` fails under `feature` disabled with the policy error naming it, spanned
    /// on `spanned`, and with `message`; and that it runs with every toggle enabled.
    fn refused(source: &str, feature: Feature, spanned: &str, message: &str) {
        let (result, calls) = run(source, without(feature));
        let error = result.expect_err(source);
        assert_eq!(error.category, Category::Policy, "{source}: {error}");
        assert_eq!(error.code, Code::CONSTRUCT_DISABLED, "{source}: {error}");
        assert_eq!(error.code.as_str(), "policy.construct_disabled");
        assert_eq!(&source[error.span.range()], spanned, "{source}: {error}");
        assert_eq!(error.message, message, "{source}");
        assert!(
            error.message.contains(&format!("`features.{feature}`")),
            "{source}: {error}"
        );
        assert!(calls.is_empty(), "{source}: {calls:?}");
        let (result, _) = run(source, Features::ALL_ENABLED);
        result.unwrap_or_else(|e| panic!("`{source}` should run with every toggle on: {e}"));
    }

    #[test]
    fn each_toggle_refuses_its_construct_on_the_construct() {
        refused(
            "let n = 0; while (n < 3) { n = n + 1; }; return n;",
            Feature::Loops,
            "while",
            "`while` is disabled by policy (`features.loops`)",
        );
        refused(
            "let s = 0; for (x in [1, 2]) { s = s + x; }; return s;",
            Feature::Loops,
            "for",
            "`for … in` is disabled by policy (`features.loops`)",
        );
        refused(
            "if (true) { return 1; } else { return 2; };",
            Feature::Conditionals,
            "if",
            "`if` is disabled by policy (`features.conditionals`)",
        );
        refused(
            "let f = fn(x) { return x; }; return f(1);",
            Feature::Callbacks,
            "fn",
            "`fn` is disabled by policy (`features.callbacks`)",
        );
        refused(
            "fn id(x) { return x; }; return id(1);",
            Feature::Callbacks,
            "fn id",
            "`fn` is disabled by policy (`features.callbacks`)",
        );
        refused(
            "let o = { a: 1 }; return o.a;",
            Feature::ObjectLiterals,
            "{ a: 1 }",
            "`{ … }` is disabled by policy (`features.object_literals`)",
        );
        refused(
            "let a = [1, 2]; return a[0];",
            Feature::ArrayLiterals,
            "[1, 2]",
            "`[ … ]` is disabled by policy (`features.array_literals`)",
        );
        refused(
            "return send(1, 2);",
            Feature::RpcCalls,
            "send(1, 2)",
            "a host call to `send` is disabled by policy (`features.rpc_calls`)",
        );
    }

    #[test]
    fn every_construct_runs_with_no_toggle_set() {
        let source = "fn sum(xs) { let t = 0; for (x in xs) { t = t + x; }; return t; }; \
                      let n = 0; while (n < 2) { n = n + 1; }; \
                      let o = { v: sum([1, 2, 3]) }; host(o.v); \
                      if (n == 2) { return o.v + n; }; return 0;";
        let (result, calls) = run(source, Features::ALL_ENABLED);
        assert_eq!(result.unwrap().as_number(), Some(8.0));
        assert_eq!(calls, ["host"]);
    }

    #[test]
    fn a_nested_block_that_declares_a_function_is_refused_when_it_opens() {
        let source = "let r = 1; { fn g() { return 2; }; r = g(); }; return r;";
        let (result, _) = run(source, without(Feature::Callbacks));
        let error = result.unwrap_err();
        assert_eq!(error.code, Code::CONSTRUCT_DISABLED);
        assert_eq!(&source[error.span.range()], "fn g");
    }

    #[test]
    fn a_function_declared_in_a_loop_body_is_refused_when_the_body_opens() {
        let source =
            "let r = 0; for (x in [1]) { r = 1; fn g() { return 2; }; r = g(); }; return r;";
        let (result, _) = run(source, without(Feature::Callbacks));
        let error = result.unwrap_err();
        assert_eq!(error.code, Code::CONSTRUCT_DISABLED);
        assert_eq!(&source[error.span.range()], "fn g");
        // The body never ran: a host call placed first in it is never reached.
        let source = "for (x in [1]) { seen(); fn g() {}; }; return 0;";
        let (result, calls) = run(source, without(Feature::Callbacks));
        assert_eq!(result.unwrap_err().code, Code::CONSTRUCT_DISABLED);
        assert!(calls.is_empty(), "{calls:?}");
    }

    #[test]
    fn an_else_if_chain_is_refused_on_its_first_if() {
        let source = "let x = 2; if (x == 1) { return 1; } else if (x == 2) { return 2; } else { return 3; };";
        let (result, _) = run(source, without(Feature::Conditionals));
        let error = result.unwrap_err();
        assert_eq!(error.code, Code::CONSTRUCT_DISABLED);
        assert_eq!(error.span.range().start, source.find("if").unwrap());
        assert_eq!(&source[error.span.range()], "if");
    }

    #[test]
    fn toggles_still_hold_after_a_resume() {
        let program = Arc::new(parse("f(); while (true) {};").unwrap());
        let execution = Execution::with_variables(program, Vec::<(&str, Value)>::new())
            .unwrap()
            .with_features(without(Feature::Loops));
        let Ok(Outcome::HostCall(call)) = execution.run() else {
            panic!("stops at the host call first");
        };
        let error = call.resume(&Value::Null).run().err().expect("refused");
        assert_eq!(error.code, Code::CONSTRUCT_DISABLED);
        assert!(error.message.contains("`features.loops`"));
    }

    #[test]
    fn each_toggle_sits_at_its_own_index() {
        for (i, feature) in Feature::ALL.iter().enumerate() {
            assert_eq!(feature.index(), i, "{feature}");
        }
    }

    #[test]
    fn a_hoisted_top_level_function_fails_before_anything_runs() {
        let source = "f(1); fn g() { return 1; };";
        let (result, calls) = run(source, without(Feature::Callbacks));
        let error = result.unwrap_err();
        assert_eq!(error.code, Code::CONSTRUCT_DISABLED);
        assert_eq!(&source[error.span.range()], "fn g");
        assert!(calls.is_empty(), "{calls:?}");
    }

    #[test]
    fn a_construct_never_reached_does_not_fail() {
        let (result, _) = run(
            "if (false) { while (true) {} }; return 1;",
            without(Feature::Loops),
        );
        assert_eq!(result.unwrap().as_number(), Some(1.0));
        let (result, calls) = run(
            "let x = null; if (x != null) { send([1], { a: fn() {} }); }; return 2;",
            Features::ALL_ENABLED
                .with(Feature::RpcCalls, false)
                .with(Feature::ArrayLiterals, false)
                .with(Feature::ObjectLiterals, false)
                .with(Feature::Callbacks, false),
        );
        assert_eq!(result.unwrap().as_number(), Some(2.0));
        assert!(calls.is_empty());
    }

    #[test]
    fn rpc_calls_is_refused_before_arguments_are_checked() {
        let source = "return f(fn() {});";
        let (result, _) = run(source, without(Feature::RpcCalls));
        let error = result.unwrap_err();
        assert_eq!(error.code, Code::CONSTRUCT_DISABLED);
        assert_eq!(&source[error.span.range()], "f(fn() {})");
        // With the toggle on, the same call fails on its argument instead.
        let (result, _) = run(source, Features::ALL_ENABLED);
        assert_eq!(result.unwrap_err().code, Code::FUNCTION_ARGUMENT);
    }

    #[test]
    fn a_local_call_is_not_a_host_call() {
        let (result, _) = run("let f = 1; return f;", without(Feature::RpcCalls));
        assert_eq!(result.unwrap().as_number(), Some(1.0));
        // Calling a function the Script declares is an ordinary call, not a host call.
        let (result, _) = run(
            "fn f(x) { return x + 1; }; return f(1);",
            without(Feature::RpcCalls),
        );
        assert_eq!(result.unwrap().as_number(), Some(2.0));
    }

    #[test]
    fn evaluate_runs_with_every_construct_enabled() {
        let program = parse(
            "fn f(xs) { let t = 0; for (x in xs) { if (x) { t = t + 1; }; }; return { t: t }; }; \
             return f([1, 0, 1]).t;",
        )
        .unwrap();
        assert_eq!(
            hexput_interpreter::evaluate(&program).unwrap().as_number(),
            Some(2.0)
        );
    }
}

// --- Story 3.11: Value Secrets the Script cannot touch ---

mod value_secrets {
    use std::sync::Arc;

    use hexput_interpreter::{
        Argument, Array, Execution, Held, Object, Outcome, Secret, Value, evaluate,
    };

    fn secret(reference: &str) -> Secret {
        Secret::new(reference, None, Vec::new())
    }

    /// `r1`, keyed `User`, with further fields the interpreter only carries.
    fn user_secret() -> Secret {
        Secret::new(
            "r1",
            Some(Arc::from("User")),
            vec![0x81, 0xa4, b't', b'i', b'e', b'r', 3],
        )
    }

    /// `{name: "a"}` carrying [`user_secret`].
    fn user() -> Held {
        Held::plain(Value::Object(Object::from_held_entries(
            [("name", Held::plain(Value::String(Arc::from("a"))))],
            Some(user_secret()),
        )))
    }

    fn held(value: Value, reference: &str) -> Held {
        Held::new(value, Some(secret(reference)))
    }

    /// One host call the Script made: its name and arguments.
    type Made = (String, Vec<Argument>);

    /// Run `source` with `variables`, answering each host call with `answer(name)`; the result
    /// and the calls made.
    fn drive(
        source: &str,
        variables: Vec<(&str, Held)>,
        answer: impl Fn(&str) -> Held,
    ) -> (Held, Vec<Made>) {
        let program = Arc::new(hexput_parser::parse(source).unwrap());
        let mut execution = Execution::with_variables(program, variables).unwrap();
        let mut calls = Vec::new();
        loop {
            match execution.run().unwrap() {
                Outcome::Finished(result) => return (result, calls),
                Outcome::HostCall(call) => {
                    let reply = answer(call.name());
                    calls.push((call.name().to_owned(), call.arguments().to_vec()));
                    execution = call.resume_held(&reply);
                }
                _ => panic!("an unmetered execution never stops at a meter"),
            }
        }
    }

    fn run(source: &str, variables: Vec<(&str, Held)>) -> Held {
        drive(source, variables, |_| Held::plain(Value::Null)).0
    }

    /// The Reference ID `argument` travels with: its own place's, or its collection's.
    fn reference(argument: &Argument) -> String {
        argument
            .value
            .secret()
            .or(argument.secret.as_ref())
            .expect("every argument carries a Value Secret")
            .reference()
            .to_owned()
    }

    #[test]
    fn a_held_starting_variable_reads_like_the_plain_value() {
        let result = run("return u.name;", vec![("u", user())]);
        assert_eq!(result.value.as_str(), Some("a"));
        assert!(result.secret.is_none(), "a property read is a copy");
    }

    #[test]
    fn a_collection_returned_keeps_its_secret_byte_for_byte() {
        let result = run("return u;", vec![("u", user())]);
        let object = result.value.as_object().expect("an object");
        assert_eq!(object.secret(), Some(&user_secret()));
        assert_eq!(object.secret().unwrap().extra(), user_secret().extra());
        assert_eq!(object.get("name").and_then(Value::as_str), Some("a"));
        assert_eq!(object.len(), 1);
    }

    #[test]
    fn reading_the_secret_yields_null_on_every_value() {
        let result = run(
            "let k = \"__secret\"; \
             return [u.__secret, u[\"__secret\"], u?.__secret, 5 .__secret, \"s\".__secret, \
                     [1][\"__secret\"], null.__secret, null[\"__secret\"], true.__secret, u[k], \
                     n.__secret, fn() {}.__secret];",
            vec![("u", user()), ("n", held(Value::Number(1.0), "r2"))],
        );
        let array = result.value.as_array().expect("an array");
        assert_eq!(array.len(), 12);
        assert!(array.iter().all(Value::is_null), "{:?}", array.to_vec());
    }

    #[test]
    fn the_secret_through_a_computed_key_is_null_on_null_and_a_write_is_a_no_op() {
        let result = run(
            "let k = \"__secret\"; let z = null; z[k] = 1; z.__secret = 2; z[\"__secret\"] = 3; \
             5[k] = 4; return [null[k], z[k], z?.[k], z[\"__secret\"], z.__secret];",
            vec![],
        );
        let array = result.value.as_array().unwrap();
        assert_eq!(array.len(), 5);
        assert!(array.iter().all(Value::is_null));
    }

    #[test]
    fn any_other_key_on_null_is_still_a_null_access() {
        let program = hexput_parser::parse("let k = \"a\"; let z = null; return z[k];").unwrap();
        let error = evaluate(&program).unwrap_err();
        assert_eq!(error.code.as_str(), "reference.null_access");
        let program = hexput_parser::parse("let z = null; z[\"a\"] = 1;").unwrap();
        let error = evaluate(&program).unwrap_err();
        assert_eq!(error.code.as_str(), "reference.null_access");
    }

    #[test]
    fn writing_the_secret_is_ignored_and_the_key_never_exists() {
        let result = run(
            "u.__secret = 1; u[\"__secret\"] = 2; let n = 5; n.__secret = 3; \
             let o = {__secret: 3, a: 1}; let keys = []; let i = 0; \
             for (k in o) { keys[i] = k; i = i + 1; }; \
             for (k in u) { keys[i] = k; i = i + 1; }; \
             return [keys, o, u];",
            vec![("u", user())],
        );
        let parts = result.value.as_array().unwrap();
        let keys: Vec<&str> = parts
            .get(0)
            .unwrap()
            .as_array()
            .unwrap()
            .iter()
            .map(|k| k.as_str().unwrap())
            .collect();
        assert_eq!(keys, ["a", "name"]);
        let o = parts.get(1).unwrap().as_object().unwrap();
        assert_eq!(o.len(), 1, "only `a`");
        assert!(o.secret().is_none());
        let u = parts.get(2).unwrap().as_object().unwrap();
        assert_eq!(u.secret(), Some(&user_secret()), "unchanged");
        assert_eq!(u.len(), 1);
    }

    #[test]
    fn equality_truthiness_and_conversion_ignore_the_secret() {
        let result = run(
            "let t = 0; if (s) { t = t + 1; }; if (z) { t = t + 10; }; \
             return [u == u, s == \"x\", s + \"!\", !z, z == 0, t, e == null];",
            vec![
                ("u", user()),
                ("s", held(Value::String(Arc::from("x")), "r9")),
                ("z", held(Value::Number(0.0), "r8")),
                ("e", held(Value::Null, "r7")),
            ],
        );
        let plain = run(
            "let t = 0; if (s) { t = t + 1; }; if (z) { t = t + 10; }; \
             return [u == u, s == \"x\", s + \"!\", !z, z == 0, t, e == null];",
            vec![
                ("u", Held::plain(user().value)),
                ("s", Held::plain(Value::String(Arc::from("x")))),
                ("z", Held::plain(Value::Number(0.0))),
                ("e", Held::plain(Value::Null)),
            ],
        );
        let shown = |held: &Held| format!("{:?}", held.value.as_array().unwrap().to_vec());
        assert_eq!(shown(&result), shown(&plain));
        let parts = result.value.as_array().unwrap();
        assert_eq!(parts.get(0).unwrap().as_bool(), Some(true));
        assert_eq!(parts.get(1).unwrap().as_bool(), Some(true));
        assert_eq!(parts.get(2).unwrap().as_str(), Some("x!"));
        assert_eq!(parts.get(5).unwrap().as_number(), Some(1.0));
    }

    #[test]
    fn a_collection_sent_twice_carries_one_generated_reference_and_so_does_its_property() {
        let (_, calls) = drive("let o = {a: 1}; f(o); f(o); f(o.a);", vec![], |_| {
            Held::plain(Value::Null)
        });
        assert_eq!(calls.len(), 3);
        let first = &calls[0].1[0];
        let second = &calls[1].1[0];
        assert!(reference(first).starts_with("hx:0000000000000000:"));
        assert_eq!(reference(first), reference(second));
        let object = first.value.as_object().unwrap();
        let property = object
            .entry_secret("a")
            .expect("a nested scalar is held too");
        assert_ne!(property.reference(), reference(first));
        let again = second.value.as_object().unwrap().entry_secret("a").unwrap();
        assert_eq!(property, again, "stable for the rest of the execution");
        // Sent on its own, the property is the same place, with the same Reference ID.
        assert_eq!(reference(&calls[2].1[0]), property.reference());
    }

    #[test]
    fn a_backend_secret_is_sent_as_it_arrived() {
        let (_, calls) = drive("f(u);", vec![("u", user())], |_| Held::plain(Value::Null));
        let sent = calls[0].1[0].value.as_object().unwrap();
        assert_eq!(sent.secret(), Some(&user_secret()));
        assert!(
            sent.entry_secret("name")
                .unwrap()
                .reference()
                .starts_with("hx:")
        );
    }

    #[test]
    fn a_place_keeps_its_reference_and_a_copy_is_a_new_place() {
        let (_, calls) = drive(
            "f(n); let m = n; f(m); f(m); f(n + 0); f(n + 0); n = 9; f(n);",
            vec![("n", held(Value::Number(5.0), "r2"))],
            |_| Held::plain(Value::Null),
        );
        let references: Vec<String> = calls.iter().map(|(_, a)| reference(&a[0])).collect();
        assert_eq!(references[0], "r2");
        assert!(references[1].starts_with("hx:"));
        assert_eq!(
            references[1], references[2],
            "the copy's place keeps its generated ID"
        );
        assert_ne!(
            references[3], references[4],
            "a computed value is fresh each time"
        );
        assert_eq!(
            references[5], "r2",
            "writing a place keeps its Reference ID"
        );
        assert_eq!(calls[5].1[0].value.as_number(), Some(9.0));
    }

    #[test]
    fn generated_references_count_from_one_under_the_prefix() {
        let program = Arc::new(hexput_parser::parse("f(1); f(2);").unwrap());
        let mut execution = Execution::with_variables(program, Vec::<(&str, Value)>::new())
            .unwrap()
            .with_reference_prefix("hx:00000000000000ab");
        let mut seen = Vec::new();
        loop {
            match execution.run().unwrap() {
                Outcome::HostCall(call) => {
                    seen.push(reference(&call.arguments()[0]));
                    execution = call.resume(&Value::Null);
                }
                Outcome::Finished(_) => break,
                _ => unreachable!(),
            }
        }
        assert_eq!(seen, ["hx:00000000000000ab:1", "hx:00000000000000ab:2"]);
    }

    #[test]
    fn a_call_value_stored_directly_keeps_its_secret_and_otherwise_is_plain() {
        let (result, calls) = drive(
            "let x = f(); g(x); let o = {}; o.p = f(); g(o.p); let a = [0]; a[0] = f(); g(a[0]); \
             let y = 0; y = f(); g(y); g(f()); let z = [f()]; g(z[0]); return x;",
            vec![],
            |name| match name {
                "f" => held(Value::Number(7.0), "r3"),
                _ => Held::plain(Value::Null),
            },
        );
        let sent: Vec<String> = calls
            .iter()
            .filter(|(name, _)| name == "g")
            .map(|(_, a)| reference(&a[0]))
            .collect();
        assert_eq!(sent[..4], ["r3", "r3", "r3", "r3"]);
        assert!(
            sent[4].starts_with("hx:"),
            "a call's value passed on is computed"
        );
        assert!(
            sent[5].starts_with("hx:"),
            "an element of a new literal is a copy"
        );
        assert_eq!(result.secret, Some(secret("r3")), "returned from its place");
    }

    #[test]
    fn a_top_level_result_carries_a_secret_only_from_a_place() {
        let n = || vec![("n", held(Value::Number(5.0), "r2"))];
        assert_eq!(run("return n;", n()).secret, Some(secret("r2")));
        assert_eq!(run("let m = n; return m;", n()).secret, None);
        assert_eq!(run("return n + 0;", n()).secret, None);
        assert_eq!(run("return (n);", n()).secret, None);
        assert_eq!(
            run("return [n];", n())
                .value
                .as_array()
                .unwrap()
                .element_secret(0),
            None
        );
        let inside = run(
            "return o;",
            vec![(
                "o",
                Held::plain(Value::Array(Array::from_held(
                    vec![held(Value::Bool(true), "r5")],
                    None,
                ))),
            )],
        );
        let array = inside.value.as_array().unwrap();
        assert_eq!(array.element_secret(0), Some(&secret("r5")));
        assert!(
            array.secret().is_none(),
            "nothing is generated for a result"
        );
    }

    #[test]
    fn a_function_parameter_is_a_copy() {
        let (_, calls) = drive(
            "fn pass(v) { f(v); }; pass(n);",
            vec![("n", held(Value::Number(5.0), "r2"))],
            |_| Held::plain(Value::Null),
        );
        assert!(reference(&calls[0].1[0]).starts_with("hx:"));
    }

    #[test]
    fn secrets_cost_memory_but_are_not_allocations() {
        let program = Arc::new(hexput_parser::parse("return 1;").unwrap());
        let bare = Value::Object(Object::from_entries([(
            "name",
            Value::String(Arc::from("a")),
        )]));
        let plain = Execution::with_variables(Arc::clone(&program), vec![("u", bare)]).unwrap();
        let secret = Execution::with_variables(program, vec![("u", user())]).unwrap();
        assert!(secret.memory_used() > plain.memory_used());
        assert_eq!(secret.allocations(), plain.allocations());
    }

    #[test]
    fn an_object_built_outside_cannot_smuggle_the_key_in() {
        let object = Object::from_entries([("__secret", Value::Number(1.0)), ("a", Value::Null)]);
        assert_eq!(object.len(), 1);
        let result = run(
            "let n = 0; for (k in o) { n = n + 1; }; return n;",
            vec![("o", Held::plain(Value::Object(object)))],
        );
        assert_eq!(result.value.as_number(), Some(1.0));
    }

    #[test]
    fn evaluate_takes_and_returns_no_secrets() {
        let program = hexput_parser::parse("let o = {__secret: 1, a: 2}; return o;").unwrap();
        let result = evaluate(&program).unwrap();
        let object = result.as_object().unwrap();
        assert!(object.secret().is_none());
        assert_eq!(object.len(), 1);
    }
}

// --- Story 3.12: Registered Methods ---

mod methods {
    use std::sync::Arc;

    use hexput_interpreter::{
        Argument, Array, Diagnostic, Execution, Feature, Features, Held, Object, Outcome, Secret,
        Value, evaluate,
    };

    fn keyed(reference: &str, key: &str) -> Secret {
        Secret::new(reference, Some(Arc::from(key)), Vec::new())
    }

    /// `{name: "a"}` keyed `User` as `r1`, plus `extra` properties.
    fn user_with(extra: Vec<(&str, Value)>) -> Held {
        let mut entries = vec![("name", Held::plain(Value::String(Arc::from("a"))))];
        entries.extend(extra.into_iter().map(|(k, v)| (k, Held::plain(v))));
        Held::plain(Value::Object(Object::from_held_entries(
            entries,
            Some(keyed("r1", "User")),
        )))
    }

    fn user() -> Held {
        user_with(vec![])
    }

    /// One host call the Script made: name, key, receiver and arguments.
    struct Made {
        name: String,
        key: Option<String>,
        receiver: Option<Argument>,
        arguments: Vec<Argument>,
    }

    /// Run `source` knowing the methods `methods` under `features`, answering every host call
    /// with `answer`; the outcome and the calls made.
    fn drive_with(
        source: &str,
        variables: Vec<(&str, Held)>,
        methods: &[(&str, &str)],
        features: Features,
        answer: &Held,
    ) -> (Result<Held, Diagnostic>, Vec<Made>) {
        let program = Arc::new(hexput_parser::parse(source).unwrap());
        let mut execution = Execution::with_variables(program, variables)
            .unwrap()
            .with_features(features)
            .with_methods(methods.iter().copied());
        let mut calls = Vec::new();
        loop {
            match execution.run() {
                Err(error) => return (Err(error), calls),
                Ok(Outcome::Finished(result)) => return (Ok(result), calls),
                Ok(Outcome::HostCall(call)) => {
                    calls.push(Made {
                        name: call.name().to_owned(),
                        key: call.key().map(str::to_owned),
                        receiver: call.receiver().cloned(),
                        arguments: call.arguments().to_vec(),
                    });
                    execution = call.resume_held(answer);
                }
                Ok(_) => panic!("an unmetered execution never stops at a meter"),
            }
        }
    }

    fn drive(
        source: &str,
        variables: Vec<(&str, Held)>,
        methods: &[(&str, &str)],
    ) -> (Result<Held, Diagnostic>, Vec<Made>) {
        drive_with(
            source,
            variables,
            methods,
            Features::ALL_ENABLED,
            &Held::plain(Value::Number(42.0)),
        )
    }

    const SAVE: &[(&str, &str)] = &[("User", "save")];

    fn spanned<'a>(source: &'a str, diagnostic: &Diagnostic) -> &'a str {
        &source[diagnostic.span.range()]
    }

    #[test]
    fn a_method_call_stops_with_its_receiver_and_key() {
        let source = "return u.save(1);";
        let (result, calls) = drive(source, vec![("u", user())], SAVE);
        assert_eq!(result.unwrap().value.as_number(), Some(42.0));
        assert_eq!(calls.len(), 1);
        let call = &calls[0];
        assert_eq!(call.name, "save");
        assert_eq!(call.key.as_deref(), Some("User"));
        let receiver = call.receiver.as_ref().expect("a receiver");
        let object = receiver.value.as_object().unwrap();
        assert_eq!(object.secret(), Some(&keyed("r1", "User")));
        assert_eq!(object.get("name").and_then(Value::as_str), Some("a"));
        assert_eq!(&source[receiver.span.range()], "u");
        assert_eq!(call.arguments.len(), 1);
        assert_eq!(call.arguments[0].value.as_number(), Some(1.0));
    }

    #[test]
    fn the_call_spans_the_receiver_through_the_parenthesis() {
        let source = "return u.nope(1);";
        let program = Arc::new(hexput_parser::parse(source).unwrap());
        let execution = Execution::with_variables(program, vec![("u", user())]).unwrap();
        let Outcome::HostCall(call) = execution.run().unwrap() else {
            panic!("a host call");
        };
        assert_eq!(&source[call.span().range()], "u.nope(1)");
    }

    #[test]
    fn a_method_wins_over_an_own_property_of_its_name() {
        let (result, calls) = drive(
            "return u.save();",
            vec![("u", user_with(vec![("save", Value::Number(5.0))]))],
            SAVE,
        );
        assert_eq!(result.unwrap().value.as_number(), Some(42.0));
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "save");
    }

    #[test]
    fn a_name_neither_method_nor_property_stops_at_a_host_call_for_the_executor_to_refuse() {
        let (_, calls) = drive("return u.nope();", vec![("u", user())], SAVE);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "nope");
        assert_eq!(calls[0].key.as_deref(), Some("User"));
    }

    #[test]
    fn an_own_property_of_a_keyed_value_is_called_as_usual() {
        let (result, calls) = drive(
            "u.greet = fn() { return 2; }; return u.greet();",
            vec![("u", user())],
            SAVE,
        );
        assert_eq!(result.unwrap().value.as_number(), Some(2.0));
        assert!(calls.is_empty());
    }

    #[test]
    fn a_value_without_a_key_has_no_methods() {
        let (result, calls) = drive(
            "let o = {save: fn() { return 1; }}; return o.save();",
            vec![],
            SAVE,
        );
        assert_eq!(result.unwrap().value.as_number(), Some(1.0));
        assert!(calls.is_empty());
        // A secret without a key is no key either; nor is a plain value's missing property.
        let unkeyed = Held::plain(Value::Object(Object::from_held_entries(
            Vec::<(&str, Held)>::new(),
            Some(Secret::new("r9", None, Vec::new())),
        )));
        let (result, calls) = drive("return u.save();", vec![("u", unkeyed)], SAVE);
        assert_eq!(result.unwrap_err().code.as_str(), "type.not_callable");
        assert!(calls.is_empty());
        let (result, calls) = drive("return ({}).save();", vec![], SAVE);
        assert_eq!(result.unwrap_err().code.as_str(), "type.not_callable");
        assert!(calls.is_empty());
    }

    #[test]
    fn every_call_form_reaches_the_method() {
        for source in [
            "return u[\"save\"](1);",
            "let k = \"save\"; return u[k](1);",
            "return u?.save(1);",
            "let o = {inner: u}; return o.inner.save(1);",
            "let a = [u]; return a[0].save(1);",
        ] {
            let (result, calls) = drive(source, vec![("u", user())], SAVE);
            assert_eq!(result.unwrap().value.as_number(), Some(42.0), "{source}");
            assert_eq!(calls.len(), 1, "{source}");
            assert_eq!(calls[0].name, "save", "{source}");
            let receiver = calls[0].receiver.as_ref().unwrap();
            assert_eq!(
                receiver.value.secret().map(Secret::reference),
                Some("r1"),
                "{source}"
            );
        }
    }

    #[test]
    fn a_keyed_scalar_is_a_receiver_by_the_secret_of_its_place() {
        let n = Held::new(Value::Number(5.0), Some(keyed("r2", "Num")));
        let methods = &[("Num", "double")];
        for source in ["return n.double();", "return n[\"double\"]();"] {
            let (result, calls) = drive(source, vec![("n", n.clone())], methods);
            assert_eq!(result.unwrap().value.as_number(), Some(42.0), "{source}");
            let receiver = calls[0].receiver.as_ref().unwrap();
            assert_eq!(receiver.value.as_number(), Some(5.0));
            assert_eq!(receiver.secret, Some(keyed("r2", "Num")), "{source}");
            assert_eq!(calls[0].key.as_deref(), Some("Num"));
        }
        // Inside a collection: the property's or element's secret.
        let o = Held::plain(Value::Object(Object::from_held_entries(
            [("n", n.clone())],
            None,
        )));
        let a = Held::plain(Value::Array(Array::from_held(vec![n.clone()], None)));
        for source in ["return o.n.double();", "return a[0].double();"] {
            let (_, calls) = drive(source, vec![("o", o.clone()), ("a", a.clone())], methods);
            assert_eq!(calls.len(), 1, "{source}");
            let receiver = calls[0].receiver.as_ref().unwrap();
            assert_eq!(receiver.secret, Some(keyed("r2", "Num")), "{source}");
        }
        // A copy is plain, so it has no key and no methods.
        let (result, calls) = drive("let m = n; return m.double();", vec![("n", n)], methods);
        assert_eq!(
            result.unwrap_err().code.as_str(),
            "type.invalid_property_access"
        );
        assert!(calls.is_empty());
    }

    #[test]
    fn the_chain_carries_on_after_the_reply() {
        let reply = Held::plain(Value::Object(Object::from_entries([(
            "x",
            Value::Number(7.0),
        )])));
        let (result, _) = drive_with(
            "return u.save().x;",
            vec![("u", user())],
            SAVE,
            Features::ALL_ENABLED,
            &reply,
        );
        assert_eq!(result.unwrap().value.as_number(), Some(7.0));
        // A scalar reply stored directly keeps its secret, as a function's does.
        let reply = Held::new(
            Value::Number(3.0),
            Some(Secret::new("r7", None, Vec::new())),
        );
        let (result, _) = drive_with(
            "let x = u.save(); return x;",
            vec![("u", user())],
            SAVE,
            Features::ALL_ENABLED,
            &reply,
        );
        assert_eq!(result.unwrap().secret.unwrap().reference(), "r7");
    }

    #[test]
    fn a_method_cannot_be_overridden() {
        for (source, target) in [
            ("u.save = 1; return 0;", "u.save"),
            ("u[\"save\"] = 1; return 0;", "u[\"save\"]"),
            ("let k = \"save\"; u[k] = 1; return 0;", "u[k]"),
        ] {
            let (result, calls) = drive(source, vec![("u", user())], SAVE);
            let error = result.unwrap_err();
            assert_eq!(
                error.code.as_str(),
                "capability.method_override",
                "{source}"
            );
            assert_eq!(error.category.as_str(), "capability");
            assert_eq!(spanned(source, &error), target);
            assert!(calls.is_empty());
        }
        // Any other property may be written, and without the method list nothing is a method.
        let (result, _) = drive("u.other = 1; return u.other;", vec![("u", user())], SAVE);
        assert_eq!(result.unwrap().value.as_number(), Some(1.0));
        let (result, _) = drive("u.save = 1; return u.save;", vec![("u", user())], &[]);
        assert_eq!(result.unwrap().value.as_number(), Some(1.0));
    }

    #[test]
    fn an_override_is_refused_whatever_the_value_s_own_properties() {
        // Refused before anything is written, whether or not the value already has a property of
        // the method's name — and the error ends the Script, so nothing runs after it.
        let source = "u.save = 1; return f(u);";
        let (result, calls) = drive(
            source,
            vec![("u", user_with(vec![("save", Value::Number(5.0))]))],
            SAVE,
        );
        assert_eq!(
            result.unwrap_err().code.as_str(),
            "capability.method_override"
        );
        assert!(calls.is_empty());
    }

    #[test]
    fn rpc_calls_refuses_a_method_call_before_anything_is_checked() {
        let off = Features::ALL_ENABLED.with(Feature::RpcCalls, false);
        let source = "return u.save(fn() {});";
        let (result, calls) = drive_with(
            source,
            vec![("u", user())],
            SAVE,
            off,
            &Held::plain(Value::Null),
        );
        let error = result.unwrap_err();
        assert_eq!(error.code.as_str(), "policy.construct_disabled");
        assert_eq!(
            error.message,
            "a method call to `save` is disabled by policy (`features.rpc_calls`)"
        );
        assert_eq!(spanned(source, &error), "u.save(fn() {})");
        assert!(calls.is_empty());
        // An own property of a keyed value is no host call, so the toggle does not touch it.
        let (result, _) = drive_with(
            "u.greet = fn() { return 2; }; return u.greet();",
            vec![("u", user())],
            SAVE,
            off,
            &Held::plain(Value::Null),
        );
        assert_eq!(result.unwrap().value.as_number(), Some(2.0));
    }

    #[test]
    fn a_receiver_that_cannot_be_sent_is_refused_on_the_receiver() {
        let source = "u.me = u; return u.save();";
        let (result, calls) = drive(source, vec![("u", user())], SAVE);
        let error = result.unwrap_err();
        assert_eq!(error.code.as_str(), "type.cyclic_argument");
        assert_eq!(&source[error.span.range()], "u");
        assert!(calls.is_empty());
    }

    #[test]
    fn a_receiver_s_contents_get_references_like_an_argument_s() {
        let (_, calls) = drive("return u.save();", vec![("u", user())], SAVE);
        let object = calls[0]
            .receiver
            .as_ref()
            .unwrap()
            .value
            .as_object()
            .unwrap();
        let name = object.entry_secret("name").expect("a generated reference");
        assert!(name.reference().starts_with("hx:"));
    }

    #[test]
    fn optional_access_on_a_keyed_null_short_circuits() {
        let n = Held::new(Value::Null, Some(keyed("r3", "User")));
        let (result, calls) = drive("return n?.save();", vec![("n", n)], SAVE);
        assert!(result.unwrap().value.is_null());
        assert!(calls.is_empty());
    }

    #[test]
    fn a_keyed_array_has_methods_and_cannot_have_them_overridden() {
        let list = || {
            Held::plain(Value::Array(Array::from_held(
                vec![Held::plain(Value::Number(1.0))],
                Some(keyed("r4", "List")),
            )))
        };
        let methods = &[("List", "save")];
        let (_, calls) = drive("return a.save();", vec![("a", list())], methods);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].key.as_deref(), Some("List"));
        let source = "a[\"save\"] = 1; return 0;";
        let (result, calls) = drive(source, vec![("a", list())], methods);
        let error = result.unwrap_err();
        assert_eq!(error.code.as_str(), "capability.method_override");
        assert_eq!(spanned(source, &error), "a[\"save\"]");
        assert!(calls.is_empty());
    }

    #[test]
    fn calling_the_secret_is_calling_null() {
        let (result, calls) = drive(
            "return u[\"__secret\"]();",
            vec![("u", user())],
            &[("User", "save"), ("User", "__secret")],
        );
        assert_eq!(result.unwrap_err().code.as_str(), "type.not_callable");
        assert!(calls.is_empty());
    }

    #[test]
    fn a_method_call_inside_an_assignment_target_resumes_and_completes() {
        let reply = Held::plain(Value::Object(Object::from_entries([(
            "x",
            Value::Number(1.0),
        )])));
        let (result, calls) = drive_with(
            "u.save().x = 5; return 0;",
            vec![("u", user())],
            SAVE,
            Features::ALL_ENABLED,
            &reply,
        );
        assert_eq!(result.unwrap().value.as_number(), Some(0.0));
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "save");
    }

    #[test]
    fn the_same_call_dispatches_every_time_in_a_loop_and_in_recursion() {
        let n = Held::new(Value::Number(5.0), Some(keyed("r2", "Num")));
        let o = Held::plain(Value::Object(Object::from_held_entries([("n", n)], None)));
        for source in [
            "let i = 0; while (i < 3) { u.save(i); o.n.double(); i = i + 1; }; return 0;",
            "fn r(k) { if (k == 0) { return 0; }; u.save(k); o.n.double(); return r(k - 1); }; \
             return r(3);",
        ] {
            let (result, calls) = drive(
                source,
                vec![("u", user()), ("o", o.clone())],
                &[("User", "save"), ("Num", "double")],
            );
            result.unwrap();
            let seen: Vec<(&str, Option<&str>)> = calls
                .iter()
                .map(|call| (call.name.as_str(), call.key.as_deref()))
                .collect();
            assert_eq!(
                seen,
                [("save", Some("User")), ("double", Some("Num"))].repeat(3),
                "{source}"
            );
            for call in calls.iter().filter(|call| call.name == "double") {
                let receiver = call.receiver.as_ref().unwrap();
                assert_eq!(receiver.secret, Some(keyed("r2", "Num")), "{source}");
            }
        }
    }

    #[test]
    fn evaluate_knows_no_methods() {
        let program =
            hexput_parser::parse("let o = {save: fn() { return 1; }}; return o.save();").unwrap();
        assert_eq!(evaluate(&program).unwrap().as_number(), Some(1.0));
    }
}
