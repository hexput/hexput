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
//! # What Story 6.5 adds
//!
//! Firing (FR-19). A Backend fires a declared Event of a ready Plugin with an `EventFire`
//! request, payload exactly `{plugin, event, params}`. Like registration, a fire is phases the
//! connection drives. [`fetch`] runs as the connection dispatches the request: it looks the
//! named Plugin up in [`Plugins`] ([`Plugins::named`]) without decoding anything, so a fire
//! takes effect in the order it was sent. [`prepare_fire`] — synchronous, run on the blocking
//! pool — decodes the payload in full (`params` converted like a starting variable, under the
//! path root `params`, and required to be a map), then refuses a Plugin that was not found
//! (`protocol.unknown_plugin`: never registered, another Session's, or still initializing —
//! readiness is presence in [`Plugins`]) and then an undeclared or reserved Event
//! ([`Plugin::event`]), making a [`Fire`]. [`run_fire`] runs every handler bound to the Event
//! ([`Plugin::handlers_for`]) once, in source order, one after another, each through
//! `hexput_exec::invoke_handler` (AD-3) as its own execution with its own Resource Budget and its
//! own copy of `params`, under the Session's live settings and registrations; a handler's
//! failure is its [`HandlerOutcome`] and never stops the next one. [`fire_reply`] builds the one
//! `Result {results}` payload, an entry per handler in the order they ran. `priority` and `async`
//! have no effect yet (Stories 6.11, 6.12).
//!
//! # What Story 6.7 adds
//!
//! Global Variables persist (FR-20). Every Plugin has one store, a `hexput-globalvar`
//! `PluginStore` ([`Plugin::globals`]): [`prepare`] makes it empty and unregistered,
//! [`initialize`] seeds it — every Global Variable initializer runs once, before the first init
//! handler, and never again — and [`finish`] registers it in [`Plugins::global_variables`] as it
//! stores the Plugin, so a registration that fails leaves no store behind. Every handler
//! invocation, init or fire, reads and writes the Globals through it, on any Connection of the
//! Session; only `hexput-session` tears a store down (AD-4).
//!
//! # What Story 6.6 adds
//!
//! Return shapes are enforced (FR-19). As [`fire_reply`] builds each successful entry, it holds
//! the handler's converted result to the fired Event's declared shape ([`Fire::returns`]) with
//! [`conform`]: a conforming result is sent exactly as before; one that does not conform makes
//! that entry `{handler, error}` with `protocol.shape_mismatch`, naming the Event and the first
//! offending path under `value` (`` `value.tags[1]` must be a string; found a number ``), and no
//! `modifications`. Each entry is checked on its own, so a mismatch never touches a sibling. The
//! check runs after conversion — a result an entry cannot carry keeps its own error — on the wire
//! value, seeing through Value Secret holders without altering them; it is charged to no
//! Resource Budget, and the handler's side effects stand. Init handlers' results are discarded,
//! never checked.
//!
//! Binds: AD-3, AD-4, AD-6, AD-8.

mod shape;

pub use shape::conform;

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, PoisonError, RwLock};

