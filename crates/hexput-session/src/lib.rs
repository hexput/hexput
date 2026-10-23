//! Client ID -> Session, TTL (from System Config), reconnect + FR-13 credential protection.
//! Holds the single mutable, live copy of a Session's per-backend Config; does not depend on
//! hexput-config (System Config), keeping the two surfaces separate.
//!
//! Depends on hexput-globalvar because AD-4 makes this crate the sole caller of
//! `teardown(plugin_id)` — invoked synchronously from the TTL-expiry and explicit-unregister
//! paths, before the Plugin actor is dropped, and never implied by `Drop`.
//!
//! # What lands in Stories 2.4 and 2.5
//!
//! [`Sessions`] is the registry. [`Sessions::connect`] issues a Connection its [`ConnectionId`]
//! when it opens, before init; [`Sessions::create`] turns a decoded [`InitRequest`] into a
//! Session keyed by a freshly issued [`ClientId`], born with its creating Connection attached.
//! A Session holds a *set* of attached Connections — zero or more, never "exactly one" (AD-2).
//! [`Sessions::detach`] removes one; detaching the last hands the Session to the one explicit
//! teardown path — at once on a registry with no TTL, or after the TTL Story 5.8 adds, which
//! delays that path rather than relocating it.
//!
//! # What Story 3.8 adds
//!
//! [`Sessions::update_config`] replaces a live Session's Config with a whole new one, decoded from
//! a `ConfigUpdate` payload by [`Config::from_update_payload`]. It is still the single live copy
//! (AD-5): every attached Connection's later executions read it afresh when dispatched, and an
//! execution already dispatched keeps what it read. A refused update never reaches the registry.
//!
//! # What Story 4.3 adds
//!
//! [`Sessions::with_teardown`] gives the registry a teardown hook: a function called with the
//! Client ID of every Session the one teardown path releases, last, after the Config and the
//! registrations are released and outside any lock. The Daemon wires it to the AST Cache, so a
//! Session's cached Scripts go with it without this crate reaching `hexput-script`. Story 5.8
//! delays teardown behind a TTL, and the hook with it.
//!
//! # What Story 5.5 adds
//!
//! [`Sessions::create`] also issues the Session's reconnect credential (FR-13): a 256-bit
//! [`ReconnectSecret`] from the OS CSPRNG, returned once with the Client ID for the `Init` reply
//! to carry. The Session keeps only a per-Session random salt and the HMAC-SHA256 of the secret
//! keyed by it — never the secret, which is never logged and whose `Debug` is redacted.
//! [`Sessions::verify_reconnect`] checks a presented secret against that hash in constant time;
//! an unknown Client ID is checked against a fixed dummy credential, so either way one HMAC verify
//! runs and gives `false` (not a promise of equal timing: the registry lookup is not constant
//! time). The credential is defined here alone, whatever carried the
//! Connection; Story 5.6 builds reconnect on it.
//!
//! # What Story 5.6 adds
//!
//! [`Sessions::reconnect`] is the one wire path that attaches a further Connection to a live
//! Session: it verifies the presented secret with the Session's credential (constant time) and,
//! when it matches, inserts the Connection under the same shard lock — so whatever removes the
//! Session (a [`Sessions::detach`] on a registry with no TTL, or [`Sessions::expire`] once Story
//! 5.8's TTL ran out) either removes it first (and the reconnect is denied) or sees the new
//! attachment. An unknown Client ID runs the same dummy verify as
//! [`Sessions::verify_reconnect`] and is denied identically. [`ReconnectRequest`] decodes a
//! `Reconnect` payload — exactly `client_id` and `reconnect_secret` — from the payload alone,
//! before any lookup; a carried Config or registrations list is refused, never applied, since the
//! Session's stored ones are resumed. The presented secret is never logged or echoed, and is
//! dropped with the request after verification.
//!
//! # What Story 5.7 adds
//!
//! Nothing new in the registry: it states the multi-Connection contract the registry already
//! holds. Any number of Connections may be attached to one Session at once — the first by
//! `Init`, every further one by [`Sessions::reconnect`] — with no cap, whatever carried each of
//! them. What the Session owns is shared by all of them and read afresh at dispatch: its Config
//! (replaced by [`Sessions::update_config`] from any attached Connection), the registrations made
//! at `Init` (there is no request that registers a function later) and, keyed by its Client ID,
//! its AST Cache entries. What a Connection owns — its request ids, its replies, its host-call
//! correlation — is never here, so nothing in this crate can deliver one Connection's reply to
//! another, and no change made through one is announced to the others. [`Sessions::detach`]
//! removes one Connection alone; the Session can go only once the last one has, in any detach
//! order (at once with no TTL, else after Story 5.8's countdown).
//!
//! # What Story 5.8 adds
//!
//! A Session outlives its last Connection by a TTL. [`Sessions::with_ttl`] sets it (the Daemon
//! passes System Config's `session_ttl_secs`); the registry stays synchronous, so it owns *what*
//! expires and the caller owns *when*. When the last Connection detaches, [`Sessions::detach`]
//! marks the Session expiring under a fresh countdown generation and returns an [`Expiry`]; the
//! caller waits [`Expiry::ttl`] and then calls [`Sessions::expire`], which removes the Session —
//! under the shard lock, through the same private teardown path and hook as before — only if it
//! is still unattached *and* still under that generation. A [`Sessions::reconnect`] (or
//! [`Sessions::attach`]) during the countdown attaches and stops it under the same shard lock, the
//! Session's Config, registrations, AST Cache entries and reconnect secret untouched; an `Expiry`
//! from before is stale from then on, even if the Session is orphaned again (each orphaning draws
//! a new generation). Once expired, the Session is gone from the registry, so a reconnect with its
//! Client ID is denied exactly like one with an unknown Client ID. While any Connection is
//! attached no countdown runs. A registry with no TTL ([`Sessions::new`], or a zero TTL) tears
//! down on the last detach at once, as before — the same path with no delay. There is no reaper:
//! a countdown is one caller's timer.
//!
//! # What Story 6.2 adds
//!
//! A Session owns the set of its Plugins: their metadata names, each under a [`PluginId`].
//! [`Sessions::register_plugin`] checks that a name is free *in this Session* and reserves it
//! under a freshly issued id in one step, under the Session's shard lock, so two concurrent
//! registrations of one name cannot both succeed; another Session's names are never consulted
//! (names are unique per Client ID, not Daemon-wide) and compare exactly, by bytes. Ids come from
//! one counter per registry, starting at 1 and never reused during a run.
//! [`Sessions::plugins`] lists the set. The parsed Plugin itself lives in `hexput-plugin`'s
//! registry, which this crate cannot name; the Daemon's teardown hook releases it. The set goes
//! with the Session on the one teardown path, where Story 6.13 calls each id's
//! `hexput_globalvar::teardown` first (AD-4).
//!
//! # What Story 6.4 adds
//!
//! A reserved name may be given back: a registration whose `BackendRegisteredInit` handlers fail
//! calls [`Sessions::release_plugin`], which frees the name only while it still maps to that
//! registration's id, so the same name can be registered afresh (under a new id).
//!
//! Nothing here is `async`, so no lock of this crate's can be held across an `.await`; nothing
//! here writes to disk.
//!
//! Binds: AD-2, AD-4, AD-5, FR-13.

