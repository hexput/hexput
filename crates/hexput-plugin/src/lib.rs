//! Plugin actor: registration, event routing, priority/async dispatch. Owns event routing
//! and ordering metadata only — it does not itself execute the ordered chain; a per-
//! (Plugin, Event) sequencing task, spawned independently of the actor's mailbox, does that.
//! Invokes hexput-check once, at Plugin registration.
//!
//! # What Story 6.2 adds
//!
//! Registration. A Backend registers a Plugin with a `PluginRegister` request — its message
//! type alone marks it as one, so nothing is inferred from the payload's shape — whose payload is
//! exactly `{source}`. Registration decodes it, parses the source with
//! `hexput_parser::parse_plugin`, reserves the Plugin's metadata `name` in its Session through a
//! caller-supplied closure (the Session owns the set of its Plugins' names, each under a
//! [`PluginId`], and a name is unique per Client ID), stores the parsed [`Plugin`] in the per-run
//! [`Plugins`] registry and answers `{name}`.
//!
//! [`Plugins`] holds the parsed Plugins keyed by Client ID and then [`PluginId`], with the same
//! guard as the AST Cache: [`Plugins::open`] runs when a Session is created, the Daemon's Session
//! teardown hook calls [`Plugins::close`], and an insert for a Client ID that is not open is
//! refused — so a registration still in flight when its Session ends leaves nothing behind.
//!
//! "Independent state" in this story means each Plugin is its own [`Plugin`] value under its own
//! id, with its own parsed program and Global Variable declarations. Init handlers, firing, the
//! Global Variable store and unregister land with Stories 6.4–6.14.
//!
//! # What Story 6.3 adds
//!
//! Event declarations (FR-19, FR-25). The payload is exactly `{source, events}`: `events` is
//! required (`{}` declares no custom Event) and maps each Event name — a §2 identifier, given
//! once, never the reserved `BackendRegisteredInit` — to `{returns, keyed?}`. `returns` is a
//! return shape written as a Hexput literal used as a template and parsed by
//! `hexput_parser::parse_shape` — there is no second schema language — into a flat
//! [`Shape`](hexput_parser::Shape) no deeper than a result may be; `keyed` says whether a fire
//! carries a partition key. Every refusal is `protocol.invalid_payload` naming the full path,
//! and the payload is decoded in full before the source is parsed. A [`Plugin`] holds its
//! [`EventDeclaration`]s in payload order ([`Plugin::events`]); [`Plugin::event`] resolves a
//! name for a fire and refuses an undeclared or the reserved one as `protocol.undeclared_event`.
//!
//! The static check at Plugin registration (AD-8, FR-26) runs here, once, after parsing and
//! before the name is reserved, when the Session's live check mode is not `off`: the same pass as
//! a Script's through `hexput_check::check_plugin`, with the Session's Registered Function names
//! as the callable list, the declared Event names and the policy the feature toggles describe.
//! It adds the Plugin-only warning `reference.undeclared_event` and never reports a Global
//! Variable or a handler unused. Its findings are shaped, bounded and fitted by
//! `hexput_port::findings`, the rule every submission path shares; a rejection under `error`
//! reserves nothing ([`Refusal::Rejected`] tells it apart so the connection can log it).
//!
//! # What Story 6.4 adds
//!
//! Initialization (FR-18): a Plugin is ready only once every `@Event(BackendRegisteredInit)`
//! handler has run to completion. Registration is three phases. [`prepare`] — synchronous, run
//! on the blocking pool — decodes, parses, checks and reserves the name, making a [`Prepared`]
//! registration that no registry holds. [`initialize`] runs each init handler exactly once, in
//! source order, one after another, through the one handler-invocation function
//! `hexput_exec::invoke_handler` (AD-3) under the Session's live settings and registrations, with
//! `{}` as `params`, discarding each result; the first failure stops the rest and is the
//! registration's error. [`finish`] — synchronous again — stores the Plugin in [`Plugins`], which
//! is what makes it ready: until then [`Plugins::get`] and [`Plugins::ids`] do not return it, so
//! no Event can run ahead of its initialization. A failed initialization stores nothing, and the
//! caller releases the reserved name (`hexput_session::Sessions::release_plugin`). A [`Plugin`]
//! keeps its program shared ([`Plugin::executable`]) for Story 6.5's fires.
//!
//! `BackendRegisteredInit` stays reserved: declaring it in `events` is refused and
//! [`Plugin::event`] refuses it as a fire target.
//!
//! This crate has no async runtime of its own: [`prepare`] and [`finish`] are synchronous, and
//! the connection runs them on the blocking pool; [`initialize`] is `async` and needs the Tokio
//! runtime the Executor runs on. No lock here is held while calling code outside this crate's own
//! types, nor across an `.await`.
//!
//! Binds: AD-3, AD-4, AD-6, AD-8.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, PoisonError, RwLock};