use hexput_check::{Environment, Policy};
use hexput_exec::wire::{
    self as exec_wire, MAX_RESULT_DEPTH, MODIFICATIONS_KEY, Path, Unsendable, bounded,
};
use hexput_exec::{
    Caller, Code, Diagnostic, Finished, GlobalStore, GlobalVariables, Held, Host, Limits, Object,
    Program, Registration, Value as ExecValue,
};
use hexput_parser::{BACKEND_REGISTERED_INIT, Category, PluginProgram, Shape, Span};
use hexput_port::findings::{self, Rejection};
use hexput_port::{
    CheckMode, ClientId, ErrorBody, MAX_FRAME_LEN, PluginId, ProtocolCode, ProtocolError, Settings,
    Value,
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
/// The `EventFire` request's wire name, for messages (Story 6.5).
const FIRE_REQUEST: &str = "EventFire";
/// The `EventFire` payload's Plugin name key.
const PLUGIN: &str = "plugin";
/// The `EventFire` payload's Event name key.
const EVENT: &str = "event";
/// The `EventFire` payload's parameters key.
const PARAMS: &str = "params";
/// The fire reply's one key.
const RESULTS: &str = "results";
/// A fire reply entry's handler name key.
const HANDLER: &str = "handler";
/// A successful fire reply entry's value key.
const VALUE: &str = "value";
/// A failed fire reply entry's error key.
const ERROR: &str = "error";
/// How many arrays or maps deep one handler's result, or its `modifications` array, may nest in
/// a fire reply: two levels fewer than a Script's result ([`MAX_RESULT_DEPTH`]), since the
/// `results` array and the entry's map sit between the payload and the value.
const MAX_ENTRY_DEPTH: usize = MAX_RESULT_DEPTH - 2;

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
    /// Its Global Variable store (Story 6.7), shared by every invocation; the Daemon's
    /// [`GlobalVariables`] registry holds it too, until its Session tears it down.
    globals: Arc<GlobalStore>,
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

    /// Its Global Variable store (Story 6.7): what every invocation of its handlers reads and
    /// writes its Global Variables through.
    #[must_use]
    pub const fn globals(&self) -> &Arc<GlobalStore> {
        &self.globals
    }

    /// Every Event the Backend declared at registration, in payload order. Empty when it declared
    /// none (`events: {}`), so only `BackendRegisteredInit` handlers can ever run.
    #[must_use]
    pub fn events(&self) -> &[EventDeclaration] {
        &self.events
    }

    /// The function names of the handlers bound to the Event `event`, in source order: what a
    /// fire of it runs (Story 6.5). Empty for an Event no handler is bound to.
    pub fn handlers_for<'a>(&'a self, event: &'a str) -> impl Iterator<Item = &'a str> + 'a {
        self.program
            .handlers
            .iter()
            .filter(move |handler| handler.event.name == event)
            .map(|handler| handler.function.name.as_str())
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
        self.declaration(name).map(|at| &self.events[at])
    }

    /// The index in [`Plugin::events`] of the Event `name` a Backend may fire, refused exactly as
    /// [`Plugin::event`] refuses it.
    fn declaration(&self, name: &str) -> Result<usize, Box<ErrorBody>> {
        if name == BACKEND_REGISTERED_INIT {
            return Err(protocol(
                ProtocolCode::UndeclaredEvent,
                format!("`{BACKEND_REGISTERED_INIT}` is reserved; a Backend cannot fire it"),
            ));
        }
        self.index
            .get(name)
            .copied()
            .filter(|&at| at < self.events.len())
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
    clients: RwLock<HashMap<ClientId, ClientPlugins>>,
    /// Every ready Plugin's Global Variable store (Story 6.7), by id. Only `hexput-session` tears
    /// one down (AD-4), through [`Plugins::global_variables`] as the Daemon wires it.
    globals: Arc<GlobalVariables>,
}

/// The Plugins one open Client ID holds: by id, and each id by its name (Story 6.5), kept in
/// step by [`Plugins::insert`].
#[derive(Debug, Default)]
struct ClientPlugins {
    by_id: HashMap<PluginId, Arc<Plugin>>,
    by_name: HashMap<String, PluginId>,
}

impl Plugins {
    /// An empty registry, with no Client ID open.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// The Global Variable stores of the Plugins this registry holds (Story 6.7): what the Daemon
    /// hands `hexput-session`, the only caller of their teardown (AD-4).
    #[must_use]
    pub fn global_variables(&self) -> Arc<GlobalVariables> {
        Arc::clone(&self.globals)
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
            .and_then(|plugins| plugins.by_id.get(&id))
            .map(Arc::clone)
    }

    /// The ready Plugin `client_id` holds under the metadata `name` (compared exactly, by bytes),
    /// if it holds one — what a fire resolves (Story 6.5). A Plugin whose initialization has not
    /// completed is not held yet, so it is not found.
    #[must_use]
    pub fn named(&self, client_id: ClientId, name: &str) -> Option<Arc<Plugin>> {
        let clients = self.read();
        let plugins = clients.get(&client_id)?;
        plugins
            .by_name
            .get(name)
            .and_then(|id| plugins.by_id.get(id))
            .map(Arc::clone)
    }

    /// The ids of every Plugin `client_id` holds, sorted; empty when it holds none or is not
    /// open.
    #[must_use]
    pub fn ids(&self, client_id: ClientId) -> Vec<PluginId> {
        let mut ids: Vec<PluginId> = self
            .read()
            .get(&client_id)
            .map(|plugins| plugins.by_id.keys().copied().collect())
            .unwrap_or_default();
        ids.sort_unstable();
        ids
    }