mod credential;
mod init;
mod reconnect;

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use dashmap::DashMap;
use dashmap::mapref::entry::Entry;
use hexput_globalvar::Teardown;
pub use hexput_shared::ids::{ClientId, PluginId};

use credential::Credential;
pub use credential::ReconnectSecret;
pub use hexput_port::{Setting, Settings};
pub use init::{Config, InitError, InitRequest, RegisteredFunction};
pub use reconnect::ReconnectRequest;

/// A Connection's identity for this Daemon run, issued by [`Sessions::connect`] when it opens —
/// before init, so even its pre-init log events name it — and also its attachment to a Session.
/// Local to this Daemon run and never on the wire, so it is not a `hexput-shared` id: no two
/// Connections in one run share a value, and a Connection is attached under this id alone.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ConnectionId(u64);

impl ConnectionId {
    /// The raw counter value, for logs and tests.
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }
}

/// A Backend's Session: the single live copy of its Config (AD-5), its Registered Functions,
/// the Connections attached to it right now, and the salted hash of its reconnect secret (never
/// the secret).
#[derive(Debug)]
pub struct Session {
    config: Config,
    credential: Credential,
    registrations: Vec<RegisteredFunction>,
    connections: HashSet<ConnectionId>,
    /// The Session's Plugins (Story 6.2): each metadata name, unique in this Session, under the
    /// id issued when it was reserved.
    plugins: HashMap<Arc<str>, PluginId>,
    /// The generation of the running TTL countdown (Story 5.8), if one runs: set when the last
    /// Connection detaches, cleared when one attaches.
    expiring: Option<u64>,
}