use hexput_check::{Environment, Policy};
use hexput_exec::wire::{MAX_RESULT_DEPTH, bounded};
use hexput_exec::{
    Caller, Code, Held, Host, Limits, Object, Program, Registration, Value as ExecValue,
};
use hexput_parser::{BACKEND_REGISTERED_INIT, PluginProgram, Shape};
use hexput_port::findings::{self, Rejection};
use hexput_port::{
    CheckMode, ClientId, ErrorBody, PluginId, ProtocolCode, ProtocolError, Settings, Value,
};

/// The `PluginRegister` payload's source key.
const SOURCE: &str = "source";
/// The `PluginRegister` payload's Event declarations key (Story 6.3).
const EVENTS: &str = "events";
/// An Event declaration's return shape key.
const RETURNS: &str = "returns";
/// An Event declaration's partition-key flag.
const KEYED: &str = "keyed";
/// The success reply's one key (beside `findings`).
const NAME: &str = "name";
/// The request's wire name, for messages.
const REQUEST: &str = "PluginRegister";

/// One Event a Backend declared at Plugin registration (Story 6.3, FR-19, FR-25): its name, the
/// shape every handler's result must have, and whether a fire must carry a partition key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EventDeclaration {
    /// The Event's name: a §2 identifier, never `BackendRegisteredInit`.
    pub name: String,
    /// The return shape, parsed from the Hexput literal the Backend wrote (Story 6.6 holds results
    /// to it).
    pub returns: Shape,
    /// Whether a fire of this Event carries a partition key, for `keyed` Global Variables
    /// (Story 6.10). `false` when the declaration left it out.
    pub keyed: bool,
}

/// One registered Plugin: its id, its parsed source and its declared Events.
#[derive(Debug)]
pub struct Plugin {
    id: PluginId,
    program: PluginProgram,
    /// The program its handlers run against (Story 6.4), shared by every invocation.
    executable: Arc<Program>,
    events: Vec<EventDeclaration>,
    /// Each declared Event's index in `events`, by name.
    index: HashMap<String, usize>,
}

impl Plugin {
    /// The id the Session reserved its name under.
    #[must_use]
    pub const fn id(&self) -> PluginId {
        self.id
    }

    /// Its metadata `name`, unique in its Session.
    #[must_use]
    pub fn name(&self) -> &str {
        self.program.metadata.name()
    }

    /// Its parsed source: the program, the metadata, the Global Variable declarations and the
    /// Event handlers.
    #[must_use]
    pub const fn program(&self) -> &PluginProgram {
        &self.program
    }

    /// The program its handlers run against, as [`hexput_exec::invoke_handler`] takes it: shared,
    /// so any number of invocations run against one copy (Story 6.4).
    #[must_use]
    pub const fn executable(&self) -> &Arc<Program> {
        &self.executable
    }

    /// Every Event the Backend declared at registration, in payload order. Empty when it declared
    /// none (`events: {}`), so only `BackendRegisteredInit` handlers can ever run.
    #[must_use]
    pub fn events(&self) -> &[EventDeclaration] {
        &self.events
    }

    /// The declaration of the Event `name`, as a fire resolves it (Story 6.3; Story 6.5 wires the
    /// fire to it).
    ///
    /// # Errors
    ///
    /// `protocol.undeclared_event` when the registration did not declare `name`, naming it
    /// (bounded) — or, for `BackendRegisteredInit`, saying that it is reserved and that a Backend
    /// cannot fire it.
    pub fn event(&self, name: &str) -> Result<&EventDeclaration, Box<ErrorBody>> {
        if name == BACKEND_REGISTERED_INIT {
            return Err(protocol(
                ProtocolCode::UndeclaredEvent,
                format!("`{BACKEND_REGISTERED_INIT}` is reserved; a Backend cannot fire it"),
            ));
        }
        self.index
            .get(name)
            .and_then(|&at| self.events.get(at))
            .ok_or_else(|| {
                protocol(
                    ProtocolCode::UndeclaredEvent,
                    format!(
                        "`{}` is not an Event the Plugin `{}` declared; nothing was fired",
                        bounded(name),
                        bounded(self.name())
                    ),
                )
            })
    }
}