    /// How many Plugins are held, across every Client ID.
    #[must_use]
    pub fn len(&self) -> usize {
        self.read()
            .values()
            .map(|plugins| plugins.by_id.len())
            .sum()
    }

    /// Whether no Plugin is held.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Store `plugin` for `client_id`, returning whether it was stored: `false`, storing
    /// nothing, when `client_id` is not open. The plugin is dropped outside the lock either way.
    ///
    /// Its Global Variable store joins [`Plugins::global_variables`] under the same lock, so once
    /// [`Plugins::close`] has run for a Client ID no store of it can join: the Session's teardown
    /// closes this registry before it tears the stores down.
    fn insert(&self, client_id: ClientId, plugin: Plugin) -> bool {
        let plugin = Arc::new(plugin);
        let mut clients = self.write();
        match clients.get_mut(&client_id) {
            Some(plugins) => {
                self.globals.insert(plugin.id, Arc::clone(&plugin.globals));
                plugins.by_name.insert(plugin.name().to_owned(), plugin.id);
                plugins.by_id.insert(plugin.id, plugin);
                true
            }
            None => false,
        }
    }

    /// The map for reading. A poisoned lock is still a consistent map: no code here panics
    /// halfway through changing it.
    fn read(&self) -> std::sync::RwLockReadGuard<'_, HashMap<ClientId, ClientPlugins>> {
        self.clients.read().unwrap_or_else(PoisonError::into_inner)
    }

    /// The map for writing; see [`read`](Self::read).
    fn write(&self) -> std::sync::RwLockWriteGuard<'_, HashMap<ClientId, ClientPlugins>> {
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
    /// Its Global Variable store (Story 6.7): seeded by [`initialize`], held by no registry until
    /// [`finish`] — so a registration that fails leaves none behind.
    globals: Arc<GlobalStore>,
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
        globals: Arc::new(GlobalStore::new()),
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
/// First, before any handler, the Global Variables are initialized (Story 6.7): every Global
/// Variable initializer runs once, in source order, seeding the Plugin's store
/// ([`hexput_exec::seed_globals`], under the same limits and no host); no invocation runs one
/// again. Every handler then reads and writes the Globals through that store, so a value an init
/// handler writes is what the first fire reads. A Plugin with no init handler makes no call.
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
        hexput_exec::seed_globals(
            Arc::clone(&prepared.executable),
            Arc::clone(&prepared.globals),
            limits,
        )
        .await
        .map_err(|diagnostic| Box::new(ErrorBody::from(&diagnostic)))?;
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
                Some(Arc::clone(&prepared.globals)),
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
        globals,
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
        globals,
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

/// One accepted `EventFire` (Story 6.5): the ready Plugin, the declared Event and the converted
/// `params`, as [`prepare_fire`] makes it and [`run_fire`] runs it.
#[derive(Debug)]
pub struct Fire {
    plugin: Arc<Plugin>,
    event: String,
    /// The fired Event's index in the Plugin's declarations (Story 6.6).
    declaration: usize,
    params: Held,
}

impl Fire {
    /// The Plugin fired.
    #[must_use]
    pub fn plugin(&self) -> &Plugin {
        &self.plugin
    }

    /// The Event fired, a name its registration declared.
    #[must_use]
    pub fn event(&self) -> &str {
        &self.event
    }

    /// The fired Event's declared return shape, every handler's result is held to (Story 6.6).
    #[must_use]
    pub fn returns(&self) -> &Shape {
        &self.plugin.events[self.declaration].returns
    }

    /// The parameters every handler receives its own copy of: an object.
    #[must_use]
    pub const fn params(&self) -> &Held {
        &self.params
    }

    /// The function names of the handlers the fire runs, in source order.
    pub fn handlers(&self) -> impl Iterator<Item = &str> {
        self.plugin.handlers_for(&self.event)
    }
}

/// How one handler of a fire ended (Story 6.5): its function name, and its execution's
/// [`Finished`] or the [`Diagnostic`] it failed with.
#[derive(Debug)]
pub struct HandlerOutcome {
    /// The handler's function name.
    pub handler: String,
    /// What its invocation returned.
    pub result: Result<Finished, Diagnostic>,
}