/// A Session's TTL countdown, started when its last Connection detached (Story 5.8) and returned
/// by [`Sessions::detach`] — the only way to make one. The holder waits [`ttl`](Self::ttl) and
/// then hands it to [`Sessions::expire`], which tears the Session down only if this countdown is
/// still the one running.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Expiry {
    client_id: ClientId,
    ttl: Duration,
    generation: u64,
}

impl Expiry {
    /// The Session this countdown belongs to.
    #[must_use]
    pub const fn client_id(&self) -> ClientId {
        self.client_id
    }

    /// How long to wait before [`Sessions::expire`]: the registry's TTL when the countdown
    /// started.
    #[must_use]
    pub const fn ttl(&self) -> Duration {
        self.ttl
    }
}

/// Why [`Sessions::register_plugin`] reserved nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PluginNameError {
    /// This Session already holds a Plugin with exactly this name.
    Taken,
    /// No Session has this Client ID: never issued, or already torn down.
    NoSession,
}

impl core::fmt::Display for PluginNameError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(match self {
            Self::Taken => "this Session already has a Plugin with that name",
            Self::NoSession => "no Session has this Client ID",
        })
    }
}

impl core::error::Error for PluginNameError {}

/// The registry of live Sessions, keyed by Client ID. One per Daemon run, shared by every
/// Connection.
///
/// Every operation takes one `DashMap` shard lock synchronously and releases it before
/// returning; none is `async`.
#[derive(Default)]
pub struct Sessions {
    sessions: DashMap<ClientId, Session>,
    next_connection: AtomicU64,
    /// How long a Session outlives its last Connection (Story 5.8); zero tears down at once.
    ttl: Duration,
    /// The next countdown generation; every orphaning draws a new one.
    next_generation: AtomicU64,
    /// Called with a Session's Client ID when it is torn down (Story 4.3), if set.
    on_teardown: Option<TeardownHook>,
    /// The last Plugin id issued (Story 6.2); the next is one more, so the first is 1.
    last_plugin: AtomicU64,
    /// The Plugins' Global Variable stores (Story 6.7), which this registry alone tears down
    /// (AD-4), if set.
    global_variables: Option<Arc<dyn Teardown>>,
}

/// The function [`Sessions::with_teardown`] stores.
type TeardownHook = Arc<dyn Fn(ClientId) + Send + Sync + 'static>;

impl core::fmt::Debug for Sessions {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Sessions")
            .field("sessions", &self.sessions)
            .field("next_connection", &self.next_connection)
            .field("ttl", &self.ttl)
            .field("next_generation", &self.next_generation)
            .field("on_teardown", &self.on_teardown.is_some())
            .field("last_plugin", &self.last_plugin)
            .field("global_variables", &self.global_variables.is_some())
            .finish()
    }
}

