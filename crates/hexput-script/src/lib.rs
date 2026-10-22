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
//! Modifications flow back too (Story 3.13): a successful `Result` is `{value, modifications}`
//! when the Script wrote any referenced place or collection — each `{ref, value}` once, with its
//! final value — and exactly the payload it always was when it wrote none. `findings` follows
//! them. The frame checks cover the whole payload, and an `Error` never carries modifications.
//!
//! [`code_register`] serves one `CodeRegister` (FR-5, Story 4.1): decode `{source, variables?}`,
//! parse the source once, run the static check when the Session's check mode asks for it — the
//! same pass as Direct Execution's, run at registration and never again when the Script is later
//! executed (AD-8) — and only then store the AST in the [`AstCache`] under the registering
//! Session's Client ID and a fresh [`ScriptHandle`], answering `{handle}` (with `findings` when
//! the pass found anything). Decoding, parsing and checking run on the blocking pool. A refused
//! registration leaves the cache exactly as it was. Direct Execution never touches the cache
//! (FR-4).
//!
//! [`cached_execution`] serves one `CachedExecutionStart` (FR-5, Story 4.2): decode
//! `{handle, variables, overrides?}`, look the handle up in the [`AstCache`] under the caller's
//! Client ID, and run the cached `Arc<Program>` with fresh starting variables through exactly the
//! path a Direct Execution takes after parsing — the same Executor, registrations, limits and
//! reply conversion, one private helper shared by both (AD-3). Nothing is parsed and the static
//! check does not run: it ran at registration (AD-8), so `overrides.check` is refused and the
//! Session's live check mode is not consulted, while the feature toggles are still enforced at
//! runtime. The registrations and settings are the Session's live ones, read at dispatch (AD-5).
//! The cached program is immutable and shared; every run builds its own heap, scopes and budget,
//! so nothing one run writes is visible to another. A handle this Session has no entry under —
//! never issued, another Session's, or no longer cached — is `protocol.unknown_script_handle`,
//! telling the Backend to register again; the payload is decoded fully first, so a malformed
//! one is `protocol.invalid_payload` whatever its handle.
//!
//! Not yet: the cache's capacity, eviction, unregister and Session-teardown cleanup (Story 4.3) —
//! until then an entry lives until the Daemon exits.
//!
//! Binds: AD-3, AD-6, AD-8.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use hexput_check::{Environment, Policy, Severity};
/// A Session's registration as Direct Execution takes it — a Registered Function, or a
/// Registered Method under an object key (Story 3.12) — re-exported so the connection, which
/// reads the Session's registrations, can hand them over without an edge to the Executor.
pub use hexput_exec::Registration;
use hexput_exec::wire;
use hexput_exec::{Caller, Finished, Host, Limits};
use hexput_interpreter::{Category, Code, Diagnostic, Held, Program, Span};
use hexput_port::{
    CheckMode, ClientId, ErrorBody, MAX_FRAME_LEN, ProtocolCode, ProtocolError, ScriptHandle,
    Settings, Value, decode_settings, findings_value,
};

pub use hexput_exec::wire::{MAX_MODIFICATION_DEPTH, MAX_RESULT_DEPTH};

const SOURCE: &str = "source";
const VARIABLES: &str = "variables";
const OVERRIDES: &str = "overrides";
const VALUE: &str = "value";
const FINDINGS: &str = "findings";
const HANDLE: &str = "handle";

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
/// left out. When the Script wrote a referenced place or collection (Story 3.13), the payload has
/// `modifications` after `value` — `[{ref, value}, …]`, each Reference ID once, in the order of its
/// first write, with its final value — and before `findings`; an `Error` never has them.
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
/// * `protocol.result_too_deep` — the result nests past [`MAX_RESULT_DEPTH`], or a modification's
///   value past [`MAX_MODIFICATION_DEPTH`] (holders counted).
/// * `protocol.response_too_large` — the result and its modifications are certain to encode past
///   the maximum frame.
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
        let checked = (effective.check() != CheckMode::Off).then(|| {
            let starting = variables.iter().map(|(name, _)| name.to_string()).collect();
            static_check(
                &program,
                starting,
                names,
                keyed(&variables, &methods),
                &effective,
            )
        });
        Ok::<_, Box<ErrorBody>>((program, variables, effective, checked))
    })
    .await?;
    let findings = passed(checked.unwrap_or(Ok(Vec::new())))?;
    run(
        program,
        variables,
        &effective,
        findings,
        registrations,
        caller,
    )
    .await
}

