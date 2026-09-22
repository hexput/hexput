//! Printing a Script result in Hexput literal form (Story 1.9 decision 3).
//!
//! What is printed can be pasted straight back into a script, and is exactly the grammar
//! `--var name=<expression>` accepts as input — one surface, not two. Numbers go through the
//! language's own formatting, so a number has exactly one spelling wherever it appears.
//!
//! Nothing here recurses: a Script result's nesting is attacker-controlled, so the printer walks
//! an explicit work stack the way the interpreter's own detach does.

use std::sync::Arc;

use hexput_interpreter::{Value, number_to_string};

use crate::is_identifier;

/// One pending piece of output. `Text` carries the punctuation a collection owes after its
/// children, which is what lets the walk stay flat.
enum Step {
    Value(Value),
    Text(&'static str),
    Key(Arc<str>),
}

/// `value` in Hexput literal form, without a trailing newline.
pub(crate) fn literal(value: &Value) -> String {
    let mut out = String::new();
    let mut work = vec![Step::Value(value.clone())];
    while let Some(step) = work.pop() {
        match step {
            Step::Text(text) => out.push_str(text),
            Step::Key(key) => push_key(&mut out, &key),
            Step::Value(value) => push_value(&mut out, &mut work, value),
        }
    }
    out
}

fn push_value(out: &mut String, work: &mut Vec<Step>, value: Value) {
    match value {
        Value::Null => out.push_str("null"),
        Value::Bool(b) => out.push_str(if b { "true" } else { "false" }),
        Value::Number(n) => out.push_str(&number_to_string(n)),
        Value::String(s) => push_string(out, &s),
        Value::Array(array) => {
            let items = array.to_vec();
            out.push('[');
            if items.is_empty() {
                out.push(']');
                return;
            }
            let mut steps = Vec::with_capacity(items.len() * 2);
            for (index, item) in items.into_iter().enumerate() {
                if index > 0 {
                    steps.push(Step::Text(", "));
                }
                steps.push(Step::Value(item));
            }
            steps.push(Step::Text("]"));
            // Reversed so the steps come back off the stack in the order they were built.
            work.extend(steps.into_iter().rev());
        }
        Value::Object(object) => {
            let entries = object.entries();
            if entries.is_empty() {
                out.push_str("{}");
                return;
            }
            out.push_str("{ ");
            let mut steps = Vec::with_capacity(entries.len() * 4);
            for (index, (key, item)) in entries.into_iter().enumerate() {
                if index > 0 {
                    steps.push(Step::Text(", "));
                }
                steps.push(Step::Key(key));
                steps.push(Step::Text(": "));
                steps.push(Step::Value(item));
            }
            steps.push(Step::Text(" }"));
            work.extend(steps.into_iter().rev());
        }
        // `Value` is `#[non_exhaustive]`: the six §3 types above are all of it today, and a
        // seventh (a re-triggerable callable handle, §7) would need its own printed form before
        // it could ever reach a Script result. Printing `null` keeps this total meanwhile.
        _ => out.push_str("null"),
    }
}

/// An object key: bare where §2 allows it, quoted otherwise, so the printed object re-parses.
fn push_key(out: &mut String, key: &str) {
    if is_identifier(key) {
        out.push_str(key);
    } else {
        push_string(out, key);
    }
}

/// A string literal with §3 escapes. Non-ASCII content is printed verbatim — strings are full
/// Unicode (§2) — while control characters, which would be invisible or would break the line,
/// take the escape that round-trips them.
fn push_string(out: &mut String, text: &str) {
    out.push('"');
    for c in text.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c.is_control() => out.push_str(&format!("\\u{{{:x}}}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
}
