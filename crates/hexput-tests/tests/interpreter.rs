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
    let e = assert_error(
        "let a = null; return a[nope];",
        Category::Reference,
        Code::NULL_ACCESS,
        "[nope]",
    );
    assert!(e.message.contains("`a`"));
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
                Outcome::Finished(result) => return (calls, result),
                Outcome::HostCall(call) => {
                    let arguments: Vec<Value> =
                        call.arguments().iter().map(|a| a.value.clone()).collect();
                    let value = answer(call.name(), &arguments);
                    calls.push((call.name().to_owned(), arguments));
                    execution = call.resume(&value);
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
        assert_eq!(result.as_number(), Some(4.0));
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