impl Sessions {
    /// An empty registry, with no teardown hook and no TTL: a Session is torn down when its last
    /// Connection detaches.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// This registry, calling `hook` with the Client ID of every Session it tears down (Story
    /// 4.3) — once per Session, after the Session is out of the registry and its Config and
    /// registrations are released, outside any lock of the registry's, so the hook may call back
    /// into it. The Daemon wires it to release the Session's AST Cache entries. A later call
    /// replaces an earlier hook.
    #[must_use]
    pub fn with_teardown(mut self, hook: impl Fn(ClientId) + Send + Sync + 'static) -> Self {
        self.on_teardown = Some(Arc::new(hook));
        self
    }

    /// This registry, tearing down the Global Variable store of every Plugin of a Session it
    /// tears down (Story 6.7, AD-4) through `stores` — `hexput_globalvar::GlobalVariables`'s
    /// `teardown(plugin_id)`, called for each of the Session's Plugin ids before anything else is
    /// released and again after the teardown hook, which closes the Plugin registry so no store
    /// of the Session can join after it — and the
    /// store of a registration whose name [`release_plugin`](Self::release_plugin) frees. This
    /// registry is the only caller of that teardown. A later call replaces an earlier one.
    #[must_use]
    pub fn with_global_variables(mut self, stores: Arc<dyn Teardown>) -> Self {
        self.global_variables = Some(stores);
        self
    }

    /// This registry, keeping a Session whose last Connection detached for `ttl` before it may be
    /// torn down (Story 5.8): [`detach`](Self::detach) then returns an [`Expiry`] carrying `ttl`
    /// for the caller to wait out before [`expire`](Self::expire). A zero `ttl` is the default:
    /// teardown at the last detach, with no `Expiry`. Chains with
    /// [`with_teardown`](Self::with_teardown) in either order; a later call replaces an earlier
    /// TTL.
    #[must_use]
    pub fn with_ttl(mut self, ttl: Duration) -> Self {
        self.ttl = ttl;
        self
    }

    /// Issue a new Connection its identity for this run. Called when a Connection opens, before
    /// it sends anything; the same id is later its attachment in [`create`](Self::create) or
    /// [`attach`](Self::attach). Takes no lock.
    #[must_use]
    pub fn connect(&self) -> ConnectionId {
        // Uniqueness is all that is needed; no other memory is ordered by this counter.
        ConnectionId(self.next_connection.fetch_add(1, Ordering::Relaxed))
    }

    /// Create a Session from a decoded `Init`, with one Connection attached: `connection`, the
    /// one that sent it (issued by [`connect`](Self::connect)). Returns the Session's newly
    /// issued Client ID and its reconnect secret — the one time the secret exists outside the
    /// caller's reply: the Session keeps only its salted hash.
    ///
    /// Precondition, not checked: `connection` came from [`connect`](Self::connect) on this
    /// registry and is attached to at most one Session, once — this call or one
    /// [`attach`](Self::attach), never both, never twice.
    ///
    /// The Client ID is 128 bits from the OS CSPRNG. A collision with a live Session draws again
    /// rather than overwriting it. The reconnect secret is 256 bits from the same source.
    ///
    /// # Panics
    ///
    /// If the OS CSPRNG cannot be read. A Daemon that cannot issue unguessable identifiers must
    /// not issue guessable ones; the panic stays within the calling Connection's task.
    #[must_use]
    pub fn create(
        &self,
        init: InitRequest,
        connection: ConnectionId,
    ) -> (ClientId, ReconnectSecret) {
        let (credential, secret) = Credential::issue();
        let session = Session {
            config: init.config,
            credential,
            registrations: init.registrations,
            connections: HashSet::from([connection]),
            plugins: HashMap::new(),
            expiring: None,
        };
        loop {
            // The entry holds its shard's lock, so no racing `create` can claim the same id
            // between the check and the insert.
            if let Entry::Vacant(vacant) = self.sessions.entry(generate_client_id()) {
                let client_id = *vacant.key();
                vacant.insert(session);
                return (client_id, secret);
            }
        }
    }