/// Run a parsed Script to its reply: the one path every execution takes after its program is in
/// hand, shared by Direct and Cached Execution (AD-3). The Executor runs `program` under the limits
/// `effective` describes, with the Host `registrations` and `caller` make, and its result becomes
/// the `Result` payload (with `findings`, when there are any) or the `Error` body.
async fn run(
    program: Arc<Program>,
    variables: Vec<(Arc<str>, Held)>,
    effective: &Settings,
    findings: Vec<ErrorBody>,
    registrations: Vec<Registration>,
    caller: Caller,
) -> Result<Value, Box<ErrorBody>> {
    let limits = Limits::from_settings(effective);
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

/// Serve one Cached Execution (FR-5, Story 4.2): `payload` is a `CachedExecutionStart` payload, a
/// map with the keys `handle` (the 16-lowercase-hex-character handle a `CodeRegister` was answered
/// with), `variables` (its starting variables, exactly as `ExecutionStart`'s, holders included;
/// `{}` is valid) and optionally `overrides` (exactly as `ExecutionStart`'s, except that `check`
/// is refused). The Script registered under `handle` by the Session `client_id` names is taken
/// from `cache` and run with those variables; nothing is parsed.
///
/// It runs exactly as a Direct Execution does once parsed: through the one Executor, under
/// `settings` — the Session's Config settings as they were when the execution was dispatched
/// (AD-5) — overlaid with the payload's `overrides`, with the Registered Functions and Methods in
/// `registrations` callable through `caller` as the Executor allows. The static check does not run
/// (AD-8): it ran at registration, so whatever the live `check` mode, the reply never carries
/// `findings`. The feature toggles are enforced at runtime as always.
///
/// Every run is fresh: the cached program is shared and immutable, and each run builds its own
/// heap, scopes and Resource Budget, so nothing one run writes is visible to another. A run holds
/// its own handle on the program, so it finishes safely even if the entry is removed meanwhile.
///
/// Returns the `Result` payload `{value}`, with `modifications` after `value` when the Script
/// wrote a referenced place or collection — exactly as [`direct_execution`] does.
///
/// Must run inside a Tokio runtime: the work runs on its blocking pool. Its timers must be enabled
/// when a registration lacks a blanket grant.
///
/// # Errors
///
/// The `Error` payload, each exactly one of:
///
/// * `protocol.invalid_payload` — the payload is not a map; `handle` or `variables` is missing
///   (absent or nil; both named together when both are); a key is unknown (`source` included:
///   nothing is parsed), repeated or not a string; `handle` is not a string or not a Script
///   handle (echoed bounded); `variables` is malformed exactly as for [`direct_execution`]; an
///   override is unknown, mistyped or out of its range; or `overrides.check` is given. Nothing
///   runs, and the handle is not looked up.
/// * `protocol.unknown_script_handle` — a well-formed handle with no entry under `client_id`:
///   never issued, registered by another Session (the same message, so nothing reveals that it
///   exists), or no longer cached. The message names the handle and says to register the Script
///   again with `CodeRegister`. Nothing runs.
/// * The interpreter's or Executor's [`Diagnostic`], exactly as for [`direct_execution`]:
///   `syntax.duplicate_declaration` for a starting variable the Script also declares, every
///   `reference`, `type`, `capability`, `host`, `budget` and `policy` error.
/// * `protocol.result_too_deep` or `protocol.response_too_large`, exactly as for
///   [`direct_execution`].
///
/// # Panics
///
/// If the work on the blocking pool panics, the panic continues here.
pub async fn cached_execution(
    payload: Value,
    client_id: ClientId,
    cache: Arc<AstCache>,
    registrations: Vec<Registration>,
    settings: Settings,
    caller: Caller,
) -> Result<Value, Box<ErrorBody>> {
    let (program, variables, effective) = blocking(move || {
        let (handle, variables, overrides) =
            decode(&payload, Request::CachedExecution).map_err(invalid_payload)?;
        let handle: ScriptHandle = handle.parse().map_err(|_| {
            invalid_payload(format!(
                "`{HANDLE}` is not a Script handle ({} lowercase hex characters); found {:?}",
                ScriptHandle::TEXT_LEN,
                wire::bounded(handle)
            ))
        })?;
        // Only now, with the whole payload accepted, is the handle looked up: a malformed request
        // is answered as one whatever its handle names.
        let program = cache
            .get(client_id, handle)
            .ok_or_else(|| unknown_handle(handle))?;
        Ok::<_, Box<ErrorBody>>((program, variables, settings.overlay(&overrides)))
    })
    .await?;
    run(
        program,
        variables,
        &effective,
        Vec::new(),
        registrations,
        caller,
    )
    .await
}

/// The `protocol.unknown_script_handle` body for `handle`. It names only the handle asked for, so
/// a handle another Session registered reads exactly like one never issued.
fn unknown_handle(handle: ScriptHandle) -> Box<ErrorBody> {
    Box::new(ErrorBody::from(&ProtocolError::new(
        ProtocolCode::UnknownScriptHandle,
        format!(
            "no Script is registered under handle `{handle}` for this Session; register it again \
             with `CodeRegister`"
        ),
    )))
}

/// The AST Cache (Story 4.1): every registered Script's parsed AST, keyed by the registering
/// Session's Client ID and the [`ScriptHandle`] its registration was answered with.
///
/// One per Daemon run, created by the Daemon beside its Session registry and shared by every
/// connection. A lookup under another Client ID never finds an entry, so identical source
/// registered by two Sessions makes two entries that cannot collide. Handles come from one
/// counter, starting at 1, so no handle is ever reused during the run. There is no capacity bound
/// and no eviction yet (Story 4.3).
pub struct AstCache {
    entries: moka::sync::Cache<(ClientId, ScriptHandle), Arc<Program>>,
    /// The next handle's value.
    next: AtomicU64,
}

impl AstCache {
    /// An empty cache whose first handle is `0000000000000001`.
    #[must_use]
    pub fn new() -> Self {
        Self {
            entries: moka::sync::Cache::builder().build(),
            next: AtomicU64::new(1),
        }
    }

    /// The program `client_id` registered under `handle`, if it is there.
    #[must_use]
    pub fn get(&self, client_id: ClientId, handle: ScriptHandle) -> Option<Arc<Program>> {
        self.entries.get(&(client_id, handle))
    }

    /// Every handle `client_id` has an entry under, in issue order.
    ///
    /// This walks every entry of every Session — linear in the whole cache, not in `client_id`'s
    /// share of it — so it is for inspection, not for a hot path.
    #[must_use]
    pub fn handles(&self, client_id: ClientId) -> Vec<ScriptHandle> {
        let mut handles: Vec<ScriptHandle> = self
            .entries
            .iter()
            .filter(|(key, _)| key.0 == client_id)
            .map(|(key, _)| key.1)
            .collect();
        handles.sort_unstable();
        handles
    }

    /// How many entries the cache holds, exactly: pending maintenance is run first.
    #[must_use]
    pub fn entry_count(&self) -> u64 {
        self.entries.run_pending_tasks();
        self.entries.entry_count()
    }

    /// Store `program` for `client_id` under a handle never issued before, and return it.
    fn insert(&self, client_id: ClientId, program: Arc<Program>) -> ScriptHandle {
        let handle = ScriptHandle::new(self.next.fetch_add(1, Ordering::Relaxed));
        self.entries.insert((client_id, handle), program);
        handle
    }
}

impl Default for AstCache {
    fn default() -> Self {
        Self::new()
    }
}

impl core::fmt::Debug for AstCache {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("AstCache")
            // Without running pending maintenance: unlike `entry_count`, an approximation.
            .field("approximate_entries", &self.entries.entry_count())
            .field("next", &self.next)
            .finish()
    }
}

