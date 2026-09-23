//! Direct/Cached Execution: AST cache (moka), parse/interpret entry points. Invokes
//! hexput-check exactly once per submission path — on Direct Execution and on
//! CodeRegister, never again on CachedExecutionStart — and funnels every execution through
//! hexput-exec's shared Executor.
//!
//! # What exists today
//!
//! [`direct_execution`] serves one `ExecutionStart` payload (FR-4): decode it, parse the source
//! with no AST Cache involved, run it through [`hexput_exec::execute`] — never the interpreter's
//! `evaluate*` directly (AD-3) — and turn the result into the `Result` payload `{value}`, or the
//! failure into the one [`ErrorBody`] shape. The connection only routes; everything about the
//! payload, the wire↔value conversion and the error lives here.
//!
//! Not yet: the static check (no check mode exists in Config until Epic 3), the AST Cache and
//! Cached Execution (Epic 4), Registered Function calls, Capabilities and Resource Budgets
//! (Epic 3, behind the same Executor).
//!
//! Binds: AD-3, AD-6, AD-8.

mod wire;

use std::collections::HashSet;
use std::sync::Arc;

use hexput_interpreter::{Category, Code, Diagnostic, Span, Value as Hexput};
use hexput_port::{ErrorBody, ProtocolCode, ProtocolError, Value};

pub use wire::MAX_RESULT_DEPTH;

const SOURCE: &str = "source";
const VARIABLES: &str = "variables";
const VALUE: &str = "value";

/// Serve one Direct Execution: `payload` is an `ExecutionStart` payload, a map with exactly
/// `source` (the Script, a string) and `variables` (its starting variables, a map from §2
/// identifier to value).
///
/// Returns the `Result` payload `{value: <the Script's result>}`. A number that is whole and
/// within ±2^53 is sent as a MessagePack integer (`-0` as `0`), every other number as a
/// float64; object keys keep their order.
///
/// # Errors
///
/// The `Error` payload, each exactly one of:
///
/// * `protocol.invalid_payload` — the payload is not as described, a name is not a §2 identifier
///   or appears twice, or a value has no lossless Hexput representation (an integer outside
///   ±2^53, a NaN or infinity, binary data, an extension, a non-string or repeated object key,
///   invalid UTF-8). The message names the key or path; nothing is parsed or run.
/// * The parser's or interpreter's [`Diagnostic`] — category, code, severity, message and span —
///   including `syntax.duplicate_declaration` for a starting variable the Script also declares.
/// * `protocol.result_too_deep` — the result nests past [`MAX_RESULT_DEPTH`].
/// * `protocol.response_too_large` — the result is certain to encode past the maximum frame.
///
/// The error is boxed: it is the reply's payload, built once per failed execution, and a large
/// `Err` would make every `Result` this returns as large as it.
pub fn direct_execution(payload: &Value) -> Result<Value, Box<ErrorBody>> {
    let refused = |body: ErrorBody| Box::new(body);
    let (source, variables) = decode(payload).map_err(|message| {
        refused(ErrorBody::from(&ProtocolError::new(
            ProtocolCode::InvalidPayload,
            message,
        )))
    })?;
    let program = hexput_parser::parse(source).map_err(|d| refused(ErrorBody::from(&d)))?;
    let result =
        hexput_exec::execute(&program, variables).map_err(|d| refused(ErrorBody::from(&d)))?;
    match wire::check_result(&result) {
        Ok(()) => Ok(Value::Map(vec![(
            Value::from(VALUE),
            wire::to_wire(&result),
        )])),
        Err(wire::Unsendable::Protocol(error)) => Err(refused(ErrorBody::from(&error))),
        Err(wire::Unsendable::Unrepresentable) => Err(refused(ErrorBody::from(&Diagnostic::new(
            Category::Type,
            Code::FUNCTION_RESULT,
            "the Script's result holds a value with no wire representation",
            Span::new(0, 0, 1, 1),
        )))),
    }
}

/// A decoded `ExecutionStart`: the source, and the starting variables in the order given.
type Decoded<'a> = (&'a str, Vec<(Arc<str>, Hexput)>);

/// Decode an `ExecutionStart` payload by hand, like `Init`'s: a Backend is owed the exact key,
/// name or path that was wrong.
fn decode(payload: &Value) -> Result<Decoded<'_>, String> {
    let fields: &[(Value, Value)] = match payload {
        Value::Nil => &[],
        Value::Map(fields) => fields,
        _ => {
            return Err(format!(
                "the `ExecutionStart` payload must be a map with `{SOURCE}` and `{VARIABLES}`"
            ));
        }
    };

    let mut source = None;
    let mut variables = None;
    for (key, value) in fields {
        let slot = match key.as_str() {
            Some(SOURCE) => &mut source,
            Some(VARIABLES) => &mut variables,
            Some(other) => {
                return Err(format!(
                    "the `ExecutionStart` payload has an unknown key `{}`",
                    wire::bounded(other)
                ));
            }
            None => {
                return Err(
                    "the `ExecutionStart` payload has a key that is not a string".to_owned(),
                );
            }
        };
        if slot.is_some() {
            return Err(format!(
                "the `ExecutionStart` payload repeats the key `{}`",
                key.as_str().unwrap_or_default()
            ));
        }
        *slot = Some(value);
    }

    // Absent or nil is missing; both missing keys are named together. Empty is not missing.
    let (source, variables) = match (present(source), present(variables)) {
        (Some(source), Some(variables)) => (source, variables),
        (None, Some(_)) => return Err(missing(&[SOURCE])),
        (Some(_), None) => return Err(missing(&[VARIABLES])),
        (None, None) => return Err(missing(&[SOURCE, VARIABLES])),
    };

    let Some(source) = source.as_str() else {
        return Err(format!("`{SOURCE}` is not a string"));
    };
    let Value::Map(entries) = variables else {
        return Err(format!("`{VARIABLES}` is not a map"));
    };

    let mut seen: HashSet<&str> = HashSet::with_capacity(entries.len());
    let mut bound = Vec::with_capacity(entries.len());
    for (name, value) in entries {
        let Some(name) = name.as_str() else {
            return Err(format!("`{VARIABLES}` has a key that is not a string"));
        };
        if !hexput_parser::is_identifier(name) {
            return Err(format!(
                "the starting variable {:?} is not a Hexput identifier (ASCII letters, digits and \
                 `_`, not starting with a digit, not a reserved word)",
                wire::bounded(name)
            ));
        }
        if !seen.insert(name) {
            return Err(format!(
                "`{VARIABLES}` names the starting variable `{}` twice",
                wire::bounded(name)
            ));
        }
        let value = wire::to_hexput(value, &mut wire::Path::variable(name))?;
        bound.push((Arc::from(name), value));
    }
    Ok((source, bound))
}

/// A key that is absent or nil is missing.
fn present(value: Option<&Value>) -> Option<&Value> {
    value.filter(|v| !v.is_nil())
}

fn missing(keys: &[&str]) -> String {
    let names: Vec<String> = keys.iter().map(|k| format!("`{k}`")).collect();
    format!(
        "the `ExecutionStart` payload is missing {}",
        names.join(" and ")
    )
}