/// The registered Plugins of every live Session, keyed by Client ID and then [`PluginId`]. One
/// per Daemon run, shared by every Connection.
///
/// A Client ID present in the map is *open*: its Session is live and accepts registrations.
/// Every operation takes the lock synchronously and releases it before returning.
#[derive(Debug, Default)]
pub struct Plugins {
    clients: RwLock<HashMap<ClientId, HashMap<PluginId, Arc<Plugin>>>>,
}

impl Plugins {
    /// An empty registry, with no Client ID open.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Accept registrations for `client_id`, whose Session has just been created. Opening one
    /// already open changes nothing.
    pub fn open(&self, client_id: ClientId) {
        self.write().entry(client_id).or_default();
    }

    /// Release every Plugin of `client_id`, whose Session has ended, and refuse any later insert
    /// for it — including one already in flight. The Plugins are dropped after the lock is
    /// released. Closing a Client ID that is not open changes nothing.
    pub fn close(&self, client_id: ClientId) {
        let released = self.write().remove(&client_id);
        drop(released);
    }

    /// The Plugin `client_id` holds under `id`, if it holds one.
    #[must_use]
    pub fn get(&self, client_id: ClientId, id: PluginId) -> Option<Arc<Plugin>> {
        self.read()
            .get(&client_id)
            .and_then(|plugins| plugins.get(&id))
            .map(Arc::clone)
    }

    /// The ids of every Plugin `client_id` holds, sorted; empty when it holds none or is not
    /// open.
    #[must_use]
    pub fn ids(&self, client_id: ClientId) -> Vec<PluginId> {
        let mut ids: Vec<PluginId> = self
            .read()
            .get(&client_id)
            .map(|plugins| plugins.keys().copied().collect())
            .unwrap_or_default();
        ids.sort_unstable();
        ids
    }

    /// How many Plugins are held, across every Client ID.
    #[must_use]
    pub fn len(&self) -> usize {
        self.read().values().map(HashMap::len).sum()
    }

    /// Whether no Plugin is held.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Store `plugin` for `client_id`, returning whether it was stored: `false`, storing
    /// nothing, when `client_id` is not open. The plugin is dropped outside the lock either way.
    fn insert(&self, client_id: ClientId, plugin: Plugin) -> bool {
        let plugin = Arc::new(plugin);
        let mut clients = self.write();
        match clients.get_mut(&client_id) {
            Some(plugins) => {
                plugins.insert(plugin.id, plugin);
                true
            }
            None => false,
        }
    }

    /// The map for reading. A poisoned lock is still a consistent map: no code here panics
    /// halfway through changing it.
    fn read(
        &self,
    ) -> std::sync::RwLockReadGuard<'_, HashMap<ClientId, HashMap<PluginId, Arc<Plugin>>>> {
        self.clients.read().unwrap_or_else(PoisonError::into_inner)
    }

    /// The map for writing; see [`read`](Self::read).
    fn write(
        &self,
    ) -> std::sync::RwLockWriteGuard<'_, HashMap<ClientId, HashMap<PluginId, Arc<Plugin>>>> {
        self.clients.write().unwrap_or_else(PoisonError::into_inner)
    }
}

/// What reserving a Plugin's name in its Session came to, as the `reserve` closure of
/// [`prepare`] reports it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reservation {
    /// The name was free in the Session and is now reserved under this id.
    Reserved(PluginId),
    /// The Session already holds a Plugin with this name.
    NameTaken,
    /// The Session no longer exists.
    SessionEnded,
}

/// Why [`prepare`] or [`finish`] registered nothing: a refusal, or a rejection by the static check that
/// the caller logs before sending its body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refusal {
    /// Refused before or apart from the static check; the body is ready to send.
    Refused(Box<ErrorBody>),
    /// Rejected by the static check under check mode `error` (Story 6.3): its body, not yet
    /// fitted to a frame, and how many findings the pass made.
    Rejected(Rejection),
}

impl Refusal {
    /// The `Error` payload to send: a rejection's body fitted to a frame (its `findings` dropped
    /// when they would not fit).
    #[must_use]
    pub fn into_body(self) -> Box<ErrorBody> {
        match self {
            Self::Refused(body) => body,
            Self::Rejected(rejection) => findings::fitted(rejection.body),
        }
    }
}