/// Serve one registration (FR-5, Story 4.1): `payload` is a `CodeRegister` payload, a map with the
/// key `source` (the Script, a string) and optionally `variables` (an array of its
/// starting-variable names, §2 identifiers each given once; absent or nil names none). The names
/// are for the static check alone: a registration has no values, and nothing runs.
///
/// The source is parsed once. When the `check` mode in `settings` — the Session's Config as it
/// is at registration (AD-5) — is not `off`, the static check runs exactly as on Direct Execution
/// (Story 3.10): the starting-variable names, the Registered Functions in `registrations` as the
/// callable list, and the policy the Session's feature toggles describe. No variable is declared
/// keyed, since none has a value, so a method override is never reported here. The pass is not
/// charged to any budget and its findings are never cached. Only then is the AST stored in
/// `cache` under `client_id` and a fresh handle.
///
/// Returns the `Result` payload `{handle: "<16 lowercase hex>"}`, with `findings` after it — at
/// most [`MAX_FINDINGS`], in source order, left out if they would not fit a frame — when the pass
/// ran and found anything.
///
/// Must run inside a Tokio runtime: the work runs on its blocking pool.
///
/// # Errors
///
/// The `Error` payload, each exactly one of, and in every case no entry is created:
///
/// * `protocol.invalid_payload` — the payload is not a map, `source` is missing (absent or nil)
///   or not a string, a key is unknown (`overrides` included: nothing runs at registration),
///   repeated or not a string, `variables` is not an array, or one of its items is not a string,
///   not a §2 identifier, or repeated. The message names the key, or the item by its index.
/// * The parser's [`Diagnostic`] — category, code, severity, message and span.
/// * Under check mode `error`, the first error-severity static check finding's body, with every
///   finding (warnings included, [`MAX_FINDINGS`] at most, the rejection's own among them) under
///   `findings`. Logged at `debug`.
///
/// # Panics
///
/// If the work on the blocking pool panics, the panic continues here.
pub async fn code_register(
    payload: Value,
    client_id: ClientId,
    cache: Arc<AstCache>,
    registrations: Vec<Registration>,
    settings: Settings,
) -> Result<Value, Box<ErrorBody>> {
    let callables: Vec<String> = registrations
        .into_iter()
        .filter(|r| r.key.is_none())
        .map(|r| r.name)
        .collect();
    let registered = blocking(move || {
        let (source, starting) = decode_registration(&payload).map_err(invalid_payload)?;
        let program = hexput_parser::parse(source).map_err(|d| Box::new(ErrorBody::from(&d)))?;
        // An entry is inserted only once the pass let the Script through.
        Ok::<_, Box<ErrorBody>>(
            static_check(&program, starting, callables, Vec::new(), &settings)
                .map(|findings| (cache.insert(client_id, Arc::new(program)), findings)),
        )
    })
    .await?;
    let (handle, findings) = registered.map_err(rejected)?;
    let mut fields = vec![(Value::from(HANDLE), Value::from(handle.to_string()))];
    let size = encoded_len(&Value::Map(fields.clone()));
    push_findings(&mut fields, size, &findings);
    Ok(Value::Map(fields))
}

