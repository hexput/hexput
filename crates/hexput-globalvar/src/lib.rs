//! Global Variable store: a shared concurrent per-Plugin map (dashmap), independent of and
//! reachable without the Plugin actor being alive. Depends on hexput-shared only — never
//! hexput-plugin or hexput-rpc — so the store cannot be reached through RPC and stays live
//! across the actor's mailbox lifecycle.
//! Exposes teardown(plugin_id) as its sole store-lifecycle entry point, called only by
//! hexput-session before the Plugin actor is dropped.
//!
//! # The store (Story 6.7)
//!
//! [`GlobalVariables`] is one Daemon run's registry: a [`PluginStore`] per Plugin, keyed by its
//! [`PluginId`], created once when the Plugin registers ([`GlobalVariables::create`]) and removed
//! only by [`GlobalVariables::teardown`] — never by `Drop` (AD-4). A store already fetched by a
//! running invocation stays usable by it after teardown; the registry simply no longer names it.
//!
//! A store holds one value per **lock unit** `(variable, key)`: `key` is `None` for a whole
//! variable and `Some(property)` for one top-level property of a variable holding an object. The
//! store is generic over the value it holds — the Executor decides what a unit's value is — and
//! knows nothing about the language.
//!
//! Every operation is a short synchronous critical section. Nothing here waits on anything but
//! another critical section, with one exception: [`PluginStore::begin_update`] takes a unit's
//! **writer intent**, which a second `begin_update` of an overlapping unit waits on until the
//! first [`Intent`] drops. A property unit `(name, Some(key))` overlaps only itself and its
//! variable's whole unit `(name, None)`; a whole unit overlaps every unit of its variable, since a
//! whole write rewrites them all. Reads never take an intent, so a held intent stalls nobody but
//! the next writer of an overlapping unit. Each unit write carries a version from one counter per
//! store, so a reader can tell which units changed since it last looked.
//!
//! Binds: AD-3, AD-4.

use std::collections::HashSet;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, PoisonError};

use dashmap::DashMap;
use hexput_shared::ids::PluginId;

/// One lock unit: a variable, and the top-level property of it (`None` for the whole variable).
type Unit = (Arc<str>, Option<Arc<str>>);

/// The unit `(name, key)` as a map key.
fn unit(name: &str, key: Option<&str>) -> Unit {
    (Arc::from(name), key.map(Arc::from))
}

/// One Daemon run's Global Variable stores, one per Plugin.
#[derive(Debug)]
pub struct GlobalVariables<V> {
    stores: DashMap<PluginId, Arc<PluginStore<V>>>,
}

impl<V> Default for GlobalVariables<V> {
    fn default() -> Self {
        Self {
            stores: DashMap::new(),
        }
    }
}

impl<V> GlobalVariables<V> {
    /// A registry holding no store.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Create the empty store of the Plugin `plugin_id` and return it. A store the registry
    /// already held under the id is replaced (ids are never reused within a run, so this does
    /// not happen in the Daemon).
    pub fn create(&self, plugin_id: PluginId) -> Arc<PluginStore<V>> {
        let store = Arc::new(PluginStore::new());
        self.stores.insert(plugin_id, Arc::clone(&store));
        store
    }

    /// Hold `store` as the Plugin `plugin_id`'s — one made with [`PluginStore::new`] before the
    /// Plugin was ready, and seeded while its registration completed. A store the registry already
    /// held under the id is replaced.
    pub fn insert(&self, plugin_id: PluginId, store: Arc<PluginStore<V>>) {
        self.stores.insert(plugin_id, store);
    }

    /// The store of the Plugin `plugin_id`, while it exists.
    #[must_use]
    pub fn get(&self, plugin_id: PluginId) -> Option<Arc<PluginStore<V>>> {
        self.stores
            .get(&plugin_id)
            .map(|store| Arc::clone(store.value()))
    }

    /// Remove the store of the Plugin `plugin_id`: the store's sole lifecycle exit (AD-4),
    /// called by `hexput-session` when the Plugin's Session is torn down or its registration is
    /// released. Removing an id with no store does nothing. An invocation still holding the store
    /// finishes against it; nothing reaches it through the registry again.
    pub fn teardown(&self, plugin_id: PluginId) {
        self.stores.remove(&plugin_id);
    }
}