/// The ready Plugin an `EventFire` `payload` names for `client_id`, looked up in `plugins` as the
/// connection dispatches the request (Story 6.5) — so a fire takes effect in the order it was
/// sent. Decodes nothing: it reads the first `plugin` key holding a string, and is `None` when
/// there is none, or when `client_id` holds no ready Plugin by that name. [`prepare_fire`] judges
/// the payload and gives every refusal.
#[must_use]
pub fn fetch(payload: &Value, client_id: ClientId, plugins: &Plugins) -> Option<Arc<Plugin>> {
    let name = payload
        .as_map()?
        .iter()
        .find(|(key, _)| key.as_str() == Some(PLUGIN))?
        .1
        .as_str()?;
    plugins.named(client_id, name)
}

/// Accept one `EventFire` (Story 6.5, FR-19): decode `payload` — exactly
/// `{plugin, event, params}` — and resolve the Plugin `fetched` ([`fetch`]'s result for it) and
/// the Event. Synchronous: the connection runs it on the blocking pool, since converting `params`
/// is as expensive as `params` is large.
///
/// `plugin` and `event` are strings, `params` a map converted exactly as a starting variable is
/// (Value Secret holders honoured; refusals name the path under `params`, such as
/// `` `params.user.tags[2]` ``) into the object every handler receives a copy of.
///
/// # Errors
///
/// In this order, each exactly one of, and nothing runs:
///
/// * `protocol.invalid_payload` — the payload is not a map (nil reads as missing), a key is
///   missing (absent or nil; every missing key named together), unknown, repeated or not a
///   string, `plugin` or `event` is not a string, or `params` is not a map or holds a value with
///   no Hexput form (`` `params` must be a map ``, or the path).
/// * `protocol.unknown_plugin` — `fetched` is `None`: this Session holds no ready Plugin by that
///   name (never registered, another Session's, or its initialization not complete).
/// * `protocol.undeclared_event` — the Plugin's registration did not declare `event`, or it is
///   the reserved `BackendRegisteredInit` ([`Plugin::event`]).
pub fn prepare_fire(payload: &Value, fetched: Option<Arc<Plugin>>) -> Result<Fire, Box<ErrorBody>> {
    let (name, event, params) =
        decode_fire(payload).map_err(|message| protocol(ProtocolCode::InvalidPayload, message))?;
    let Some(plugin) = fetched.filter(|plugin| plugin.name() == name) else {
        return Err(protocol(
            ProtocolCode::UnknownPlugin,
            format!(
                "this Session has no ready Plugin named `{}`; nothing was fired",
                bounded(name)
            ),
        ));
    };
    let declaration = plugin.declaration(event)?;
    let event = plugin.events[declaration].name.clone();
    Ok(Fire {
        plugin,
        event,
        declaration,
        params,
    })
}

/// Run `fire` (Story 6.5): every handler bound to its Event, once each, in source order, one
/// after another, each through the one handler-invocation function,
/// [`hexput_exec::invoke_handler`] (AD-3), as its own execution with its own Resource Budget and
/// its own copy of the parameters.
///
/// Each runs under `settings` — the Session's live Config at dispatch (AD-5) — as
/// [`Limits::from_settings`] computes the limits and feature toggles from it, and reaches the
/// host through the Session's `registrations` and `caller`, exactly as a Script would. A
/// handler's failure — runtime, budget, capability, host or policy — is its outcome, and the next
/// handler still runs; nothing about the Plugin changes. An Event no handler is bound to makes
/// no outcome. The settings are read when it is called; the future borrows only `fire`.
pub fn run_fire<'f>(
    fire: &'f Fire,
    registrations: Vec<Registration>,
    caller: Caller,
    settings: &Settings,
) -> impl Future<Output = Vec<HandlerOutcome>> + Send + 'f {
    // Read now, as the call is made: the future borrows nothing of `settings`.
    let limits = Limits::from_settings(settings);
    async move {
        let mut outcomes = Vec::new();
        for function in fire.handlers() {
            let host = Host::new(registrations.clone(), caller.clone());
            let result = hexput_exec::invoke_handler(
                Arc::clone(fire.plugin.executable()),
                function,
                fire.params.clone(),
                host,
                limits,
                Some(Arc::clone(fire.plugin.globals())),
            )
            .await;
            outcomes.push(HandlerOutcome {
                handler: function.to_owned(),
                result,
            });
        }
        outcomes
    }
}