/// A `protocol.invalid_payload` body carrying `message`.
fn invalid_payload(message: String) -> Box<ErrorBody> {
    Box::new(ErrorBody::from(&ProtocolError::new(
        ProtocolCode::InvalidPayload,
        message,
    )))
}

/// Decode a `CodeRegister` payload by hand, like `ExecutionStart`'s: the source, and the
/// starting-variable names in the order given (none when `variables` is absent or nil).
fn decode_registration(payload: &Value) -> Result<(&str, Vec<String>), String> {
    let fields: &[(Value, Value)] = match payload {
        Value::Nil => &[],
        Value::Map(fields) => fields,
        _ => {
            return Err(format!(
                "the `CodeRegister` payload must be a map with `{SOURCE}`"
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
                    "the `CodeRegister` payload has an unknown key `{}`",
                    wire::bounded(other)
                ));
            }
            None => {
                return Err("the `CodeRegister` payload has a key that is not a string".to_owned());
            }
        };
        if slot.is_some() {
            return Err(format!(
                "the `CodeRegister` payload repeats the key `{}`",
                key.as_str().unwrap_or_default()
            ));
        }
        *slot = Some(value);
    }

    let Some(source) = present(source) else {
        return Err(format!("the `CodeRegister` payload is missing `{SOURCE}`"));
    };
    let Some(source) = source.as_str() else {
        return Err(format!("`{SOURCE}` is not a string"));
    };
    let items: &[Value] = match present(variables) {
        None => &[],
        Some(Value::Array(items)) => items,
        Some(_) => return Err(format!("`{VARIABLES}` is not an array of names")),
    };
    let mut seen: HashSet<&str> = HashSet::with_capacity(items.len());
    let mut names = Vec::with_capacity(items.len());
    for (index, item) in items.iter().enumerate() {
        let Some(name) = item.as_str() else {
            return Err(format!("`{VARIABLES}[{index}]` is not a string"));
        };
        if !hexput_parser::is_identifier(name) {
            return Err(format!(
                "`{VARIABLES}[{index}]`, the starting variable {:?}, is not a Hexput identifier \
                 (ASCII letters, digits and `_`, not starting with a digit, not a reserved word)",
                wire::bounded(name)
            ));
        }
        if !seen.insert(name) {
            return Err(format!(
                "`{VARIABLES}[{index}]` names the starting variable `{}` a second time",
                wire::bounded(name)
            ));
        }
        names.push(name.to_owned());
    }
    Ok((source, names))
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