impl From<Box<ErrorBody>> for Refusal {
    fn from(body: Box<ErrorBody>) -> Self {
        Self::Refused(body)
    }
}

/// A Plugin registration that has passed every check and reserved its name, but is not ready
/// yet (Story 6.4): what [`prepare`] makes, [`initialize`] runs the `BackendRegisteredInit`
/// handlers of, and [`finish`] stores. Until it is finished it is in no [`Plugins`] registry, so
/// nothing can look it up — no Event can run ahead of its initialization. Dropping it stores
/// nothing; the reserved name is the caller's to release.
#[derive(Debug)]
pub struct Prepared {
    id: PluginId,
    program: PluginProgram,
    /// The program the handlers run against, shared by every invocation.
    executable: Arc<Program>,
    events: Vec<EventDeclaration>,
    /// The static check's findings, as the success reply carries them.
    found: Vec<ErrorBody>,
}

impl Prepared {
    /// The id the Session reserved its name under.
    #[must_use]
    pub const fn id(&self) -> PluginId {
        self.id
    }

    /// Its metadata `name`, reserved in its Session.
    #[must_use]
    pub fn name(&self) -> &str {
        self.program.metadata.name()
    }

    /// The function names of its `BackendRegisteredInit` handlers, in source order: what
    /// [`initialize`] runs.
    pub fn init_handlers(&self) -> impl Iterator<Item = &str> {
        self.program
            .handlers
            .iter()
            .filter(|handler| handler.event.name == BACKEND_REGISTERED_INIT)
            .map(|handler| handler.function.name.as_str())
    }
}

/// The first phase of one Plugin registration (FR-17, FR-19, FR-25; Stories 6.2–6.4): decode
/// `payload` — a `PluginRegister` payload, exactly `{source, events}` — parse and check the
/// source, and reserve its metadata `name` through `reserve`. Synchronous, and as expensive as
/// the parse and the check: the connection runs it on the blocking pool.
///
/// `events` is a map from Event name to declaration: each name a §2 identifier given once and
/// never `BackendRegisteredInit`; each declaration a map with exactly `returns` (required, a
/// string holding a return shape — a Hexput literal used as a template, parsed by
/// [`hexput_parser::parse_shape`] and nested no deeper than a result may be,
/// [`MAX_RESULT_DEPTH`]) and optionally `keyed` (a bool; absent or nil is `false`). `events: {}`
/// declares no custom Event.
///
/// In order: the payload is decoded in full, shapes included; the source is parsed with
/// `hexput_parser::parse_plugin`; when the `check` mode in `settings` — the Session's Config as it
/// is now (AD-5) — is not `off`, the static check runs ([`hexput_check::check_plugin`]) with no
/// starting variables, `callables` (the Session's Registered Function names) as the callable
/// list, the declared Event names and the policy the feature toggles describe; and its metadata
/// `name` is handed to `reserve`, which reserves it in the Session (the connection wires it to
/// `hexput_session::Sessions::register_plugin`). Nothing is reserved for a payload, source or
/// check that refuses.
///
/// # Errors
///
/// Each exactly one of, and in every case nothing is reserved:
///
/// * `protocol.invalid_payload` — the payload is not a map (nil reads as missing), `source` or
///   `events` is missing (absent or nil) or mistyped, a key is unknown, repeated or not a string,
///   or an Event declaration is malformed. The message names the key or the full path
///   (`` `events.OrderPlaced.keyed` ``); a refused shape is
///   `` `events.X.returns` is not a return shape: <parser message> (line L, column C) ``.
/// * The parser's [`Diagnostic`](hexput_parser::Diagnostic) — `lexical.*` or `syntax.*`, with
///   its span.
/// * [`Refusal::Rejected`] — under check mode `error`, a source with an error-severity finding:
///   the first one's body with every finding (warnings too) under `findings`.
/// * `protocol.duplicate_plugin_name` — the Session already holds a Plugin with this name; that
///   Plugin is untouched.
/// * `protocol.init_not_completed` — the Session has ended: "the Session has ended; nothing was
///   registered".
pub fn prepare(
    payload: &Value,
    reserve: impl FnOnce(&str) -> Reservation,
    callables: &[String],
    settings: &Settings,
) -> Result<Prepared, Refusal> {
    let (source, events) =
        decode(payload).map_err(|message| protocol(ProtocolCode::InvalidPayload, message))?;
    let program = hexput_parser::parse_plugin(source).map_err(|d| Box::new(ErrorBody::from(&d)))?;
    let found = check(&program, &events, callables, settings).map_err(Refusal::Rejected)?;
    let id = match reserve(program.metadata.name()) {
        Reservation::Reserved(id) => id,
        Reservation::NameTaken => {
            return Err(protocol(
                ProtocolCode::DuplicatePluginName,
                format!(
                    "this Session already has a Plugin named `{}`; nothing was registered",
                    bounded(program.metadata.name())
                ),
            )
            .into());
        }
        Reservation::SessionEnded => return Err(session_ended().into()),
    };
    // One copy the handlers share (Story 6.5 invokes them from it, any number at once); the
    // parsed `PluginProgram` keeps its own for its side tables' indices.
    let executable = Arc::new(program.program.clone());
    Ok(Prepared {
        id,
        program,
        executable,
        events,
        found,
    })
}