/// The `Result` payload answering a fire (Story 6.5): `{results}`, one entry per outcome in the
/// order given. A success is `{handler, value}`, with `modifications` after `value` when the
/// handler wrote a referenced place — converted exactly as a Script's result and modifications
/// are; a failure is `{handler, error}`, `error` the one wire error shape of its diagnostic (span
/// included). The entries share one frame: each may take only what the entries before it left of
/// [`MAX_FRAME_LEN`], so a large result costs its own entry, never its siblings'. A result an
/// entry cannot carry is that entry's error instead: `protocol.result_too_deep`,
/// `protocol.response_too_large` or `type.function_result`. Never carries `findings`.
///
/// A converted result is then held to `fire`'s declared return shape (Story 6.6, [`conform`]): one
/// that does not conform makes its entry `{handler, error}` with `protocol.shape_mismatch` naming
/// the Event and the first offending path, and no `modifications`; each entry is checked on its
/// own.
#[must_use]
pub fn fire_reply(fire: &Fire, outcomes: &[HandlerOutcome]) -> Value {
    // What the frame leaves the entries: the envelope's room, and the payload's map, `results`
    // key and the array's header.
    let mut left = MAX_FRAME_LEN - findings::ENVELOPE_ROOM - (RESULTS.len() + 1 + 1 + 5);
    let entries = outcomes
        .iter()
        .map(|outcome| {
            let handler = (Value::from(HANDLER), Value::from(outcome.handler.as_str()));
            let failed = |body: &ErrorBody| {
                Value::Map(vec![handler.clone(), (Value::from(ERROR), body.to_value())])
            };
            let mut entry = match &outcome.result {
                Ok(finished) => match entry_value(finished, fire, left) {
                    Ok(mut fields) => {
                        fields.insert(0, handler.clone());
                        Value::Map(fields)
                    }
                    Err(body) => failed(&body),
                },
                Err(diagnostic) => failed(&ErrorBody::from(diagnostic)),
            };
            let mut size = findings::encoded_len(&entry);
            if size > left {
                entry = failed(&too_large());
                size = findings::encoded_len(&entry);
            }
            left = left.saturating_sub(size);
            entry
        })
        .collect();
    Value::Map(vec![(Value::from(RESULTS), Value::Array(entries))])
}

/// The error of an entry whose result does not fit what is left of the reply's frame.
fn too_large() -> Box<ErrorBody> {
    protocol(
        ProtocolCode::ResponseTooLarge,
        format!(
            "the handler's result and its modifications encode to more than what is left of the \
             maximum frame of {MAX_FRAME_LEN} bytes beside the entries before it"
        ),
    )
}

/// A successful handler's `value` and, when it wrote a referenced place, `modifications` — taking
/// at most `budget` bytes together — or the error its entry carries instead: a conversion's,
/// then a mismatch with `fire`'s return shape (Story 6.6).
fn entry_value(
    finished: &Finished,
    fire: &Fire,
    budget: usize,
) -> Result<Vec<(Value, Value)>, Box<ErrorBody>> {
    let Finished {
        result,
        modifications,
    } = finished;
    let too_deep = |what: &str| {
        protocol(
            ProtocolCode::ResultTooDeep,
            format!(
                "the handler's {what} nests more than {MAX_ENTRY_DEPTH} arrays or objects deep \
                 (counting its Value Secret holders), past what a fire reply's entry may carry"
            ),
        )
    };
    let unsendable = |unsendable: Unsendable, what: &str| match unsendable {
        Unsendable::TooDeep => too_deep(what),
        Unsendable::TooLarge => too_large(),
        Unsendable::Unrepresentable => Box::new(ErrorBody::from(&Diagnostic::new(
            Category::Type,
            Code::FUNCTION_RESULT,
            "the handler's result holds a value with no wire representation",
            Span::new(0, 0, 1, 1),
        ))),
    };
    // The result and its modifications share what the frame has left, as a Script's do.
    let mut budget = budget;
    let value = exec_wire::result_to_wire_within(result, &mut budget)
        .map_err(|u| unsendable(u, "result"))?;
    if exec_wire::wire_depth(&value) > MAX_ENTRY_DEPTH {
        return Err(too_deep("result"));
    }
    shape::conform(&value, fire.returns()).map_err(|problem| {
        protocol(
            ProtocolCode::ShapeMismatch,
            format!(
                "the handler's result does not conform to the `{}` return shape: {problem}",
                bounded(fire.event())
            ),
        )
    })?;
    let mut fields = vec![(Value::from(VALUE), value)];
    if !modifications.is_empty() {
        let what = "`modifications` list";
        let listed = exec_wire::modifications_to_wire(modifications, &mut budget)
            .map_err(|u| unsendable(u, what))?;
        if exec_wire::wire_depth(&listed) > MAX_ENTRY_DEPTH {
            return Err(too_deep(what));
        }
        fields.push((Value::from(MODIFICATIONS_KEY), listed));
    }
    Ok(fields)
}