/// Append `findings` to a successful reply's `fields` under `findings`, unless the reply — `size`
/// bytes without them — would then not fit a frame. The one fitting rule for every successful
/// reply that carries findings (Direct Execution's `{value}`, a registration's `{handle}`).
fn push_findings(fields: &mut Vec<(Value, Value)>, size: usize, findings: &[ErrorBody]) {
    if findings.is_empty() {
        return;
    }
    let listed = findings_value(findings);
    // The `findings` key and value, beside the payload without them.
    let size = size
        .saturating_add(FINDINGS.len() + 1)
        .saturating_add(encoded_len(&listed));
    if size <= MAX_FRAME_LEN - ENVELOPE_ROOM {
        fields.push((Value::from(FINDINGS), listed));
    }
}

/// The MessagePack length of `value`.
fn encoded_len(value: &Value) -> usize {
    let mut bytes = Vec::new();
    // Writing to a `Vec` cannot fail.
    let _ = hexput_port::rmpv::encode::write_value(&mut bytes, value);
    bytes.len()
}

/// The findings of a pass that let the Script through, or — logged at `debug` — the rejection's
/// body, fitted to a frame.
///
/// Called on the async task, not on the blocking pool, so the event carries the request span.
fn passed(checked: Result<Vec<ErrorBody>, Rejection>) -> Result<Vec<ErrorBody>, Box<ErrorBody>> {
    checked.map_err(rejected)
}

/// A rejection's body, fitted to a frame, logged at `debug` — on the async task, like [`passed`].
fn rejected(Rejection { body, found }: Rejection) -> Box<ErrorBody> {
    tracing::debug!(
        code = %body.code,
        findings = found,
        "rejected by the static check"
    );
    fitted(body)
}

/// The starting variables holding an array or object whose own Value Secret carries a key the
/// Session registered methods under (Story 3.12), each with those methods' names. A keyed string,
/// number, bool or `null` has no properties to override — writing one is a `type` error — so it is
/// never declared keyed.
fn keyed(
    variables: &[(Arc<str>, Held)],
    methods: &HashMap<String, Vec<String>>,
) -> Vec<(String, Vec<String>)> {
    variables
        .iter()
        .filter_map(|(name, held)| {
            held.value
                .secret()
                .and_then(|secret| secret.key())
                .and_then(|key| methods.get(key))
                .map(|methods| (name.to_string(), methods.clone()))
        })
        .collect()
}

