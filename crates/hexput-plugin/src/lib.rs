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
//! id, with its own parsed program and Global Variable declarations. Event declarations, init
//! handlers, firing, the Global Variable store and unregister land with Stories 6.3–6.14. The
//! static check at registration (AD-8) is deferred to Story 6.3, which adds the Event
//! declarations its Plugin-only findings need.
//!
//! This crate has no async runtime: [`register`] is synchronous, and the connection runs it on
//! the blocking pool. No lock here is held while calling code outside this crate's own types.
//!
//! Binds: AD-3, AD-4, AD-6, AD-8.

use std::collections::HashMap;
use std::sync::{Arc, PoisonError, RwLock};

use hexput_parser::PluginProgram;
use hexput_port::{ClientId, ErrorBody, PluginId, ProtocolCode, ProtocolError, Value};

/// The `PluginRegister` payload's one key.
const SOURCE: &str = "source";
/// The success reply's one key.
const NAME: &str = "name";
/// The request's wire name, for messages.
const REQUEST: &str = "PluginRegister";

/// One registered Plugin: its id, and its parsed source.
#[derive(Debug)]
pub struct Plugin {
    id: PluginId,
    program: PluginProgram,
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

/// Serve one Plugin registration (FR-17, Story 6.2): `payload` is a `PluginRegister` payload,
/// exactly `{source}`, the Plugin source as a string.
///
/// In order: the payload is decoded; the source is parsed with `hexput_parser::parse_plugin`;
/// its metadata `name` is handed to `reserve`, which reserves it in `client_id`'s Session (the
/// connection wires it to `hexput_session::Sessions::register_plugin`); and the parsed Plugin is
/// stored in `plugins` under `client_id` and the reserved id. Nothing is reserved for a payload
/// or source that is refused, and nothing is stored unless the reservation succeeded. `reserve`
/// is called with no lock of `plugins` held.
///
/// Returns the `Result` payload `{name}`, the metadata `name` echoed. Synchronous, and as
/// expensive as the parse: the connection runs it on the blocking pool.
///
/// # Errors
///
/// The `Error` payload, each exactly one of, and in every case nothing is stored:
///
/// * `protocol.invalid_payload` — the payload is not a map (nil reads as missing), `source` is
///   missing or not a string, or a key is unknown, repeated or not a string. The message names
///   the key.
/// * The parser's [`Diagnostic`](hexput_parser::Diagnostic) — `lexical.*` or `syntax.*`, with
///   its span. Nothing is reserved.
/// * `protocol.duplicate_plugin_name` — the Session already holds a Plugin with this name; that
///   Plugin is untouched.
/// * `protocol.init_not_completed` — the Session has ended, before the reservation or before the
///   insert (`client_id` is not open in `plugins`): "the Session has ended; nothing was
///   registered".
pub fn register(
    payload: &Value,
    client_id: ClientId,
    plugins: &Plugins,
    reserve: impl FnOnce(&str) -> Reservation,
) -> Result<Value, Box<ErrorBody>> {
    let source =
        decode(payload).map_err(|message| protocol(ProtocolCode::InvalidPayload, message))?;
    let program = hexput_parser::parse_plugin(source).map_err(|d| Box::new(ErrorBody::from(&d)))?;
    let name = program.metadata.name().to_owned();
    let id = match reserve(&name) {
        Reservation::Reserved(id) => id,
        Reservation::NameTaken => {
            return Err(protocol(
                ProtocolCode::DuplicatePluginName,
                format!(
                    "this Session already has a Plugin named `{}`; nothing was registered",
                    hexput_exec::wire::bounded(&name)
                ),
            ));
        }
        Reservation::SessionEnded => return Err(session_ended()),
    };
    if !plugins.insert(client_id, Plugin { id, program }) {
        return Err(session_ended());
    }
    Ok(Value::Map(vec![(Value::from(NAME), Value::from(name))]))
}

/// Decode a `PluginRegister` payload by hand: exactly `{source}`, a string.
fn decode(payload: &Value) -> Result<&str, String> {
    let fields: &[(Value, Value)] = match payload {
        Value::Nil => &[],
        Value::Map(fields) => fields,
        _ => {
            return Err(format!(
                "the `{REQUEST}` payload must be a map with `{SOURCE}`"
            ));
        }
    };
    let mut source = None;
    for (key, value) in fields {
        match key.as_str() {
            Some(SOURCE) if source.is_some() => {
                return Err(format!(
                    "the `{REQUEST}` payload repeats the key `{SOURCE}`"
                ));
            }
            Some(SOURCE) => source = Some(value),
            Some(other) => {
                return Err(format!(
                    "the `{REQUEST}` payload has an unknown key `{}`",
                    hexput_exec::wire::bounded(other)
                ));
            }
            None => {
                return Err(format!(
                    "the `{REQUEST}` payload has a key that is not a string"
                ));
            }
        }
    }
    match source {
        None | Some(Value::Nil) => Err(format!("the `{REQUEST}` payload is missing `{SOURCE}`")),
        Some(Value::String(text)) => text
            .as_str()
            .ok_or_else(|| format!("the `{REQUEST}` payload's `{SOURCE}` is not valid UTF-8")),
        Some(_) => Err(format!(
            "the `{REQUEST}` payload's `{SOURCE}` must be a string"
        )),
    }
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