/// Decode an `EventFire` payload by hand: exactly `{plugin, event, params}`, `params` converted.
fn decode_fire(payload: &Value) -> Result<(&str, &str, Held), String> {
    let fields: &[(Value, Value)] = match payload {
        Value::Nil => &[],
        Value::Map(fields) => fields,
        _ => {
            return Err(format!(
                "the `{FIRE_REQUEST}` payload must be a map with `{PLUGIN}`, `{EVENT}` and \
                 `{PARAMS}`"
            ));
        }
    };
    let mut plugin = None;
    let mut event = None;
    let mut params = None;
    for (key, value) in fields {
        let slot = match key.as_str() {
            Some(PLUGIN) => &mut plugin,
            Some(EVENT) => &mut event,
            Some(PARAMS) => &mut params,
            Some(other) => {
                return Err(format!(
                    "the `{FIRE_REQUEST}` payload has an unknown key `{}`; it holds exactly \
                     `{PLUGIN}`, `{EVENT}` and `{PARAMS}`",
                    bounded(other)
                ));
            }
            None => {
                return Err(format!(
                    "the `{FIRE_REQUEST}` payload has a key that is not a string"
                ));
            }
        };
        if slot.is_some() {
            return Err(format!(
                "the `{FIRE_REQUEST}` payload repeats the key `{}`",
                key.as_str().unwrap_or_default()
            ));
        }
        *slot = Some(value);
    }
    let plugin = plugin.filter(|value| !value.is_nil());
    let event = event.filter(|value| !value.is_nil());
    let params = params.filter(|value| !value.is_nil());
    let missing: Vec<String> = [
        (PLUGIN, plugin.is_none()),
        (EVENT, event.is_none()),
        (PARAMS, params.is_none()),
    ]
    .into_iter()
    .filter(|(_, missing)| *missing)
    .map(|(key, _)| format!("`{key}`"))
    .collect();
    let (Some(plugin), Some(event), Some(params)) = (plugin, event, params) else {
        let named = match missing.as_slice() {
            [one] => one.clone(),
            [first, second] => format!("{first} and {second}"),
            [rest @ .., last] => format!("{} and {last}", rest.join(", ")),
            [] => String::new(),
        };
        return Err(format!("the `{FIRE_REQUEST}` payload is missing {named}"));
    };
    let text = |value: &'_ Value, key: &str, what: &str| -> Result<(), String> {
        match value {
            Value::String(text) if text.as_str().is_some() => Ok(()),
            Value::String(_) => Err(format!(
                "the `{FIRE_REQUEST}` payload's `{key}` is not valid UTF-8"
            )),
            _ => Err(format!(
                "the `{FIRE_REQUEST}` payload's `{key}` must be a string ({what})"
            )),
        }
    };
    text(plugin, PLUGIN, "the Plugin's name")?;
    text(event, EVENT, "the Event's name")?;
    let must_be_a_map = || format!("`{PARAMS}` must be a map, the object every handler receives");
    if !matches!(params, Value::Map(_)) {
        return Err(must_be_a_map());
    }
    let params = exec_wire::to_hexput(params, &mut Path::params())?;
    // A holder around a scalar is a map on the wire but no object once converted.
    if !matches!(params.value, ExecValue::Object(_)) {
        return Err(format!(
            "{}; a Value Secret holder around anything but a map is not one",
            must_be_a_map()
        ));
    }
    Ok((
        plugin.as_str().unwrap_or_default(),
        event.as_str().unwrap_or_default(),
        params,
    ))
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
