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
//! [`Sessions`] is the registry. [`Sessions::create`] turns a decoded [`InitRequest`] into a
//! Session keyed by a freshly issued [`ClientId`], born with its creating Connection attached.
//! A Session holds a *set* of attached Connections — zero or more, never "exactly one" (AD-2).
//! [`Sessions::detach`] removes one; detaching the last removes the Session from the registry in
//! the same step and then tears it down through the one explicit teardown path, which Story 5.8
//! will delay behind a TTL rather than relocate. Nothing here is `async`, so no lock of this
//! crate's can be held across an `.await`; nothing here writes to disk.
//!
//! Binds: AD-2, AD-4, AD-5.

mod init;

use std::collections::HashSet;
use std::sync::atomic::{AtomicU64, Ordering};

use dashmap::DashMap;
use dashmap::mapref::entry::Entry;
pub use hexput_shared::ids::ClientId;

pub use init::{Config, InitError, InitRequest, RegisteredFunction};

/// One Connection's attachment to a Session. Local to this Daemon run and never on the wire,
/// so it is not a `hexput-shared` id: two attachments of the same Connection never exist, and
/// no two attachments in one run share a value.
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
/// and the Connections attached to it right now.
#[derive(Debug)]
pub struct Session {
    config: Config,
    registrations: Vec<RegisteredFunction>,
    connections: HashSet<ConnectionId>,
}

/// The registry of live Sessions, keyed by Client ID. One per Daemon run, shared by every
/// Connection.
///
/// Every operation takes one `DashMap` shard lock synchronously and releases it before
/// returning; none is `async`.
#[derive(Debug, Default)]
pub struct Sessions {
    sessions: DashMap<ClientId, Session>,
    next_connection: AtomicU64,
}

impl Sessions {
    /// An empty registry.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Create a Session from a decoded `Init`, with one new Connection attached: the Connection
    /// that sent it. Returns the Session's newly issued Client ID and that attachment.
    ///
    /// The Client ID is 128 bits from the OS CSPRNG. A collision with a live Session draws again
    /// rather than overwriting it.
    ///
    /// # Panics
    ///
    /// If the OS CSPRNG cannot be read. A Daemon that cannot issue unguessable identifiers must
    /// not issue guessable ones; the panic stays within the calling Connection's task.
    #[must_use]
    pub fn create(&self, init: InitRequest) -> (ClientId, ConnectionId) {
        let connection = self.next_connection_id();
        let session = Session {
            config: init.config,
            registrations: init.registrations,
            connections: HashSet::from([connection]),
        };
        loop {
            // The entry holds its shard's lock, so no racing `create` can claim the same id
            // between the check and the insert.
            if let Entry::Vacant(vacant) = self.sessions.entry(generate_client_id()) {
                let client_id = *vacant.key();
                vacant.insert(session);
                return (client_id, connection);
            }
        }
    }

    /// Attach one more Connection to a live Session. `None` when no Session has this Client ID.
    ///
    /// Nothing on the wire reaches this before reconnect (Epic 5); it exists so a Session with
    /// several attachments is a state the registry supports from the start (AD-2).
    #[must_use]
    pub fn attach(&self, client_id: ClientId) -> Option<ConnectionId> {
        let mut session = self.sessions.get_mut(&client_id)?;
        let connection = self.next_connection_id();
        session.connections.insert(connection);
        Some(connection)
    }

    /// Detach a Connection from its Session. When it was the last one attached, the Session is
    /// removed from the registry under the same shard lock as the detach — so no racing
    /// [`attach`](Self::attach) can find a Session on its way out — and then torn down, outside
    /// any lock.
    ///
    /// Detaching a Connection that is not attached, or from a Session that no longer exists, does
    /// nothing.
    pub fn detach(&self, client_id: ClientId, connection: ConnectionId) {
        let removed = self.sessions.remove_if_mut(&client_id, |_, session| {
            session.connections.remove(&connection) && session.connections.is_empty()
        });
        if let Some((_, session)) = removed {
            teardown(session);
        }
    }

    /// Whether a Session with this Client ID is live.
    #[must_use]
    pub fn contains(&self, client_id: ClientId) -> bool {
        self.sessions.contains_key(&client_id)
    }

    /// How many Connections the Session has attached; `None` when it does not exist.
    #[must_use]
    pub fn attached(&self, client_id: ClientId) -> Option<usize> {
        self.sessions.get(&client_id).map(|s| s.connections.len())
    }

    /// The names of the Session's Registered Functions, in registration order; `None` when it
    /// does not exist.
    #[must_use]
    pub fn registration_names(&self, client_id: ClientId) -> Option<Vec<String>> {
        self.sessions.get(&client_id).map(|session| {
            session
                .registrations
                .iter()
                .map(|r| r.name().to_owned())
                .collect()
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

    fn next_connection_id(&self) -> ConnectionId {
        // Uniqueness is all that is needed; no other memory is ordered by this counter.
        ConnectionId(self.next_connection.fetch_add(1, Ordering::Relaxed))
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
/// Today it releases the Config and the Registered Functions. When Plugins land (Epic 6), each
/// of the Session's Plugins has its Global Variable store torn down here through
/// `hexput_globalvar::teardown(plugin_id)` before its actor is dropped (AD-4); when TTLs land
/// (Story 5.8), the call to this function is delayed, not moved.
fn teardown(session: Session) {
    let Session {
        config,
        registrations,
        connections,
    } = session;
    debug_assert!(connections.is_empty(), "a Session is torn down unattached");
    drop((config, registrations));
}