    /// Attach one more Connection — `connection`, issued by [`connect`](Self::connect) — to a
    /// live Session. Returns whether a Session has this Client ID; when none does, nothing
    /// changes.
    ///
    /// Precondition, not checked: `connection` came from [`connect`](Self::connect) on this
    /// registry and is attached to at most one Session, once — this call or one
    /// [`create`](Self::create), never both, never twice.
    ///
    /// Nothing on the wire reaches this: the wire path is [`reconnect`](Self::reconnect), which
    /// verifies the reconnect secret first. This unverified form stays for tests and for a
    /// Session with several attachments to be a state the registry supports directly (AD-2).
    ///
    /// Attaching to a Session whose TTL countdown runs stops it, under the same shard lock.
    #[must_use]
    pub fn attach(&self, client_id: ClientId, connection: ConnectionId) -> bool {
        match self.sessions.get_mut(&client_id) {
            Some(mut session) => {
                session.connections.insert(connection);
                session.expiring = None;
                true
            }
            None => false,
        }
    }

    /// Detach a Connection from its Session.
    ///
    /// When it was the last one attached and the registry has a TTL (Story 5.8), the Session
    /// stays live and unattached, its TTL countdown starts under a fresh generation in the same
    /// shard lock, and the returned [`Expiry`] is the caller's to wait out and hand to
    /// [`expire`](Self::expire). With no TTL, the Session is removed from the registry under the
    /// same shard lock as the detach — so no racing [`attach`](Self::attach) or
    /// [`reconnect`](Self::reconnect) can find a Session on its way out — and then torn down,
    /// outside any lock; nothing is returned.
    ///
    /// Detaching a Connection that is not the last, or is not attached, or from a Session that no
    /// longer exists, returns `None` and starts no countdown.
    ///
    /// Dropping a returned [`Expiry`] instead of handing it to [`expire`](Self::expire) leaves the
    /// Session unattached and expiring for the registry's whole life: nothing else ends a
    /// countdown but a reconnect. (Not `#[must_use]`, since on a registry with no TTL nothing is
    /// ever returned and many callers detach as a statement.)
    pub fn detach(&self, client_id: ClientId, connection: ConnectionId) -> Option<Expiry> {
        if self.ttl.is_zero() {
            let removed = self.sessions.remove_if_mut(&client_id, |_, session| {
                session.connections.remove(&connection) && session.connections.is_empty()
            });
            if let Some((client_id, session)) = removed {
                teardown(
                    client_id,
                    session,
                    self.on_teardown.as_deref(),
                    self.global_variables.as_deref(),
                );
            }
            return None;
        }
        let mut session = self.sessions.get_mut(&client_id)?;
        if !(session.connections.remove(&connection) && session.connections.is_empty()) {
            return None;
        }
        // Uniqueness is all that is needed; the shard lock orders the Session's own state.
        let generation = self.next_generation.fetch_add(1, Ordering::Relaxed);
        session.expiring = Some(generation);
        Some(Expiry {
            client_id,
            ttl: self.ttl,
            generation,
        })
    }

    /// End the TTL countdown `expiry` names (Story 5.8): when its Session is still unattached and
    /// still under that countdown, remove it from the registry under the shard lock and tear it
    /// down — through the one teardown path and its hook, outside any lock — and return `true`.
    ///
    /// Otherwise nothing changes and this returns `false`: the Session was reconnected (so the
    /// countdown stopped, even if a newer one has since started), or it is already gone. The
    /// caller decides when the TTL has elapsed; this checks only that the countdown is current.
    #[must_use]
    pub fn expire(&self, expiry: &Expiry) -> bool {
        let removed = self.sessions.remove_if(&expiry.client_id, |_, session| {
            session.connections.is_empty() && session.expiring == Some(expiry.generation)
        });
        match removed {
            Some((client_id, session)) => {
                teardown(
                    client_id,
                    session,
                    self.on_teardown.as_deref(),
                    self.global_variables.as_deref(),
                );
                true
            }
            None => false,
        }
    }

