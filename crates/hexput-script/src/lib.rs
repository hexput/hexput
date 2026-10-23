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
//! never issued, another Session's, or no longer cached (evicted, unregistered or released with
//! its Session, Story 4.3) — is `protocol.unknown_script_handle`,
//! telling the Backend to register again; the payload is decoded fully first, so a malformed
//! one is `protocol.invalid_payload` whatever its handle.
//!
//! The cache is bounded and cleaned up (Story 4.3). It holds at most System Config's
//! `ast_cache_capacity` entries; which one goes over it follows moka's policy, and every such
//! eviction is counted ([`AstCache::evictions`], for Epic 7's metrics) and logged at `debug`.
//! [`code_unregister`] serves one `CodeUnregister {handle}`, removing the entry at once and
//! answering `{}`. A Session's entries are released when it is torn down: the Daemon wires the
//! Session registry's teardown hook to [`AstCache::close`], and the cache refuses an insert for a
//! Session that is not open, so a registration still in flight when its Session ends answers
//! `protocol.init_not_completed` and leaves nothing behind. Every removal releases the program
//! at once, and a run that already fetched it finishes normally. An evicted, unregistered or
//! released handle is `protocol.unknown_script_handle`, exactly like one never issued; nothing is
//! ever re-parsed.
//!
//! Binds: AD-3, AD-6, AD-8.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, PoisonError, RwLock};

use hexput_check::{Environment, Policy};
/// A Session's registration as Direct Execution takes it — a Registered Function, or a
/// Registered Method under an object key (Story 3.12) — re-exported so the connection, which
/// reads the Session's registrations, can hand them over without an edge to the Executor.
pub use hexput_exec::Registration;
use hexput_exec::wire;
use hexput_exec::{Caller, Finished, Host, Limits};
use hexput_interpreter::{Category, Code, Diagnostic, Held, Program, Span};
use hexput_port::findings::{self, Rejection, encoded_len, fitted, push_findings, with_findings};
use hexput_port::{
    CheckMode, ClientId, ErrorBody, MAX_FRAME_LEN, ProtocolCode, ProtocolError, ScriptHandle,
    Settings, Value, decode_settings,
};

pub use hexput_exec::wire::{MAX_MODIFICATION_DEPTH, MAX_RESULT_DEPTH};

const SOURCE: &str = "source";
const VARIABLES: &str = "variables";
const OVERRIDES: &str = "overrides";
const VALUE: &str = "value";
const HANDLE: &str = "handle";