/// The second phase (Story 6.4, FR-18): run every `BackendRegisteredInit` handler of `prepared`
/// exactly once, in source order, one after another — the next starts only once the previous one
/// finished — each through the one handler-invocation function,
/// [`hexput_exec::invoke_handler`] (AD-3), as its own execution with its own Resource Budget.
///
/// Each runs under `settings` — the Session's live Config at registration (AD-5) — as
/// [`Limits::from_settings`] computes the limits and feature toggles from it, and reaches the
/// host through the Session's `registrations` and `caller`, exactly as a Script would. Its
/// argument (`params`) is the empty object `{}`; its result and modifications are discarded —
/// once the execution produced them as any execution does, so a result with no wire form (a
/// function, a cycle) or past the output size budget still fails the handler. Every handler and
/// helper of a Plugin is a top-level named `fn`, so under a disabled `features.callbacks` every
/// handler is refused (`policy.construct_disabled`), as the static check reports at registration.
/// Until the Global Variable store lands (Story 6.7), every handler evaluates the Global Variable
/// initializers afresh and its writes end with it. A Plugin with no init handler makes no call.
/// Makes nothing ready: [`finish`] does. The settings are read when it is called; the future
/// borrows only `prepared`.
///
/// # Errors
///
/// The first failing handler's [`Diagnostic`](hexput_exec::Diagnostic), as an `ErrorBody`
/// unchanged — code, message and span; the handlers after it do not run. One exception: a
/// handler that failed with `host.no_reply` — its connection stopped reading before the Backend
/// answered its `Call` — is `protocol.init_not_completed` ("the connection stopped reading before
/// the Plugin's initialization completed; nothing was registered"), as a registration whose
/// Session ends before it completes is. An `Authorize` question left unanswered that way is a
/// denial, `capability.unknown_function`, exactly as for a Script (Story 3.3).
pub fn initialize<'p>(
    prepared: &'p Prepared,
    registrations: Vec<Registration>,
    caller: Caller,
    settings: &Settings,
) -> impl Future<Output = Result<(), Box<ErrorBody>>> + Send + 'p {
    // Read now, as the call is made: the future borrows nothing of `settings`.
    let limits = Limits::from_settings(settings);
    async move {
        for function in prepared.init_handlers() {
            let host = Host::new(registrations.clone(), caller.clone());
            let params = Held::plain(ExecValue::Object(Object::from_entries(Vec::<(
                &str,
                ExecValue,
            )>::new(
            ))));
            let invoked = hexput_exec::invoke_handler(
                Arc::clone(&prepared.executable),
                function,
                params,
                host,
                limits,
            )
            .await;
            if let Err(diagnostic) = invoked {
                if diagnostic.code == Code::NO_REPLY {
                    return Err(protocol(
                        ProtocolCode::InitNotCompleted,
                        "the connection stopped reading before the Plugin's initialization \
                         completed; nothing was registered",
                    ));
                }
                return Err(Box::new(ErrorBody::from(&diagnostic)));
            }
        }
        Ok(())
    }
}

