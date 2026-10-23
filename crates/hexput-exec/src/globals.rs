//! A Plugin's Global Variables (Story 6.7): what the store keeps per lock unit, and the
//! interpreter's [`Globals`] implemented over `hexput-globalvar` for one invocation.
//!
//! A Global holding an object is kept as one unit per top-level property plus its whole-variable
//! unit, which holds the property order and the object's own secret; any other value is kept
//! whole in the whole-variable unit. Each invocation remembers the version of every unit its copy
//! reflects, so a read only brings over what another invocation changed since.

use std::collections::HashMap;
use std::sync::Arc;

use hexput_globalvar::Variable;
use hexput_interpreter::{Changes, Globals, Held, Object, Secret, Value};

/// The value of one lock unit of a Plugin's Global Variable store.
#[derive(Debug, Clone)]
pub struct Stored(Kind);

#[derive(Debug, Clone)]
enum Kind {
    /// A whole variable that holds no object, or one property of a variable holding an object.
    Value(Held),
    /// The whole-variable unit of a variable holding an object: its properties' order and its
    /// own secret. Each property is a unit of its own.
    Object {
        keys: Vec<Arc<str>>,
        secret: Option<Secret>,
    },
}

/// One Plugin's Global Variable store, as the Executor keeps it.
pub type GlobalStore = hexput_globalvar::PluginStore<Stored>;

/// One Daemon run's Global Variable stores, as the Executor keeps them.
pub type GlobalVariables = hexput_globalvar::GlobalVariables<Stored>;

/// The unit versions an invocation's copy of one variable reflects.
#[derive(Debug, Default)]
struct Synced {
    whole: Option<u64>,
    properties: HashMap<Arc<str>, u64>,
}

/// The interpreter's view of one Plugin's store, for one invocation.
pub(crate) struct PluginGlobals {
    store: Arc<GlobalStore>,
    synced: HashMap<Arc<str>, Synced>,
}

impl PluginGlobals {
    pub(crate) fn new(store: Arc<GlobalStore>) -> Self {
        Self {
            store,
            synced: HashMap::new(),
        }
    }
}

/// Write `held` as the whole variable `variable`, decomposing an object into its units and
/// dropping the units of properties it no longer has. Returns the versions written.
fn write_whole(variable: &mut Variable<'_, Stored>, held: Held) -> Synced {
    let old_keys = match variable.get(None) {
        Some((Stored(Kind::Object { keys, .. }), _)) => keys,
        _ => Vec::new(),
    };
    let mut synced = Synced::default();
    match &held.value {
        Value::Object(object) => {
            let mut keys = Vec::with_capacity(object.len());
            for (key, value, secret) in object.iter_with_secrets() {
                let key: Arc<str> = Arc::from(key);
                let secret = secret.filter(|_| !value.is_collection()).cloned();
                let version = variable.set(
                    Some(&key),
                    Stored(Kind::Value(Held::new(value.clone(), secret))),
                );
                synced.properties.insert(Arc::clone(&key), version);
                keys.push(key);
            }
            for old in old_keys {
                if !keys.contains(&old) {
                    variable.remove(Some(&old));
                }
            }
            let secret = object.secret().cloned();
            synced.whole = Some(variable.set(None, Stored(Kind::Object { keys, secret })));
        }
        _ => {
            for old in old_keys {
                variable.remove(Some(&old));
            }
            synced.whole = Some(variable.set(None, Stored(Kind::Value(held))));
        }
    }
    synced
}