/// Run the static check the `check` mode in `effective` asks for (Story 3.10) and return its
/// findings as wire bodies — none at all under `off`, where no pass runs. Shared by Direct
/// Execution and registration (Story 4.1), the two submission paths (AD-8).
///
/// `starting` are the starting-variable names, `callables` the Registered Functions' names and
/// `keyed` the starting variables declared keyed, with their methods.
///
/// # Errors
///
/// Under `error`, a Script with an error-severity finding: the first one's body, with every
/// finding under `findings`.
fn static_check(
    program: &Program,
    starting: Vec<String>,
    callables: Vec<String>,
    keyed: Vec<(String, Vec<String>)>,
    effective: &Settings,
) -> Result<Vec<ErrorBody>, Rejection> {
    let mode = effective.check();
    if mode == CheckMode::Off {
        return Ok(Vec::new());
    }
    let mut environment = Environment::new()
        .with_variables(starting)
        .with_callables(callables);
    for (name, methods) in keyed {
        environment = environment.with_keyed(name, methods);
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
            Err(error) => panic!("a Script step was cancelled: {error}"),
        },
    }
}

/// A parsed Script, its starting variables and its overrides, ready for the Executor.
type Prepared = (Arc<Program>, Vec<(Arc<str>, Held)>, Settings);

/// Decode the payload and parse its source.
fn prepare(payload: &Value) -> Result<Prepared, Box<ErrorBody>> {
    let (source, variables, overrides) =
        decode(payload, Request::DirectExecution).map_err(invalid_payload)?;
    let program = hexput_parser::parse(source).map_err(|d| Box::new(ErrorBody::from(&d)))?;
    Ok((Arc::new(program), variables, overrides))
}

/// The `Result` payload for a Script's result, its modifications (`modifications` only when
/// there are any, Story 3.13) and its static check findings (`findings` only when there are any),
/// or why it cannot be sent.
///
/// The findings are left out when with them the reply would not fit a frame, and they go with the
/// refusal when the result cannot be sent.
fn reply(finished: &Finished, findings: Vec<ErrorBody>) -> Result<Value, Box<ErrorBody>> {
    let refused = |error: ProtocolError| with_findings(ErrorBody::from(&error), findings.clone());
    let Finished {
        result,
        modifications,
    } = finished;
    // The result and its modifications share one frame; `true` marks a failure of the latter.
    let mut budget = MAX_FRAME_LEN;
    let converted = wire::result_to_wire_within(result, &mut budget)
        .map_err(|unsendable| (false, unsendable))
        .and_then(|value| {
            if modifications.is_empty() {
                return Ok((value, None));
            }
            wire::modifications_to_wire(modifications, &mut budget)
                .map(|listed| (value, Some(listed)))
                .map_err(|unsendable| (true, unsendable))
        });
    match converted {
        Ok((value, listed)) => {
            let mut fields = vec![(Value::from(VALUE), value)];
            if let Some(listed) = listed {
                fields.push((Value::from(wire::MODIFICATIONS_KEY), listed));
            }
            if !findings.is_empty() {
                let size = wire::result_payload_size(result, modifications, MAX_FRAME_LEN);
                push_findings(&mut fields, size, &findings);
            }
            Ok(Value::Map(fields))
        }
        Err((false, wire::Unsendable::TooDeep)) => Err(refused(ProtocolError::new(
            ProtocolCode::ResultTooDeep,
            format!(
                "the Script's result nests more than {MAX_RESULT_DEPTH} arrays or objects deep \
                 (counting its Value Secret holders), past what a frame may carry"
            ),
        ))),
        Err((true, wire::Unsendable::TooDeep)) => Err(refused(ProtocolError::new(
            ProtocolCode::ResultTooDeep,
            format!(
                "a value the Script wrote to a referenced place nests more than \
                 {MAX_MODIFICATION_DEPTH} arrays or objects deep (counting its Value Secret \
                 holders), past what a frame may carry in `modifications`"
            ),
        ))),
        Err((_, wire::Unsendable::TooLarge)) => Err(refused(ProtocolError::new(
            ProtocolCode::ResponseTooLarge,
            format!(
                "the Script's result and its modifications encode to more than the maximum frame \
                 of {MAX_FRAME_LEN} bytes"
            ),
        ))),
        Err((_, wire::Unsendable::Unrepresentable)) => Err(with_findings(
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

/// The two requests that run a Script with starting variables, whose payloads share one decoder.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Request {
    /// `ExecutionStart`: `{source, variables, overrides?}`.
    DirectExecution,
    /// `CachedExecutionStart`: `{handle, variables, overrides?}` (Story 4.2).
    CachedExecution,
}

impl Request {
    /// The message type's name, as the payload's refusals spell it.
    const fn name(self) -> &'static str {
        match self {
            Self::DirectExecution => "ExecutionStart",
            Self::CachedExecution => "CachedExecutionStart",
        }
    }

    /// The key naming what to run: the source, or the registered Script's handle.
    const fn first(self) -> &'static str {
        match self {
            Self::DirectExecution => SOURCE,
            Self::CachedExecution => HANDLE,
        }
    }
}

/// A decoded `ExecutionStart` or `CachedExecutionStart`: the source or the handle text, the
/// starting variables in the order given, and the overrides (none set when the payload has none).
type Decoded<'a> = (&'a str, Vec<(Arc<str>, Held)>, Settings);