/// The most static check findings one reply carries, in source order (Story 3.10). A Script with
/// more has the rest dropped — except that a rejection's first error is always kept, in place of
/// the last one, so the rejection's own body is always among its `findings`. Defined, with the
/// rest of how findings travel, in `hexput_port::findings` (Story 6.3), which every submission
/// path shares.
pub use hexput_port::MAX_FINDINGS;

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
/// The handle's entry is fetched when this function is *called*, before the returned future is
/// first polled: the connection calls it as it dispatches the request, so a `CodeUnregister` (or
/// an eviction or teardown) processed after the dispatch cannot take the program from a run sent
/// before it (Story 4.3). The payload itself is still decoded on the blocking pool, and its
/// errors come first: a malformed payload is answered as one whatever its handle names.
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
///   runs, and whatever entry the handle names is not used.
/// * `protocol.unknown_script_handle` — a well-formed handle with no entry under `client_id`
///   when the call was made: never issued, registered by another Session (the same message, so
///   nothing reveals that it exists), or no longer cached — evicted over the cache's capacity,
///   removed by a `CodeUnregister`, or released with its Session. The message names the handle
///   and says to register the Script again with `CodeRegister`. Nothing runs.
/// * The interpreter's or Executor's [`Diagnostic`], exactly as for [`direct_execution`]:
///   `syntax.duplicate_declaration` for a starting variable the Script also declares, every
///   `reference`, `type`, `capability`, `host`, `budget` and `policy` error.
/// * `protocol.result_too_deep` or `protocol.response_too_large`, exactly as for
///   [`direct_execution`].
///
/// # Panics
///
/// If the work on the blocking pool panics, the panic continues here.
pub fn cached_execution(
    payload: Value,
    client_id: ClientId,
    cache: &AstCache,
    registrations: Vec<Registration>,
    settings: Settings,
    caller: Caller,
) -> impl Future<Output = Result<Value, Box<ErrorBody>>> + Send + 'static {
    // Fetched now, as the request is dispatched — a scan for the key, no decoding — so a removal
    // processed after the dispatch leaves this run its program.
    let fetched = handle_named(&payload).and_then(|handle| cache.get(client_id, handle));
    async move {
        let (program, variables, effective) = blocking(move || {
            let (handle, variables, overrides) =
                decode(&payload, Request::CachedExecution).map_err(invalid_payload)?;
            let handle = parse_handle(handle)?;
            // Only now, with the whole payload accepted, is the fetched program used: a malformed
            // request is answered as one whatever its handle names. An accepted payload has one
            // `handle`, so it is the one fetched.
            let program = fetched.ok_or_else(|| unknown_handle(handle))?;
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
}

/// The well-formed handle under the first `handle` key of `payload`, if there is one. Only a scan:
/// the payload is decoded, and its errors decided, on the blocking pool.
fn handle_named(payload: &Value) -> Option<ScriptHandle> {
    payload
        .as_map()?
        .iter()
        .find(|(key, _)| key.as_str() == Some(HANDLE))?
        .1
        .as_str()?
        .parse()
        .ok()
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
/// counter, starting at 1, so no handle is ever reused during the run.
///
/// Story 4.3 bounds and cleans it up:
///
/// * It holds at most its capacity of entries — a count, not bytes (each source is already
///   bounded by a frame). Which entry goes follows moka's policy, which may also refuse to admit
///   the newest entry, so a handle just issued can be unknown at once; every such removal is
///   counted by [`evictions`](Self::evictions) and logged at `debug`, naming the evicted entry's
///   Client ID. The capacity is one pool shared by every Session, with no per-Session share: a
///   Backend that registers heavily can displace another Backend's entries, which then register
///   again. A per-Session quota is deferred.
/// * [`remove`](Self::remove) takes one entry out at once (`CodeUnregister`).
/// * [`close`](Self::close), called through the Session registry's teardown hook, releases every
///   entry of a Session that has ended. A Session is [`open`](Self::open)ed at init, and an
///   insert for a Client ID that is not open is refused: the insert holds the open set's read
///   lock while it inserts and `close` takes its write lock before releasing anything, so a
///   registration still in flight when its Session ends can never leave an entry behind.
///
/// Every removal runs moka's pending maintenance before returning, so the cache keeps no
/// reference to a removed program; a run that already fetched one holds its own and finishes.
/// No lock here is ever held across an `.await` or while calling code outside this type: the
/// open set's read lock covers only an insert's check and moka's own insert, never maintenance,
/// which calls the eviction listener.
pub struct AstCache {
    entries: moka::sync::Cache<(ClientId, ScriptHandle), Arc<Program>>,
    /// The next handle's value.
    next: AtomicU64,
    /// The Client IDs whose Sessions are live: the only ones an insert is accepted for.
    open: RwLock<HashSet<ClientId>>,
    /// How many entries the capacity removed, shared with the eviction listener.
    evictions: Arc<AtomicU64>,
    /// The most entries the cache holds.
    capacity: u64,
}

impl AstCache {
    /// An empty cache holding at most `capacity` entries, whose first handle is
    /// `0000000000000001`. The Daemon passes System Config's `ast_cache_capacity`.
    #[must_use]
    pub fn with_capacity(capacity: u64) -> Self {
        let evictions = Arc::new(AtomicU64::new(0));
        let counted = Arc::clone(&evictions);
        let entries = moka::sync::Cache::builder()
            .max_capacity(capacity)
            .eviction_listener(move |key: Arc<(ClientId, ScriptHandle)>, _program, cause| {
                // Only the capacity's removals are evictions: an unregister or a teardown
                // (`Explicit`) and a replacement never happen to a Backend unasked.
                if cause == moka::notification::RemovalCause::Size {
                    counted.fetch_add(1, Ordering::Relaxed);
                    // A root event naming the evicted entry's own Session: maintenance runs on
                    // whichever request's thread happens to trigger it — often another
                    // Session's — whose span must not claim the eviction (FR-12).
                    tracing::debug!(
                        parent: None,
                        client_id = %key.0,
                        handle = %key.1,
                        "evicted a cached Script over the cache's capacity"
                    );
                }
            })
            .build();
        Self {
            entries,
            next: AtomicU64::new(1),
            open: RwLock::new(HashSet::new()),
            evictions,
            capacity,
        }
    }

    /// The most entries this cache holds.
    #[must_use]
    pub const fn capacity(&self) -> u64 {
        self.capacity
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

    /// How many entries the capacity has removed during this run, a refused admission included —
    /// never an unregister or a teardown. Pending maintenance is run first, so the count is exact.
    /// Epic 7's metrics publish it.
    #[must_use]
    pub fn evictions(&self) -> u64 {
        self.entries.run_pending_tasks();
        self.evictions.load(Ordering::Relaxed)
    }

    /// Accept registrations for `client_id`, whose Session has just been created. Opening one
    /// already open changes nothing.
    pub fn open(&self, client_id: ClientId) {
        self.open_set_mut().insert(client_id);
    }

    /// Release every entry of `client_id`, whose Session has ended, and refuse any later insert
    /// for it — including one already in flight. Walks the whole cache once: linear in its size,
    /// run once per Session teardown. Closing a Client ID that is not open still releases its
    /// entries.
    pub fn close(&self, client_id: ClientId) {
        // Taken before anything is released: an insert for `client_id` holding the read lock
        // finishes first and its entry is released below; any later one sees the ID closed.
        self.open_set_mut().remove(&client_id);
        self.entries.run_pending_tasks();
        let ended: Vec<(ClientId, ScriptHandle)> = self
            .entries
            .iter()
            .filter(|(key, _)| key.0 == client_id)
            .map(|(key, _)| *key)
            .collect();
        for key in &ended {
            self.entries.invalidate(key);
        }
        self.entries.run_pending_tasks();
    }

    /// Remove the entry `client_id` registered under `handle`, at once. Returns whether there
    /// was one; another Session's entry is never touched.
    pub fn remove(&self, client_id: ClientId, handle: ScriptHandle) -> bool {
        let removed = self.entries.remove(&(client_id, handle)).is_some();
        self.entries.run_pending_tasks();
        removed
    }

    /// Store `program` for `client_id` under a handle never issued before, and return it — or
    /// `None`, storing nothing, when `client_id`'s Session is not open. Pending maintenance runs
    /// before it returns, so the capacity holds as soon as the registration is answered.
    fn insert(&self, client_id: ClientId, program: Arc<Program>) -> Option<ScriptHandle> {
        // Held across the insert itself, so a `close` is ordered strictly before or after it.
        let open = self.open.read().unwrap_or_else(PoisonError::into_inner);
        if !open.contains(&client_id) {
            return None;
        }
        let handle = ScriptHandle::new(self.next.fetch_add(1, Ordering::Relaxed));
        self.entries.insert((client_id, handle), program);
        // Released before maintenance, which calls the eviction listener and so the installed
        // subscriber: the entry is already visible to a `close` walking the cache.
        drop(open);
        self.entries.run_pending_tasks();
        Some(handle)
    }

    /// The open set for writing. A poisoned lock is still a consistent set: no code here panics
    /// halfway through changing it.
    fn open_set_mut(&self) -> std::sync::RwLockWriteGuard<'_, HashSet<ClientId>> {
        self.open.write().unwrap_or_else(PoisonError::into_inner)
    }
}

impl core::fmt::Debug for AstCache {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("AstCache")
            .field("capacity", &self.capacity)
            // Without running pending maintenance: unlike `entry_count`, an approximation.
            .field("approximate_entries", &self.entries.entry_count())
            .field("evictions", &self.evictions.load(Ordering::Relaxed))
            .field("next", &self.next)
            .finish_non_exhaustive()
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
/// * `protocol.init_not_completed` — `client_id`'s Session is not open in `cache`: it has ended
///   (or was never opened) before the entry could be stored, so nothing was registered (Story
///   4.3).
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
        let findings = match static_check(&program, starting, callables, Vec::new(), &settings) {
            Ok(findings) => findings,
            Err(rejection) => return Ok(Err(rejection)),
        };
        // Inside this closure, so even a connection aborted meanwhile cannot leave an entry
        // behind: the cache refuses it once the Session has ended (Story 4.3).
        let handle = cache
            .insert(client_id, Arc::new(program))
            .ok_or_else(session_ended)?;
        Ok::<_, Box<ErrorBody>>(Ok((handle, findings)))
    })
    .await?;
    let (handle, findings) = registered.map_err(rejected)?;
    let mut fields = vec![(Value::from(HANDLE), Value::from(handle.to_string()))];
    let size = encoded_len(&Value::Map(fields.clone()));
    push_findings(&mut fields, size, &findings);
    Ok(Value::Map(fields))
}

/// The body answering a registration whose Session ended before its entry could be stored (Story
/// 4.3): the cache no longer accepts entries for its Client ID.
fn session_ended() -> Box<ErrorBody> {
    Box::new(ErrorBody::from(&ProtocolError::new(
        ProtocolCode::InitNotCompleted,
        "the Session has ended; nothing was registered".to_owned(),
    )))
}

/// Serve one unregister (Story 4.3): `payload` is a `CodeUnregister` payload, exactly `{handle}`
/// — the 16-lowercase-hex-character handle a `CodeRegister` was answered with. The entry
/// `client_id` registered under it is removed from `cache` at once, and the cache keeps no
/// reference to its program; a run that already fetched the program holds its own and finishes
/// normally. The next `CachedExecutionStart` of the handle is `protocol.unknown_script_handle`.
///
/// Returns the `Result` payload `{}`, an empty map. Synchronous and cheap — no parse, no Script —
/// so the connection serves it inline.
///
/// # Errors
///
/// The `Error` payload, each exactly one of, and in every case nothing is removed:
///
/// * `protocol.invalid_payload` — the payload is not a map, `handle` is missing (absent or nil),
///   not a string or not a Script handle (echoed bounded), or a key is unknown, repeated or not a
///   string. The message names the key. The handle is not looked up.
/// * `protocol.unknown_script_handle` — a well-formed handle with no entry under `client_id`:
///   never issued, another Session's (left untouched, and the same message), evicted, or already
///   removed — so a repeated unregister tells the Backend the entry was already gone.
pub fn code_unregister(
    payload: &Value,
    client_id: ClientId,
    cache: &AstCache,
) -> Result<Value, Box<ErrorBody>> {
    let handle = decode_unregister(payload).map_err(invalid_payload)?;
    if cache.remove(client_id, handle) {
        Ok(Value::Map(Vec::new()))
    } else {
        Err(unknown_handle(handle))
    }
}

/// Decode a `CodeUnregister` payload by hand: exactly `{handle}`.
fn decode_unregister(payload: &Value) -> Result<ScriptHandle, String> {
    const NAME: &str = "CodeUnregister";
    let fields: &[(Value, Value)] = match payload {
        Value::Nil => &[],
        Value::Map(fields) => fields,
        _ => {
            return Err(format!(
                "the `{NAME}` payload must be a map with `{HANDLE}`"
            ));
        }
    };
    let mut handle = None;
    for (key, value) in fields {
        match key.as_str() {
            Some(HANDLE) if handle.is_some() => {
                return Err(format!("the `{NAME}` payload repeats the key `{HANDLE}`"));
            }
            Some(HANDLE) => handle = Some(value),
            Some(other) => {
                return Err(format!(
                    "the `{NAME}` payload has an unknown key `{}`",
                    wire::bounded(other)
                ));
            }
            None => {
                return Err(format!(
                    "the `{NAME}` payload has a key that is not a string"
                ));
            }
        }
    }
    let Some(handle) = present(handle) else {
        return Err(missing(NAME, &[HANDLE]));
    };
    let Some(handle) = handle.as_str() else {
        return Err(format!("`{HANDLE}` is not a string"));
    };
    parse_handle(handle).map_err(|body| body.message)
}

/// `text` as a [`ScriptHandle`], or the `protocol.invalid_payload` body naming `handle` and echoing
/// it bounded.
fn parse_handle(text: &str) -> Result<ScriptHandle, Box<ErrorBody>> {
    text.parse().map_err(|_| {
        invalid_payload(format!(
            "`{HANDLE}` is not a Script handle ({} lowercase hex characters); found {:?}",
            ScriptHandle::TEXT_LEN,
            wire::bounded(text)
        ))
    })
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
    findings::shape(findings.diagnostics(), mode == CheckMode::Error)
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