/// What `hexput-session` holds to tear stores down, whatever value they hold.
pub trait Teardown: Send + Sync {
    /// Remove the store of the Plugin `plugin_id` (see [`GlobalVariables::teardown`]).
    fn teardown(&self, plugin_id: PluginId);
}

impl<V: Send + Sync> Teardown for GlobalVariables<V> {
    fn teardown(&self, plugin_id: PluginId) {
        Self::teardown(self, plugin_id);
    }
}

/// A unit's value and the version its last write was given.
#[derive(Debug)]
struct Slot<V> {
    value: V,
    version: u64,
}

/// One Plugin's Global Variables, by lock unit.
#[derive(Debug)]
pub struct PluginStore<V> {
    units: DashMap<Unit, Slot<V>>,
    /// Each variable's gate: held, briefly, by a write — and by a read of several units — so a
    /// write touching several of a variable's units is seen whole or not at all.
    variables: DashMap<Arc<str>, ()>,
    /// Each variable's writer intents. One entry per variable name, never per property, so the
    /// map does not grow with the keys ever written.
    intents: DashMap<Arc<str>, Arc<IntentLock>>,
    /// Where unit versions come from: every write takes the next one.
    versions: AtomicU64,
}

impl<V> Default for PluginStore<V> {
    fn default() -> Self {
        Self::new()
    }
}

impl<V> PluginStore<V> {
    /// An empty store, in no registry: a registration seeds it before its Plugin is ready, and
    /// [`GlobalVariables::insert`] makes it the Plugin's. Dropped unregistered, it never existed
    /// for anyone else.
    #[must_use]
    pub fn new() -> Self {
        Self {
            units: DashMap::new(),
            variables: DashMap::new(),
            intents: DashMap::new(),
            versions: AtomicU64::new(0),
        }
    }

    /// Replace the value of the unit `(name, key)` with `value`. Atomic: a concurrent read sees
    /// the old value or the new one, never anything else.
    pub fn write(&self, name: &str, key: Option<&str>, value: V) {
        self.update(name, |variable| {
            variable.set(key, value);
        });
    }

    /// Run `change` on the variable `name` with its gate held: every unit it sets or removes
    /// lands together, as far as [`PluginStore::view`] can tell. `change` must be short and must
    /// not touch this store through any other method.
    pub fn update<R>(&self, name: &str, change: impl FnOnce(&mut Variable<'_, V>) -> R) -> R {
        let gate = self.variables.entry(Arc::from(name)).or_default();
        let mut variable = Variable {
            store: self,
            name: Arc::clone(gate.key()),
        };
        let result = change(&mut variable);
        drop(gate);
        result
    }

    /// Take the writer intent of the unit `(name, key)`, waiting while another holder has an
    /// overlapping one — the same property, or the whole variable; for `key` `None`, any intent
    /// on the variable. It is released when the returned [`Intent`] drops. Reads never take it.
    pub fn begin_update(&self, name: &str, key: Option<&str>) -> Intent {
        // The shard lock is released before waiting: only the intent's own mutex is waited on.
        let lock = Arc::clone(self.intents.entry(Arc::from(name)).or_default().value());
        let key: Option<Arc<str>> = key.map(Arc::from);
        lock.acquire(key.as_ref());
        Intent { lock, key }
    }
}

impl<V: Clone> PluginStore<V> {
    /// The value of the unit `(name, key)`, if it has one.
    #[must_use]
    pub fn read(&self, name: &str, key: Option<&str>) -> Option<V> {
        self.units
            .get(&unit(name, key))
            .map(|slot| slot.value.clone())
    }

