//! Direct/Cached Execution: AST cache (moka), parse/interpret entry points. Invokes
//! hexput-check exactly once per submission path — on Direct Execution and on
//! CodeRegister, never again on CachedExecutionStart — and funnels every execution through
//! hexput-exec's shared Executor.
//!
//! # What exists today
//!
//! [`direct_execution`] serves one `ExecutionStart` payload (FR-4): decode it, parse the source
//! with no AST Cache involved, run it through [`hexput_exec::execute`] — never the interpreter
//! directly (AD-3) — and turn the result into the `Result` payload `{value}`, or the failure into
//! the one [`ErrorBody`] shape. The connection only routes; everything about the payload and the
//! error lives here, and the wire↔value conversion is the Executor's (`hexput_exec::wire`), which
//! converts a host call's values the same way.
//!
//! The Script may call its Session's Registered Functions (Story 3.1): the connection passes the
//! registrations with their grants, read once per execution, and the [`Caller`] its calls travel
//! through, and the Executor decides (Story 3.2) and makes every call. This crate holds the
//! `Caller` only to pass it on (AD-3). Nothing in the type system stops it dispatching through it:
//! a source-text guard in `scripts/check-crate-graph.py` does, and a sealed token the compiler
//! enforces is still open. Decoding, parsing and converting the result run on the blocking pool,
//! like the Script itself, so a large payload never occupies a runtime worker.
//!
//! The execution runs under limits tuned per backend and per execution (Story 3.7): the
//! connection passes the Session's Config settings, read once per execution, and the payload's
//! optional `overrides` — decoded by `hexput-port`'s one settings decoder, exactly as `Init`'s
//! `config` is — are laid over them for this execution alone. The Executor enforces the result;
//! nothing stored changes. The same overlay carries the language feature toggles (Story 3.9).
//!
//! The static check (FR-26, Story 3.10) runs here, and only here on the Direct Execution path
//! (AD-8), when the effective `check` mode — the Session's Config overlaid with the payload's
//! `overrides` — is not `off`. It runs on the blocking pool right after parsing, before the
//! Executor, with the starting-variable names, every registration name as the callable list
//! (blanket or not: a per-call function is still callable) and the policy the effective feature
//! toggles describe. Under `error` a Script with any error-severity finding is rejected before
//! anything runs: the `Error` payload is the first error finding's body with every finding under
//! `findings`, logged at `debug`. Otherwise the Script runs and its reply — a successful
//! `Result`'s `{value}`, or an `Error` body when it then fails — carries `findings` when there
//! are any. At most [`MAX_FINDINGS`] are kept, in source order, and a reply whose findings would
//! not fit a frame is sent without them: the value, or the error, always wins. Under `off` no pass
//! runs at all. The check grants nothing and
//! charges nothing — it is not Script code, so it is not charged to the CPU budget — and its
//! findings are never cached.
//!
//! Registered Methods (Story 3.12) arrive among the registrations, each under its object key. The
//! static check takes only the functions' names as its callable list — a method is never called
//! by a bare name — and declares keyed every starting variable holding an array or object whose
//! Value Secret carries a key methods are registered under, so a Script overriding one is
//! reported (`capability.method_override`).
//!
//! Value Secrets (Story 3.11) cross here too: a starting variable may be a holder
//! (`{__secret: {ref, key?, …}, value}`, anywhere inside it as well), decoded by
//! `hexput_exec::wire` — a malformed one is `protocol.invalid_payload` naming its path — and the
//! result carries every secret it holds back out as holders, unchanged. A result with none is
//! exactly the payload it always was.
//!
//! Not yet: the AST Cache and Cached Execution (Epic 4), which will run the check once at
//! `CodeRegister`.
//!
//! Binds: AD-3, AD-6, AD-8.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use hexput_check::{Environment, Policy, Severity};
/// A Session's registration as Direct Execution takes it — a Registered Function, or a
/// Registered Method under an object key (Story 3.12) — re-exported so the connection, which
/// reads the Session's registrations, can hand them over without an edge to the Executor.
pub use hexput_exec::Registration;
use hexput_exec::wire;
use hexput_exec::{Caller, Host, Limits};
use hexput_interpreter::{Category, Code, Diagnostic, Held, Program, Span};
use hexput_port::{
    CheckMode, ErrorBody, MAX_FRAME_LEN, ProtocolCode, ProtocolError, Settings, Value,
    decode_settings, findings_value,
};