/// The last phase (Story 6.4): store `prepared` in `plugins` under `client_id`, which makes it
/// ready — from here [`Plugins::get`] and [`Plugins::ids`] return it — and return the `Result`
/// payload `{name}`, the metadata `name` echoed, with `findings` after it when the static check
/// ran and found anything (at most `hexput_port::MAX_FINDINGS`, in source order, left out if they
/// would not fit a frame). Called only once [`initialize`] succeeded.
///
/// # Errors
///
/// `protocol.init_not_completed` when the Session has ended (`client_id` is not open in
/// `plugins`): "the Session has ended; nothing was registered". Nothing is stored.
pub fn finish(
    prepared: Prepared,
    client_id: ClientId,
    plugins: &Plugins,
) -> Result<Value, Refusal> {
    let Prepared {
        id,
        program,
        executable,
        events,
        found,
    } = prepared;
    let name = program.metadata.name().to_owned();
    let index = events
        .iter()
        .enumerate()
        .map(|(at, event)| (event.name.clone(), at))
        .collect();
    let plugin = Plugin {
        id,
        program,
        executable,
        events,
        index,
    };
    if !plugins.insert(client_id, plugin) {
        return Err(session_ended().into());
    }
    let mut fields = vec![(Value::from(NAME), Value::from(name))];
    let size = findings::encoded_len(&Value::Map(fields.clone()));
    findings::push_findings(&mut fields, size, &found);
    Ok(Value::Map(fields))
}

/// Run the static check the `check` mode in `settings` asks for (AD-8, FR-26) over `program`,
/// returning its findings as wire bodies — none under `off`, where no pass runs.
///
/// # Errors
///
/// Under `error`, a source with an error-severity finding: the [`Rejection`].
fn check(
    program: &PluginProgram,
    events: &[EventDeclaration],
    callables: &[String],
    settings: &Settings,
) -> Result<Vec<ErrorBody>, Rejection> {
    let mode = settings.check();
    if mode == CheckMode::Off {
        return Ok(Vec::new());
    }
    let environment = Environment::new()
        .with_callables(callables.iter().cloned())
        .with_events(events.iter().map(|event| event.name.clone()));
    let found = hexput_check::check_plugin(
        program,
        &environment,
        &Policy::from_features(settings.features()),
    );
    findings::shape(found.diagnostics(), mode == CheckMode::Error)
}

/// Decode a `PluginRegister` payload by hand: exactly `{source, events}`.
fn decode(payload: &Value) -> Result<(&str, Vec<EventDeclaration>), String> {
    let fields: &[(Value, Value)] = match payload {
        Value::Nil => &[],
        Value::Map(fields) => fields,
        _ => {
            return Err(format!(
                "the `{REQUEST}` payload must be a map with `{SOURCE}` and `{EVENTS}`"
            ));
        }
    };
    let mut source = None;
    let mut events = None;
    for (key, value) in fields {
        let slot = match key.as_str() {
            Some(SOURCE) => &mut source,
            Some(EVENTS) => &mut events,
            Some(other) => {
                return Err(format!(
                    "the `{REQUEST}` payload has an unknown key `{}`",
                    bounded(other)
                ));
            }
            None => {
                return Err(format!(
                    "the `{REQUEST}` payload has a key that is not a string"
                ));
            }
        };
        if slot.is_some() {
            return Err(format!(
                "the `{REQUEST}` payload repeats the key `{}`",
                key.as_str().unwrap_or_default()
            ));
        }
        *slot = Some(value);
    }
    let source = source.filter(|value| !value.is_nil());
    let events = events.filter(|value| !value.is_nil());
    // Both missing are named together; otherwise `source` is judged before `events`.
    let source = match (source, events) {
        (None, None) => {
            return Err(format!(
                "the `{REQUEST}` payload is missing `{SOURCE}` and `{EVENTS}`"
            ));
        }
        (None, Some(_)) => return Err(format!("the `{REQUEST}` payload is missing `{SOURCE}`")),
        (Some(Value::String(text)), _) => text
            .as_str()
            .ok_or_else(|| format!("the `{REQUEST}` payload's `{SOURCE}` is not valid UTF-8"))?,
        (Some(_), _) => {
            return Err(format!(
                "the `{REQUEST}` payload's `{SOURCE}` must be a string"
            ));
        }
    };
    let Some(events) = events else {
        return Err(format!("the `{REQUEST}` payload is missing `{EVENTS}`"));
    };
    Ok((source, decode_events(events)?))
}