    /// Whether the Session's TTL countdown runs (Story 5.8): `Some(true)` from its last
    /// Connection's detach until a Connection attaches or it expires; `None` when it does not
    /// exist.
    #[must_use]
    pub fn expiring(&self, client_id: ClientId) -> Option<bool> {
        self.sessions
            .get(&client_id)
            .map(|session| session.expiring.is_some())
    }

    /// Whether `secret` is the reconnect secret issued with this Client ID's Session, compared in
    /// constant time against the salted hash the Session keeps. An unknown Client ID is checked
    /// against a fixed dummy credential — one HMAC verify either way, though the registry lookup
    /// itself is not constant time — and gives `false`, like a wrong secret.
    #[must_use]
    pub fn verify_reconnect(&self, client_id: ClientId, secret: &ReconnectSecret) -> bool {
        match self.sessions.get(&client_id) {
            Some(session) => session.credential.verify(secret),
            None => {
                // The result is discarded — an unknown Client ID never verifies — but kept opaque to
                // the optimizer so the dummy verify is not elided.
                let _ = core::hint::black_box(
                    Credential::dummy().verify(core::hint::black_box(secret)),
                );
                false
            }
        }
    }

    /// Attach `connection` to the Session with this Client ID when `secret` is its reconnect
    /// secret (Story 5.6), and return whether it did.
    ///
    /// The secret is verified with the Session's credential in constant time, and the Connection
    /// is inserted under the same shard lock, so whatever removes the Session — a racing
    /// [`detach`](Self::detach) of its last Connection on a registry with no TTL, or
    /// [`expire`](Self::expire) — either removes it first — this returns `false` — or finds the
    /// new attachment and leaves the Session live. An unknown Client ID (never issued,
    /// or already torn down) is checked against the fixed dummy credential, as in
    /// [`verify_reconnect`](Self::verify_reconnect), and gives `false` like a wrong secret: one
    /// HMAC verify per attempt either way. When this returns `false`, nothing changes.
    ///
    /// A reconnect to a Session whose TTL countdown runs (Story 5.8) stops it in the same shard
    /// lock as the verify; a wrong secret leaves it running. A Session already expired is gone,
    /// and so denied like an unknown Client ID.
    ///
    /// What is identical between the two denials is the outcome, and so the reply; the timing is
    /// not guaranteed to be — a live Client ID takes its shard's write lock and may wait on it —
    /// which is no oracle worth having, as a Client ID is 128 random bits.
    ///
    /// Precondition, not checked: as for [`attach`](Self::attach).
    #[must_use]
    pub fn reconnect(
        &self,
        client_id: ClientId,
        secret: &ReconnectSecret,
        connection: ConnectionId,
    ) -> bool {
        match self.sessions.get_mut(&client_id) {
            Some(mut session) => {
                if session.credential.verify(secret) {
                    session.connections.insert(connection);
                    session.expiring = None;
                    true
                } else {
                    false
                }
            }
            None => {
                // As in `verify_reconnect`: discarded, but kept opaque so it is not elided.
                let _ = core::hint::black_box(
                    Credential::dummy().verify(core::hint::black_box(secret)),
                );
                false
            }
        }
    }

    /// Whether a Session with this Client ID is live.
    #[must_use]
    pub fn contains(&self, client_id: ClientId) -> bool {
        self.sessions.contains_key(&client_id)
    }