pub use hexput_exec::wire::MAX_RESULT_DEPTH;

const SOURCE: &str = "source";
const VARIABLES: &str = "variables";
const OVERRIDES: &str = "overrides";
const VALUE: &str = "value";
const FINDINGS: &str = "findings";

/// The most static check findings one reply carries, in source order (Story 3.10). A Script with
/// more has the rest dropped — except that a rejection's first error is always kept, in place of
/// the last one, so the rejection's own body is always among its `findings`.
pub const MAX_FINDINGS: usize = 100;

/// Room left in a frame for everything around a reply's payload: the envelope map, its `id`,
/// `type` and `payload` keys, the largest id and the longest type name, with margin.
const ENVELOPE_ROOM: usize = 64;
/// Serve one Direct Execution: `payload` is an `ExecutionStart` payload, a map with the keys
/// `source` (the Script, a string), `variables` (its starting variables, a map from §2
/// identifier to value) and optionally `overrides` (execution limits for this execution alone,
/// the shape of `Init`'s `config`; absent or nil sets none). The Script may call the Registered
/// Functions and Methods in `registrations` — each with its name, a method's object key, and
/// whether it holds a blanket grant — as the Executor allows, through `caller` (Stories 3.1–3.3,
/// 3.12).
///
/// It runs under `settings` — the Session's Config settings, as they were when the execution was
/// dispatched — overlaid with the payload's `overrides` (Story 3.7). Neither is changed: the
/// overlay is this execution's own.
///
/// Returns the `Result` payload `{value: <the Script's result>}`. A number that is whole and
/// within ±2^53 is sent as a MessagePack integer (`-0` as `0`), every other number as a
/// float64; object keys keep their order. When the effective `check` mode ran the static check
/// (Story 3.10) and it found anything, the payload is `{value, findings: [ … ]}`, each finding
/// in the [`ErrorBody`] shape, in source order, at most [`MAX_FINDINGS`] of them — and every
/// `Error` below that follows a pass carries them too. Findings that would not fit a frame are
/// left out.
///
/// Must run inside a Tokio runtime: the work runs on its blocking pool. Its timers must be enabled
/// when a registration lacks a blanket grant, since the Executor waits for a per-call handler's
/// answer under a timeout.
///
/// # Errors
///
/// The `Error` payload, each exactly one of:
///
/// * `protocol.invalid_payload` — the payload is not as described, a name is not a §2 identifier
///   or appears twice, a value has no lossless Hexput representation (an integer outside
///   ±2^53, a NaN or infinity, binary data, an extension, a non-string or repeated object key,
///   invalid UTF-8), or an override is unknown, mistyped or out of its range (never clamped).
///   The message names the key or path — and for an override out of range, the range; nothing
///   is parsed or run.
/// * The parser's, interpreter's or Executor's [`Diagnostic`] — category, code, severity, message
///   and span — including `syntax.duplicate_declaration` for a starting variable the Script also
///   declares, every `capability`, `host` and argument error of a host call, every `budget`
///   error, and `policy.construct_disabled` for a construct the effective `features` toggles
///   switch off (Story 3.9).
/// * Under check mode `error`, the first error-severity static check finding's body, with every
///   finding (warnings included, [`MAX_FINDINGS`] at most, the rejection's own among them) under
///   `findings`; nothing runs and no host call is made. Logged at `debug`.
/// * `protocol.result_too_deep` — the result nests past [`MAX_RESULT_DEPTH`].
/// * `protocol.response_too_large` — the result is certain to encode past the maximum frame.
///   Under the default Resource Budget the Executor refuses any such result first, as
///   `budget.output_size_exceeded` (Story 3.6): the output size budget is far below a frame. Only
///   an output size limit raised to the frame itself lets one reach this check.
///
/// The error is boxed: it is the reply's payload, built once per failed execution, and a large
/// `Err` would make every `Result` this returns as large as it.
///
/// # Panics
///
/// If the work on the blocking pool panics, the panic continues here.
pub async fn direct_execution(
    payload: Value,
    registrations: Vec<Registration>,
    settings: Settings,
    caller: Caller,
) -> Result<Value, Box<ErrorBody>> {
    // What the static check needs: the Registered Functions' names — the only names a bare call
    // can reach — and the Registered Methods by key.
    let names: Vec<String> = registrations
        .iter()
        .filter(|r| r.key.is_none())
        .map(|r| r.name.clone())
        .collect();
    let mut methods: HashMap<String, Vec<String>> = HashMap::new();
    for registration in &registrations {
        if let Some(key) = &registration.key {
            methods
                .entry(key.clone())
                .or_default()
                .push(registration.name.clone());
        }
    }
    let (program, variables, effective, checked) = blocking(move || {
        let (program, variables, overrides) = prepare(&payload)?;
        let effective = settings.overlay(&overrides);
        let checked = static_check(&program, &variables, names, &methods, &effective);
        Ok::<_, Box<ErrorBody>>((program, variables, effective, checked))
    })
    .await?;
    let findings = match checked {
        Ok(findings) => findings,
        Err(Rejection { body, found }) => {
            // Logged here, not on the blocking pool, so the event carries the request span.
            tracing::debug!(
                code = %body.code,
                findings = found,
                "rejected by the static check"
            );
            return Err(fitted(body));
        }
    };
    let limits = Limits::from_settings(&effective);
    let result =
        hexput_exec::execute_held(program, variables, Host::new(registrations, caller), limits)
            .await;
    blocking(move || match result {
        Ok(result) => reply(&result, findings),
        // The findings accompany a runtime failure too: the pass ran, so the Backend is owed them.
        Err(diagnostic) => Err(with_findings(ErrorBody::from(&diagnostic), findings)),
    })
    .await
}