impl Globals for PluginGlobals {
    fn current(&mut self, name: &str) -> Option<Held> {
        let (held, synced) = self.store.view(name, |view| {
            let (Stored(marker), whole) = view.get(None)?;
            let mut synced = Synced {
                whole: Some(whole),
                properties: HashMap::new(),
            };
            let held = match marker {
                Kind::Value(held) => held,
                Kind::Object { keys, secret } => {
                    let mut entries = Vec::with_capacity(keys.len());
                    for key in keys {
                        if let Some((Stored(Kind::Value(held)), version)) = view.get(Some(&key)) {
                            synced.properties.insert(Arc::clone(&key), version);
                            entries.push((key, held));
                        }
                    }
                    Held::plain(Value::Object(Object::from_held_entries(entries, secret)))
                }
            };
            Some((held, synced))
        })?;
        self.synced.insert(Arc::from(name), synced);
        Some(held)
    }

    fn changes(&mut self, name: &str) -> Option<Changes> {
        let synced = self.synced.get(name);
        let known = synced.and_then(|synced| synced.whole);
        let changed = self.store.view(name, |view| {
            let (Stored(marker), whole) = view.get(None)?;
            if known != Some(whole) {
                return Some(None);
            }
            let Kind::Object { keys, .. } = marker else {
                return Some(Some(Vec::new()));
            };
            let mut changed = Vec::new();
            for key in keys {
                let seen = synced.and_then(|synced| synced.properties.get(&key));
                if let Some((Stored(Kind::Value(held)), version)) = view.get(Some(&key))
                    && seen != Some(&version)
                {
                    changed.push((key, held, version));
                }
            }
            Some(Some(changed))
        })?;
        match changed {
            // The variable was replaced, or never seen: bring it over whole.
            None => self.current(name).map(Changes::Whole),
            Some(changed) if changed.is_empty() => None,
            Some(changed) => {
                let synced = self.synced.entry(Arc::from(name)).or_default();
                let properties = changed
                    .into_iter()
                    .map(|(key, held, version)| {
                        synced.properties.insert(Arc::clone(&key), version);
                        (key, held)
                    })
                    .collect();
                Some(Changes::Properties(properties))
            }
        }
    }

    fn seed(&mut self, name: &str, value: Held) {
        let synced = self.store.update(name, |variable| {
            variable
                .get(None)
                .is_none()
                .then(|| write_whole(variable, value))
        });
        if let Some(synced) = synced {
            self.synced.insert(Arc::from(name), synced);
        }
    }

    fn write(&mut self, name: &str, key: Option<&str>, value: Held) {
        let Some(key) = key else {
            let synced = self
                .store
                .update(name, |variable| write_whole(variable, value));
            self.synced.insert(Arc::from(name), synced);
            return;
        };
        let key: Arc<str> = Arc::from(key);
        let known = self.synced.get(name).and_then(|synced| synced.whole);
        let written = self.store.update(name, |variable| {
            // Another invocation replaced the variable with one holding no object meanwhile: the
            // last whole write wins, and this property write is dropped.
            let Some((Stored(Kind::Object { mut keys, secret }), whole)) = variable.get(None)
            else {
                return None;
            };
            let marker = if keys.contains(&key) {
                None
            } else {
                keys.push(Arc::clone(&key));
                Some((
                    whole,
                    variable.set(None, Stored(Kind::Object { keys, secret })),
                ))
            };
            let version = variable.set(Some(&key), Stored(Kind::Value(value)));
            Some((marker, version))
        });
        let Some((marker, version)) = written else {
            return;
        };
        let synced = self.synced.entry(Arc::from(name)).or_default();
        synced.properties.insert(key, version);
        // The copy took the new property too; it reflects the new order only if it reflected
        // the one just replaced.
        if let Some((before, after)) = marker
            && known == Some(before)
        {
            synced.whole = Some(after);
        }
    }

    fn begin_update(&mut self, name: &str, key: Option<&str>) -> Box<dyn Send> {
        // No look at what the variable holds first: a property intent overlaps the whole
        // variable's, so it excludes a concurrent whole write whatever kind the variable has by
        // the time it is taken — and a property write to a variable holding no object fails or
        // is dropped, so it never needs the whole unit.
        Box::new(self.store.begin_update(name, key))
    }
}