/// Decode `events`: a map from Event name to declaration, in payload order.
fn decode_events(events: &Value) -> Result<Vec<EventDeclaration>, String> {
    let Value::Map(entries) = events else {
        return Err(format!(
            "`{EVENTS}` must be a map from Event name to declaration (`{{}}` declares none)"
        ));
    };
    let mut declared: Vec<EventDeclaration> = Vec::with_capacity(entries.len());
    let mut seen: HashSet<&str> = HashSet::with_capacity(entries.len());
    for (key, declaration) in entries {
        let Some(name) = key.as_str() else {
            return Err(format!(
                "`{EVENTS}` has a key that is not a string; an Event's name is a string"
            ));
        };
        if !hexput_parser::is_identifier(name) {
            return Err(format!(
                "`{EVENTS}.{}` is not a Hexput identifier (ASCII letters, digits and `_`, not \
                 starting with a digit, not a reserved word); an Event's name must be one",
                bounded(name)
            ));
        }
        if name == BACKEND_REGISTERED_INIT {
            return Err(format!(
                "`{EVENTS}.{BACKEND_REGISTERED_INIT}` is reserved; a Backend cannot declare it"
            ));
        }
        let path = format!("{EVENTS}.{}", bounded(name));
        if !seen.insert(name) {
            return Err(format!("`{path}` is declared twice"));
        }
        let (returns, keyed) = decode_declaration(declaration, &path)?;
        declared.push(EventDeclaration {
            name: name.to_owned(),
            returns,
            keyed,
        });
    }
    Ok(declared)
}

/// Decode one Event declaration at `path`: exactly `{returns, keyed?}`.
fn decode_declaration(declaration: &Value, path: &str) -> Result<(Shape, bool), String> {
    let Value::Map(fields) = declaration else {
        return Err(format!(
            "`{path}` must be a map with `{RETURNS}` and optionally `{KEYED}`"
        ));
    };
    let mut returns = None;
    let mut keyed = None;
    for (key, value) in fields {
        let slot = match key.as_str() {
            Some(RETURNS) => &mut returns,
            Some(KEYED) => &mut keyed,
            Some(other) => {
                return Err(format!(
                    "`{path}` has an unknown key `{}`; a declaration holds `{RETURNS}` and \
                     optionally `{KEYED}`",
                    bounded(other)
                ));
            }
            None => return Err(format!("`{path}` has a key that is not a string")),
        };
        if slot.is_some() {
            return Err(format!(
                "`{path}` repeats the key `{}`",
                key.as_str().unwrap_or_default()
            ));
        }
        *slot = Some(value);
    }
    let returns = match returns.filter(|value| !value.is_nil()) {
        None => return Err(format!("`{path}.{RETURNS}` is missing")),
        Some(Value::String(text)) => text
            .as_str()
            .ok_or_else(|| format!("`{path}.{RETURNS}` is not valid UTF-8"))?,
        Some(_) => {
            return Err(format!(
                "`{path}.{RETURNS}` must be a string holding a return shape"
            ));
        }
    };
    let keyed = match keyed {
        None | Some(Value::Nil) => false,
        Some(Value::Boolean(keyed)) => *keyed,
        Some(_) => return Err(format!("`{path}.{KEYED}` must be a boolean")),
    };
    Ok((shape(returns, path)?, keyed))
}

/// Parse `source` as the return shape at `path`, refusing one nested deeper than any result may
/// be.
fn shape(source: &str, path: &str) -> Result<Shape, String> {
    let refused = |reason: String| format!("`{path}.{RETURNS}` is not a return shape: {reason}");
    let shape = hexput_parser::parse_shape(source).map_err(|d| {
        refused(format!(
            "{} (line {}, column {})",
            d.message, d.span.line, d.span.column
        ))
    })?;
    let depth = shape.depth();
    if depth > MAX_RESULT_DEPTH {
        return Err(refused(format!(
            "it nests {depth} arrays and objects deep, past the {MAX_RESULT_DEPTH} a result may, \
             so no result could ever match it (line 1, column 1)"
        )));
    }
    Ok(shape)
}

/// A `protocol.*` body carrying `message`.
fn protocol(code: ProtocolCode, message: impl Into<String>) -> Box<ErrorBody> {
    Box::new(ErrorBody::from(&ProtocolError::new(code, message)))
}

/// The body answering a registration whose Session ended before its Plugin could be stored.
fn session_ended() -> Box<ErrorBody> {
    protocol(
        ProtocolCode::InitNotCompleted,
        "the Session has ended; nothing was registered",
    )
}