/// A Script the static check rejected: the reply's body, and how many findings the pass made
/// (before [`MAX_FINDINGS`] applied).
struct Rejection {
    body: Box<ErrorBody>,
    found: usize,
}

/// `body` carrying `findings`, fitted to a frame.
fn with_findings(mut body: ErrorBody, findings: Vec<ErrorBody>) -> Box<ErrorBody> {
    body.findings = findings;
    fitted(Box::new(body))
}

/// `body` as it can be sent: without its `findings` when with them it would not fit a frame.
fn fitted(mut body: Box<ErrorBody>) -> Box<ErrorBody> {
    if !body.findings.is_empty() && encoded_len(&body.to_value()) > MAX_FRAME_LEN - ENVELOPE_ROOM {
        body.findings.clear();
    }
    body
}

/// The MessagePack length of `value`.
fn encoded_len(value: &Value) -> usize {
    let mut bytes = Vec::new();
    // Writing to a `Vec` cannot fail.
    let _ = hexput_port::rmpv::encode::write_value(&mut bytes, value);
    bytes.len()
}

/// Run the static check the effective `check` mode asks for (Story 3.10) and return its findings
/// as wire bodies — none at all under `off`, where no pass runs.
///
/// # Errors
///
/// Under `error`, a Script with an error-severity finding: the first one's body, with every
/// finding under `findings`.
fn static_check(
    program: &Program,
    variables: &[(Arc<str>, Held)],
    callables: Vec<String>,
    methods: &HashMap<String, Vec<String>>,
    effective: &Settings,
) -> Result<Vec<ErrorBody>, Rejection> {
    let mode = effective.check();
    if mode == CheckMode::Off {
        return Ok(Vec::new());
    }
    let mut environment = Environment::new()
        .with_variables(variables.iter().map(|(name, _)| name.to_string()))
        .with_callables(callables);
    // A starting variable holding an array or object whose own Value Secret carries a key the
    // Session registered methods under (Story 3.12). A keyed string, number, bool or `null` has
    // no properties to override — writing one is a `type` error — so it is never declared keyed.
    for (name, held) in variables {
        if let Some(methods) = held
            .value
            .secret()
            .and_then(|secret| secret.key())
            .and_then(|key| methods.get(key))
        {
            environment = environment.with_keyed(name.to_string(), methods.iter().cloned());
        }
    }
    let findings = hexput_check::check(
        program,
        &environment,
        &Policy::from_features(effective.features()),
    );
    let all = findings.diagnostics();
    let mut bodies: Vec<ErrorBody> = all.iter().take(MAX_FINDINGS).map(ErrorBody::from).collect();
    if mode == CheckMode::Error
        && let Some(at) = all
            .iter()
            .position(|finding| finding.severity == Severity::Error)
    {
        let first = ErrorBody::from(&all[at]);
        if at >= MAX_FINDINGS {
            // Every finding before it was kept up to the cap; it replaces the last, so source
            // order holds and the rejection is among its own findings.
            bodies.pop();
            bodies.push(first.clone());
        }
        let mut body = first;
        body.findings = bodies;
        return Err(Rejection {
            body: Box::new(body),
            found: all.len(),
        });
    }
    Ok(bodies)
}