    /// Replace the Session's Config with `config` — the whole of it, not a patch: a setting
    /// `config` leaves unset is the Daemon default from now on (Story 3.8). The replacement
    /// happens under the Session's shard lock, so an execution dispatched on any attached
    /// Connection sees either the old Config or the new one, never a mix; one already dispatched
    /// keeps the settings it read. Returns whether a Session has this Client ID; when none does,
    /// nothing changes.
    #[must_use]
    pub fn update_config(&self, client_id: ClientId, config: Config) -> bool {
        match self.sessions.get_mut(&client_id) {
            Some(mut session) => {
                session.config = config;
                true
            }
            None => false,
        }
    }

    /// How many Connections the Session has attached; `None` when it does not exist.
    #[must_use]
    pub fn attached(&self, client_id: ClientId) -> Option<usize> {
        self.sessions.get(&client_id).map(|s| s.connections.len())
    }

    /// The Session's Registered Functions with their grants, in registration order, as they are
    /// now; `None` when it does not exist. A copy for one execution to decide its host calls from
    /// — read afresh per execution, never cached (AD-5).
    #[must_use]
    pub fn registrations(&self, client_id: ClientId) -> Option<Vec<RegisteredFunction>> {
        self.sessions
            .get(&client_id)
            .map(|session| session.registrations.clone())
    }

    /// The execution limits the Session's Config sets, as they are now; `None` when it does not
    /// exist. A copy for one execution — read afresh per execution, never cached (AD-5).
    #[must_use]
    pub fn settings(&self, client_id: ClientId) -> Option<Settings> {
        self.sessions
            .get(&client_id)
            .map(|session| session.config.settings())
    }

    /// What one execution is dispatched with, read together under one lock: the Session's
    /// Registered Functions ([`registrations`](Self::registrations)) and its Config's execution
    /// limits ([`settings`](Self::settings)), as they are now; `None` when the Session does not
    /// exist.
    #[must_use]
    pub fn for_execution(
        &self,
        client_id: ClientId,
    ) -> Option<(Vec<RegisteredFunction>, Settings)> {
        self.sessions
            .get(&client_id)
            .map(|session| (session.registrations.clone(), session.config.settings()))
    }

    /// Reserve the Plugin name `name` in the Session with this Client ID (Story 6.2), under a
    /// freshly issued [`PluginId`], and return the id.
    ///
    /// The check that no Plugin of this Session already has exactly this name (bytes compared,
    /// no case folding or trimming) and the reservation happen under one shard lock, so of two
    /// concurrent registrations of one name exactly one succeeds. Only this Session's names are
    /// consulted. An id is issued only for a reservation that succeeds; ids start at 1 and are
    /// never reused during the registry's life.
    ///
    /// # Errors
    ///
    /// [`PluginNameError::Taken`] when the Session already holds the name, and
    /// [`PluginNameError::NoSession`] when no Session has this Client ID. Either way nothing
    /// changes.
    pub fn register_plugin(
        &self,
        client_id: ClientId,
        name: &str,
    ) -> Result<PluginId, PluginNameError> {
        let mut session = self
            .sessions
            .get_mut(&client_id)
            .ok_or(PluginNameError::NoSession)?;
        if session.plugins.contains_key(name) {
            return Err(PluginNameError::Taken);
        }
        // Uniqueness is all that is needed; the shard lock orders the Session's own state.
        let id = PluginId::new(self.last_plugin.fetch_add(1, Ordering::Relaxed) + 1);
        session.plugins.insert(Arc::from(name), id);
        Ok(id)
    }

    /// Release the Plugin name reserved under `id` in the Session with this Client ID (Story 6.4):
    /// what a registration whose initialization failed calls, so the name is free for a fresh
    /// registration. Returns whether a name was released.
    ///
    /// A name is released only while it still maps to `id`, under the Session's shard lock, so a
    /// release never frees a name another registration holds. The id itself is never handed out
    /// again. `false`, changing nothing, when no name of the Session maps to `id` or no Session
    /// has this Client ID — one already torn down released every name with it.
    pub fn release_plugin(&self, client_id: ClientId, id: PluginId) -> bool {
        let Some(mut session) = self.sessions.get_mut(&client_id) else {
            return false;
        };
        let before = session.plugins.len();
        session.plugins.retain(|_, held| *held != id);
        let released = session.plugins.len() != before;
        drop(session);
        // A registration that never became ready holds no store in the registry; one that did and
        // is released anyway loses it here (AD-4).
        if released && let Some(stores) = &self.global_variables {
            stores.teardown(id);
        }
        released
    }

