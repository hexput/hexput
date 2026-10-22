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
//! [`Sessions::detach`] removes one; detaching the last removes the Session from the registry in
//! the same step and then tears it down through the one explicit teardown path, which Story 5.8
//! will delay behind a TTL rather than relocate.
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
//! Session's cached Scripts go with it without this crate reaching `hexput-script`. When Story
//! 5.8 delays teardown behind a TTL, the hook is delayed with it.
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
//! when it matches, inserts the Connection under the same shard lock — so a racing
//! [`Sessions::detach`] either tears the Session down first (and the reconnect is denied) or sees
//! the new attachment. An unknown Client ID runs the same dummy verify as
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
//! removes one Connection alone; the Session goes only with the last one, in any detach order.
//!
//! Nothing here is `async`, so no lock of this crate's can be held across an `.await`; nothing
//! here writes to disk.
//!
//! Binds: AD-2, AD-4, AD-5, FR-13.

mod credential;
mod init;
mod reconnect;

use std::collections::HashSet;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use dashmap::DashMap;
use dashmap::mapref::entry::Entry;
pub use hexput_shared::ids::ClientId;

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
}

/// The registry of live Sessions, keyed by Client ID. One per Daemon run, shared by every
/// Connection.
///
/// Every operation takes one `DashMap` shard lock synchronously and releases it before
/// returning; none is `async`.
#[derive(Default)]
pub struct Sessions {
    sessions: DashMap<ClientId, Session>,
    next_connection: AtomicU64,
    /// Called with a Session's Client ID when it is torn down (Story 4.3), if set.
    on_teardown: Option<TeardownHook>,
}

/// The function [`Sessions::with_teardown`] stores.
type TeardownHook = Arc<dyn Fn(ClientId) + Send + Sync + 'static>;

impl core::fmt::Debug for Sessions {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Sessions")
            .field("sessions", &self.sessions)
            .field("next_connection", &self.next_connection)
            .field("on_teardown", &self.on_teardown.is_some())
            .finish()
    }
}

impl Sessions {
    /// An empty registry, with no teardown hook.
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
    #[must_use]
    pub fn attach(&self, client_id: ClientId, connection: ConnectionId) -> bool {
        match self.sessions.get_mut(&client_id) {
            Some(mut session) => {
                session.connections.insert(connection);
                true
            }
            None => false,
        }
    }

    /// Detach a Connection from its Session. When it was the last one attached, the Session is
    /// removed from the registry under the same shard lock as the detach — so no racing
    /// [`attach`](Self::attach) or [`reconnect`](Self::reconnect) can find a Session on its way
    /// out — and then torn down, outside any lock.
    ///
    /// Detaching a Connection that is not attached, or from a Session that no longer exists, does
    /// nothing.
    pub fn detach(&self, client_id: ClientId, connection: ConnectionId) {
        let removed = self.sessions.remove_if_mut(&client_id, |_, session| {
            session.connections.remove(&connection) && session.connections.is_empty()
        });
        if let Some((client_id, session)) = removed {
            teardown(client_id, session, self.on_teardown.as_deref());
        }
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
    /// is inserted under the same shard lock, so a racing [`detach`](Self::detach) of the
    /// Session's last Connection either removes the Session first — this returns `false` — or
    /// finds the new attachment and leaves the Session live. An unknown Client ID (never issued,
    /// or already torn down) is checked against the fixed dummy credential, as in
    /// [`verify_reconnect`](Self::verify_reconnect), and gives `false` like a wrong secret: one
    /// HMAC verify per attempt either way. When this returns `false`, nothing changes.
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

/// The one teardown path for a Session, called only by [`Sessions::detach`] once the Session is
/// already out of the registry — never from a `Drop` impl, never under a lock.
///
/// Today it releases the Config, the reconnect credential and the Registered Functions. When
/// Plugins land (Epic 6), each of the Session's Plugins has its Global Variable store torn down
/// here through `hexput_globalvar::teardown(plugin_id)` before its actor is dropped (AD-4); when
/// TTLs land (Story 5.8), the call to this function is delayed, not moved — and the teardown hook
/// with it.
///
/// The hook, when the registry has one, is called last, with the Session's Client ID (Story 4.3).
fn teardown(
    client_id: ClientId,
    session: Session,
    hook: Option<&(dyn Fn(ClientId) + Send + Sync)>,
) {
    let Session {
        config,
        credential,
        registrations,
        connections,
    } = session;
    debug_assert!(connections.is_empty(), "a Session is torn down unattached");
    drop((config, credential, registrations));
    if let Some(hook) = hook {
        hook(client_id);
    }
}