/// Run `work` on the blocking pool and wait for it.
async fn blocking<T: Send + 'static>(work: impl FnOnce() -> T + Send + 'static) -> T {
    match tokio::task::spawn_blocking(work).await {
        Ok(done) => done,
        Err(error) => match error.try_into_panic() {
            Ok(panic) => std::panic::resume_unwind(panic),
            // Only a runtime shutting down cancels a blocking task, and it is dropping this
            // future too.
            Err(error) => panic!("a Direct Execution step was cancelled: {error}"),
        },
    }
}

/// A parsed Script, its starting variables and its overrides, ready for the Executor.
type Prepared = (Arc<Program>, Vec<(Arc<str>, Held)>, Settings);

/// Decode the payload and parse its source.
fn prepare(payload: &Value) -> Result<Prepared, Box<ErrorBody>> {
    let (source, variables, overrides) = decode(payload).map_err(|message| {
        Box::new(ErrorBody::from(&ProtocolError::new(
            ProtocolCode::InvalidPayload,
            message,
        )))
    })?;
    let program = hexput_parser::parse(source).map_err(|d| Box::new(ErrorBody::from(&d)))?;
    Ok((Arc::new(program), variables, overrides))
}

/// The `Result` payload for a Script's result and its static check findings (`findings` only
/// when there are any), or why it cannot be sent.
///
/// The findings are left out when with them the reply would not fit a frame, and they go with the
/// refusal when the result cannot be sent.
fn reply(result: &Held, findings: Vec<ErrorBody>) -> Result<Value, Box<ErrorBody>> {
    let refused = |error: ProtocolError| with_findings(ErrorBody::from(&error), findings.clone());
    match wire::result_to_wire(result) {
        Ok(value) => {
            let mut fields = vec![(Value::from(VALUE), value)];
            if !findings.is_empty() {
                let listed = findings_value(&findings);
                // The `findings` key and value, beside the `{value}` payload.
                let size =
                    wire::payload_size_held(&result.value, result.secret.as_ref(), MAX_FRAME_LEN)
                        .saturating_add(FINDINGS.len() + 1)
                        .saturating_add(encoded_len(&listed));
                if size <= MAX_FRAME_LEN - ENVELOPE_ROOM {
                    fields.push((Value::from(FINDINGS), listed));
                }
            }
            Ok(Value::Map(fields))
        }
        Err(wire::Unsendable::TooDeep) => Err(refused(ProtocolError::new(
            ProtocolCode::ResultTooDeep,
            format!(
                "the Script's result nests more than {MAX_RESULT_DEPTH} arrays or objects deep \
                 (counting its Value Secret holders), past what a frame may carry"
            ),
        ))),
        Err(wire::Unsendable::TooLarge) => Err(refused(ProtocolError::new(
            ProtocolCode::ResponseTooLarge,
            format!(
                "the Script's result encodes to more than the maximum frame of {MAX_FRAME_LEN} \
                 bytes"
            ),
        ))),
        Err(wire::Unsendable::Unrepresentable) => Err(with_findings(
            ErrorBody::from(&Diagnostic::new(
                Category::Type,
                Code::FUNCTION_RESULT,
                "the Script's result holds a value with no wire representation",
                Span::new(0, 0, 1, 1),
            )),
            findings,
        )),
    }
}

/// A decoded `ExecutionStart`: the source, the starting variables in the order given, and the
/// overrides (none set when the payload has none).
type Decoded<'a> = (&'a str, Vec<(Arc<str>, Held)>, Settings);

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
    let mut overrides = None;
    for (key, value) in fields {
        let slot = match key.as_str() {
            Some(SOURCE) => &mut source,
            Some(VARIABLES) => &mut variables,
            Some(OVERRIDES) => &mut overrides,
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
    // Checked before any variable is converted: a refused override runs nothing, and costs
    // nothing more.
    let overrides = match present(overrides) {
        None => Settings::new(),
        Some(overrides) => decode_settings(overrides, OVERRIDES)?,
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
    Ok((source, bound, overrides))
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
