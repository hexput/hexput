//! Plugin actor: registration, event routing, priority/async dispatch. Owns event routing
//! and ordering metadata only — it does not itself execute the ordered chain; a per-
//! (Plugin, Event) sequencing task, spawned independently of the actor's mailbox, does that.
//! Invokes hexput-check once, at Plugin registration.
//!
//! # What Story 6.2 adds
//!
//! Registration. A Backend registers a Plugin with a `PluginRegister` request — its message
//! type alone marks it as one, so nothing is inferred from the payload's shape — whose payload is
//! exactly `{source}`. [`register`] decodes it, parses the source with
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
//! reserves nothing ([`try_register`] tells it apart so the connection can log it).
//!
//! This crate has no async runtime: [`register`] is synchronous, and the connection runs it on
//! the blocking pool. No lock here is held while calling code outside this crate's own types.
//!
//! Binds: AD-3, AD-4, AD-6, AD-8.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, PoisonError, RwLock};

use hexput_check::{Environment, Policy};
use hexput_exec::wire::{MAX_RESULT_DEPTH, bounded};
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
/// [`register`] reports it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reservation {
    /// The name was free in the Session and is now reserved under this id.
    Reserved(PluginId),
    /// The Session already holds a Plugin with this name.
    NameTaken,
    /// The Session no longer exists.
    SessionEnded,
}

/// Why [`try_register`] registered nothing: a refusal, or a rejection by the static check that
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

/// Serve one Plugin registration (FR-17, FR-19, FR-25; Stories 6.2, 6.3): `payload` is a
/// `PluginRegister` payload, exactly `{source, events}` — the Plugin source as a string, and the
/// Events the Backend may fire, each with its return shape and partition-key flag.
///
/// [`try_register`] with its refusal turned into the body to send; see it for everything else.
///
/// # Errors
///
/// As [`try_register`], each as the `Error` payload to send.
pub fn register(
    payload: &Value,
    client_id: ClientId,
    plugins: &Plugins,
    reserve: impl FnOnce(&str) -> Reservation,
    callables: &[String],
    settings: &Settings,
) -> Result<Value, Box<ErrorBody>> {
    try_register(payload, client_id, plugins, reserve, callables, settings)
        .map_err(Refusal::into_body)
}

/// Serve one Plugin registration, telling a rejection by the static check apart so the caller can
/// log it.
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
/// list, the declared Event names and the policy the feature toggles describe; its metadata
/// `name` is handed to `reserve`, which reserves it in `client_id`'s Session (the connection wires
/// it to `hexput_session::Sessions::register_plugin`); and the Plugin is stored in `plugins` under
/// `client_id` and the reserved id. Nothing is reserved for a payload, source or check that
/// refuses, and nothing is stored unless the reservation succeeded. `reserve` is called with no
/// lock of `plugins` held.
///
/// Returns the `Result` payload `{name}`, the metadata `name` echoed — with `findings` after it
/// when the pass ran and found anything (at most `hexput_port::MAX_FINDINGS`, in source order,
/// left out if they would not fit a frame). Synchronous, and as expensive as the parse and the
/// check: the connection runs it on the blocking pool.
///
/// # Errors
///
/// Each exactly one of, and in every case nothing is stored:
///
/// * `protocol.invalid_payload` — the payload is not a map (nil reads as missing), `source` or
///   `events` is missing (absent or nil) or mistyped, a key is unknown, repeated or not a string,
///   or an Event declaration is malformed. The message names the key or the full path
///   (`` `events.OrderPlaced.keyed` ``); a refused shape is
///   `` `events.X.returns` is not a return shape: <parser message> (line L, column C) ``.
/// * The parser's [`Diagnostic`](hexput_parser::Diagnostic) — `lexical.*` or `syntax.*`, with
///   its span. Nothing is reserved.
/// * [`Refusal::Rejected`] — under check mode `error`, a source with an error-severity finding:
///   the first one's body with every finding (warnings too) under `findings`. Nothing is
///   reserved.
/// * `protocol.duplicate_plugin_name` — the Session already holds a Plugin with this name; that
///   Plugin is untouched.
/// * `protocol.init_not_completed` — the Session has ended, before the reservation or before the
///   insert (`client_id` is not open in `plugins`): "the Session has ended; nothing was
///   registered".
pub fn try_register(
    payload: &Value,
    client_id: ClientId,
    plugins: &Plugins,
    reserve: impl FnOnce(&str) -> Reservation,
    callables: &[String],
    settings: &Settings,
) -> Result<Value, Refusal> {
    let (source, events) =
        decode(payload).map_err(|message| protocol(ProtocolCode::InvalidPayload, message))?;
    let program = hexput_parser::parse_plugin(source).map_err(|d| Box::new(ErrorBody::from(&d)))?;
    let found = check(&program, &events, callables, settings).map_err(Refusal::Rejected)?;
    let name = program.metadata.name().to_owned();
    let id = match reserve(&name) {
        Reservation::Reserved(id) => id,
        Reservation::NameTaken => {
            return Err(protocol(
                ProtocolCode::DuplicatePluginName,
                format!(
                    "this Session already has a Plugin named `{}`; nothing was registered",
                    bounded(&name)
                ),
            )
            .into());
        }
        Reservation::SessionEnded => return Err(session_ended().into()),
    };
    let index = events
        .iter()
        .enumerate()
        .map(|(at, event)| (event.name.clone(), at))
        .collect();
    let plugin = Plugin {
        id,
        program,
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