/// Decode an `ExecutionStart` or `CachedExecutionStart` payload by hand, like `Init`'s: a Backend
/// is owed the exact key, name or path that was wrong.
fn decode(payload: &Value, request: Request) -> Result<Decoded<'_>, String> {
    let (name, first_key) = (request.name(), request.first());
    let fields: &[(Value, Value)] = match payload {
        Value::Nil => &[],
        Value::Map(fields) => fields,
        _ => {
            return Err(format!(
                "the `{name}` payload must be a map with `{first_key}` and `{VARIABLES}`"
            ));
        }
    };

    let mut first = None;
    let mut variables = None;
    let mut overrides = None;
    for (key, value) in fields {
        let slot = match key.as_str() {
            Some(key) if key == first_key => &mut first,
            Some(VARIABLES) => &mut variables,
            Some(OVERRIDES) => &mut overrides,
            Some(other) => {
                return Err(format!(
                    "the `{name}` payload has an unknown key `{}`",
                    wire::bounded(other)
                ));
            }
            None => {
                return Err(format!(
                    "the `{name}` payload has a key that is not a string"
                ));
            }
        };
        if slot.is_some() {
            return Err(format!(
                "the `{name}` payload repeats the key `{}`",
                key.as_str().unwrap_or_default()
            ));
        }
        *slot = Some(value);
    }

    // Absent or nil is missing; both missing keys are named together. Empty is not missing.
    let (first, variables) = match (present(first), present(variables)) {
        (Some(first), Some(variables)) => (first, variables),
        (None, Some(_)) => return Err(missing(name, &[first_key])),
        (Some(_), None) => return Err(missing(name, &[VARIABLES])),
        (None, None) => return Err(missing(name, &[first_key, VARIABLES])),
    };

    let Some(first) = first.as_str() else {
        return Err(format!("`{first_key}` is not a string"));
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
    // The static check runs at registration and never again (AD-8): asking for one here would be
    // silently ignored, which would mislead the Backend, so it is refused.
    if request == Request::CachedExecution && overrides.check_setting().is_some() {
        return Err(format!(
            "`{OVERRIDES}.check` cannot be set on a `{name}`: the static check runs once, at \
             registration (`CodeRegister`), under the Session's check mode"
        ));
    }

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
    Ok((first, bound, overrides))
}

/// A key that is absent or nil is missing.
fn present(value: Option<&Value>) -> Option<&Value> {
    value.filter(|v| !v.is_nil())
}

fn missing(request: &str, keys: &[&str]) -> String {
    let names: Vec<String> = keys.iter().map(|k| format!("`{k}`")).collect();
    format!("the `{request}` payload is missing {}", names.join(" and "))
}