    /// Run `look` on the variable `name` with its gate held, so the units it reads are the ones
    /// of one moment. `look` must be short and must not touch this store through any other
    /// method.
    pub fn view<R>(&self, name: &str, look: impl FnOnce(&View<'_, V>) -> R) -> R {
        let gate = self.variables.entry(Arc::from(name)).or_default();
        let view = View {
            store: self,
            name: Arc::clone(gate.key()),
        };
        let result = look(&view);
        drop(gate);
        result
    }
}

/// The units of one variable, read with its gate held (see [`PluginStore::view`]).
pub struct View<'s, V> {
    store: &'s PluginStore<V>,
    name: Arc<str>,
}

impl<V: Clone> View<'_, V> {
    /// The value of the unit `key` and the version of its last write.
    #[must_use]
    pub fn get(&self, key: Option<&str>) -> Option<(V, u64)> {
        self.store
            .units
            .get(&(Arc::clone(&self.name), key.map(Arc::from)))
            .map(|slot| (slot.value.clone(), slot.version))
    }

    /// The version of the unit `key`'s last write.
    #[must_use]
    pub fn version(&self, key: Option<&str>) -> Option<u64> {
        self.store
            .units
            .get(&(Arc::clone(&self.name), key.map(Arc::from)))
            .map(|slot| slot.version)
    }
}

/// The units of one variable, written with its gate held (see [`PluginStore::update`]).
pub struct Variable<'s, V> {
    store: &'s PluginStore<V>,
    name: Arc<str>,
}

impl<V> Variable<'_, V> {
    /// Replace the value of the unit `key`; returns the version the write was given.
    pub fn set(&mut self, key: Option<&str>, value: V) -> u64 {
        let version = self.store.versions.fetch_add(1, Ordering::Relaxed) + 1;
        self.store.units.insert(
            (Arc::clone(&self.name), key.map(Arc::from)),
            Slot { value, version },
        );
        version
    }

    /// Remove the unit `key`.
    pub fn remove(&mut self, key: Option<&str>) {
        self.store
            .units
            .remove(&(Arc::clone(&self.name), key.map(Arc::from)));
    }
}

impl<V: Clone> Variable<'_, V> {
    /// The value of the unit `key` and the version of its last write.
    #[must_use]
    pub fn get(&self, key: Option<&str>) -> Option<(V, u64)> {
        self.store
            .units
            .get(&(Arc::clone(&self.name), key.map(Arc::from)))
            .map(|slot| (slot.value.clone(), slot.version))
    }
}

/// One variable's writer intents: the whole variable's, or any number of distinct properties'.
#[derive(Debug, Default)]
struct IntentLock {
    held: Mutex<Holders>,
    released: Condvar,
}

/// Who holds a variable's writer intents.
#[derive(Debug, Default)]
struct Holders {
    whole: bool,
    properties: HashSet<Arc<str>>,
}

impl Holders {
    /// Whether an intent on `key` (`None`: the whole variable) overlaps one already held.
    fn overlaps(&self, key: Option<&Arc<str>>) -> bool {
        self.whole
            || match key {
                None => !self.properties.is_empty(),
                Some(key) => self.properties.contains(key),
            }
    }
}

impl IntentLock {
    fn acquire(&self, key: Option<&Arc<str>>) {
        let mut held = self.held.lock().unwrap_or_else(PoisonError::into_inner);
        while held.overlaps(key) {
            held = self
                .released
                .wait(held)
                .unwrap_or_else(PoisonError::into_inner);
        }
        match key {
            None => held.whole = true,
            Some(key) => {
                held.properties.insert(Arc::clone(key));
            }
        }
    }

    fn release(&self, key: Option<&Arc<str>>) {
        let mut held = self.held.lock().unwrap_or_else(PoisonError::into_inner);
        match key {
            None => held.whole = false,
            Some(key) => {
                held.properties.remove(key);
            }
        }
        drop(held);
        // Waiters wait on different units: every one re-checks its own.
        self.released.notify_all();
    }
}

/// A held writer intent (see [`PluginStore::begin_update`]): released when dropped. It can move
/// between threads, as a suspended execution holding it does.
#[derive(Debug)]
#[must_use = "the intent is released as soon as it drops"]
pub struct Intent {
    lock: Arc<IntentLock>,
    key: Option<Arc<str>>,
}

impl Drop for Intent {
    fn drop(&mut self) {
        self.lock.release(self.key.as_ref());
    }
}