    /// The Session's Plugins as `(name, id)` pairs, sorted by id (so in registration order);
    /// `None` when the Session does not exist, an empty list when it holds none.
    ///
    /// A name is listed from the moment it is reserved, which is before its initialization
    /// handlers run and its parsed Plugin is stored in `hexput-plugin`'s registry (Story 6.4): a
    /// listed id may have no body yet, and a reader of both (Story 6.5's firing) must treat such
    /// an id as not ready. A registration whose initialization fails releases its name
    /// ([`release_plugin`](Self::release_plugin)).
    #[must_use]
    pub fn plugins(&self, client_id: ClientId) -> Option<Vec<(Arc<str>, PluginId)>> {
        self.sessions.get(&client_id).map(|session| {
            let mut listed: Vec<(Arc<str>, PluginId)> = session
                .plugins
                .iter()
                .map(|(name, id)| (Arc::clone(name), *id))
                .collect();
            listed.sort_unstable_by_key(|(_, id)| *id);
            listed
        })
    }

    /// How many Sessions are live.
    #[must_use]
    pub fn len(&self) -> usize {
        self.sessions.len()
    }

    /// Whether no Session is live.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.sessions.is_empty()
    }
}

/// A fresh Client ID from the OS CSPRNG.
fn generate_client_id() -> ClientId {
    let mut bytes = [0_u8; 16];
    // No Client ID may be issued without the OS CSPRNG; see `Sessions::create`.
    getrandom::fill(&mut bytes).expect("the OS CSPRNG is unavailable");
    ClientId::from_bytes(bytes)
}

/// The one teardown path for a Session, called only by [`Sessions::detach`] (no TTL) or
/// [`Sessions::expire`] (after the TTL, Story 5.8) once the Session is already out of the
/// registry — never from a `Drop` impl, never under a lock.
///
/// It releases the Config, the reconnect credential, the Registered Functions and the Session's
/// Plugin names (Story 6.2). The parsed Plugins themselves are released by the hook, which the
/// Daemon wires to `hexput-plugin`. Story 5.8's TTL delays the call to this function, not moves
/// it — and the teardown hook with it.
///
/// `stores.teardown(plugin_id)` runs for each of the Session's Plugin ids (Story 6.7, AD-4) — the
/// stores' only lifecycle exit — before anything else is released; then the hook, when the
/// registry has one, is called with the Session's Client ID (Story 4.3); then the stores'
/// teardown runs once more, since only the hook closes the Plugin registry to a registration
/// still finishing (a teardown of an id with no store does nothing).
fn teardown(
    client_id: ClientId,
    session: Session,
    hook: Option<&(dyn Fn(ClientId) + Send + Sync)>,
    stores: Option<&dyn Teardown>,
) {
    let Session {
        config,
        credential,
        registrations,
        connections,
        plugins,
        expiring: _,
    } = session;
    debug_assert!(connections.is_empty(), "a Session is torn down unattached");
    let ids: Vec<PluginId> = plugins.values().copied().collect();
    let release_stores = || {
        if let Some(stores) = stores {
            for &id in &ids {
                stores.teardown(id);
            }
        }
    };
    // The stores go first, before anything of the Session's Plugins drops (AD-4)...
    release_stores();
    drop((config, credential, registrations, plugins));
    if let Some(hook) = hook {
        hook(client_id);
    }
    // ...and once more after the hook closed the Plugin registry: a registration that was
    // finishing as the Session ended may have joined its store between the two, and none can
    // join after.
    release_stores();
}
