//! The public Script result (LANGUAGE-REFERENCE §3), detached from the execution that made it.
//!
//! While a Script runs, its collections live in the execution's heap and are addressed by
//! handle (see `heap.rs`). When the Script returns, the result is copied out into these owned,
//! immutable types and the heap is dropped with everything in it. A [`Value`] therefore
//! references no execution state, is `Send + Sync`, and exposes no identity: collection
//! identity (`==` on arrays and objects) exists only inside the execution.
//!
//! Detached collections share storage through `Arc` where the execution's result reached the
//! same collection twice (`[x, x]`). That is unobservable — the result is immutable and has no
//! identity — so it reads exactly like separate copies while keeping detachment linear.
//!
//! `Value` deliberately has no `PartialEq`: structural equality is not language equality
//! (§4.2), and the result type should not suggest otherwise.
//!
//! # Value Secrets (Story 3.11)
//!
//! A value that crosses between the Script and the Backend may carry a hidden [`Secret`]
//! (LANGUAGE-REFERENCE §3): a Reference ID, an optional key and the Backend's further fields,
//! which the interpreter carries but never reads. Where it sits follows §3's rule that a Reference
//! ID names a *location*:
//!
//! * an array or object carries its own ([`Array::secret`], [`Object::secret`]), shared by
//!   identity wherever the collection is held;
//! * a string, number, bool or `null` carries it on the place that holds it — an element
//!   ([`Array::element_secret`]), a property ([`Object::entry_secret`]), or, at the top level (a
//!   starting variable, a host call's value or argument, the Script result), beside the value in a
//!   [`Held`].
//!
//! A Script can never observe any of it: no operation reads a secret, and the key `__secret` never
//! exists in its view of an object.

use core::fmt;
use std::collections::HashMap;
use std::sync::Arc;

use indexmap::IndexMap;

/// The key a Value Secret travels under, and which a Script can therefore never see (§3).
pub(crate) const SECRET_KEY: &str = "__secret";

/// One detached Hexput value.
///
/// There is deliberately no function variant: a function is a value *inside* an execution (§6)
/// but has no wire representation, so returning one is a `type` error (`type.function_result`)
/// rather than something a Backend could receive. `#[non_exhaustive]` leaves room for a later
/// re-triggerable callable handle to widen that error into a value without breaking callers.
#[derive(Clone)]
#[non_exhaustive]
pub enum Value {
    Null,
    Bool(bool),
    /// Always finite: every operation that would produce a non-finite result raises instead.
    Number(f64),
    String(Arc<str>),
    Array(Array),
    Object(Object),
}

impl Value {
    /// The Value Secret this value carries itself: an array's or object's own; `None` for every
    /// other kind, whose secret belongs to the place that holds it (see [`Held`]).
    #[must_use]
    pub fn secret(&self) -> Option<&Secret> {
        match self {
            Self::Array(array) => array.secret(),
            Self::Object(object) => object.secret(),
            _ => None,
        }
    }

    /// Whether this is an array or an object: a value that carries its own secret rather than
    /// taking its location's.
    #[must_use]
    pub const fn is_collection(&self) -> bool {
        matches!(self, Self::Array(_) | Self::Object(_))
    }

    /// The type name used in diagnostics: `null`, `bool`, `number`, `string`, `array`, `object`.
    #[must_use]
    pub const fn type_name(&self) -> &'static str {
        match self {
            Self::Null => "null",
            Self::Bool(_) => "bool",
            Self::Number(_) => "number",
            Self::String(_) => "string",
            Self::Array(_) => "array",
            Self::Object(_) => "object",
        }
    }

    /// §4.1 truthiness: `null`, `false`, `0`, `""`, and empty collections are falsy.
    #[must_use]
    pub fn is_truthy(&self) -> bool {
        match self {
            Self::Null => false,
            Self::Bool(b) => *b,
            Self::Number(n) => *n != 0.0,
            Self::String(s) => !s.is_empty(),
            Self::Array(a) => !a.is_empty(),
            Self::Object(o) => !o.is_empty(),
        }
    }

    #[must_use]
    pub const fn is_null(&self) -> bool {
        matches!(self, Self::Null)
    }

    #[must_use]
    pub const fn as_bool(&self) -> Option<bool> {
        match self {
            Self::Bool(b) => Some(*b),
            _ => None,
        }
    }

    #[must_use]
    pub const fn as_number(&self) -> Option<f64> {
        match self {
            Self::Number(n) => Some(*n),
            _ => None,
        }
    }

    #[must_use]
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Self::String(s) => Some(s),
            _ => None,
        }
    }

    #[must_use]
    pub const fn as_array(&self) -> Option<&Array> {
        match self {
            Self::Array(a) => Some(a),
            _ => None,
        }
    }

    #[must_use]
    pub const fn as_object(&self) -> Option<&Object> {
        match self {
            Self::Object(o) => Some(o),
            _ => None,
        }
    }
}

/// Shallow on purpose: collections print their length, never their contents, so formatting a
/// deeply nested value cannot recurse.
impl fmt::Debug for Value {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Null => f.write_str("Null"),
            Self::Bool(b) => write!(f, "Bool({b})"),
            Self::Number(n) => write!(f, "Number({n:?})"),
            Self::String(s) => write!(f, "String({s:?})"),
            Self::Array(a) => write!(f, "Array(len = {})", a.len()),
            Self::Object(o) => write!(f, "Object(len = {})", o.len()),
        }
    }
}

/// A detached, immutable, ordered array.
#[derive(Clone)]
pub struct Array(Arc<Elements>);

/// A detached, immutable, insertion-ordered object with string keys.
#[derive(Clone)]
pub struct Object(Arc<Entries>);

struct Elements {
    items: Vec<Value>,
    /// Each element's location secret, by position; empty when no element has one. Only ever
    /// `Some` for an element that is not itself a collection.
    secrets: Vec<Option<Secret>>,
    /// The array's own secret.
    secret: Option<Secret>,
}

struct Entries {
    entries: IndexMap<Arc<str>, Value>,
    /// Each property's location secret; only for a property whose value is not a collection.
    secrets: HashMap<Arc<str>, Secret>,
    /// The object's own secret.
    secret: Option<Secret>,
}

/// A Value Secret (LANGUAGE-REFERENCE §3, FR-28): the hidden metadata the Backend attaches to a
/// value — its Reference ID, an optional object key, and every further field it put there.
///
/// The interpreter carries a secret and never reads it: the further fields are an opaque blob
/// ([`Secret::extra`]) that the Executor encodes and decodes. No Script operation can reach any
/// part of it.
#[derive(Clone, PartialEq, Eq)]
pub struct Secret {
    reference: Arc<str>,
    key: Option<Arc<str>>,
    extra: Arc<[u8]>,
    /// Whether this execution generated it for a host call, rather than receiving it from the
    /// Backend. Never on the wire: it only keeps a generated secret out of the Script's result.
    generated: bool,
}

impl Secret {
    /// A secret with Reference ID `reference`, object key `key`, and `extra` — the Backend's
    /// further fields, encoded by whoever decoded them (empty for none).
    #[must_use]
    pub fn new(
        reference: impl Into<Arc<str>>,
        key: Option<Arc<str>>,
        extra: impl Into<Arc<[u8]>>,
    ) -> Self {
        Self {
            reference: reference.into(),
            key,
            extra: extra.into(),
            generated: false,
        }
    }

    /// A secret this execution generated for a value it sent to the host (Story 3.11): it
    /// travels in every later `Call`, and never in the Script's result.
    pub(crate) fn generated(reference: String) -> Self {
        Self {
            generated: true,
            ..Self::new(reference, None, Vec::new())
        }
    }

    /// Whether this execution generated the secret, rather than receiving it from the Backend. A
    /// generated secret is sent in `Call`s but never in the Script's result, so a Backend that
    /// supplies no secrets gets exactly the plain result it always did.
    #[must_use]
    pub const fn is_generated(&self) -> bool {
        self.generated
    }

    /// The Reference ID, `ref` on the wire.
    #[must_use]
    pub fn reference(&self) -> &str {
        &self.reference
    }

    /// The object key, `key` on the wire, when there is one.
    #[must_use]
    pub fn key(&self) -> Option<&str> {
        self.key.as_deref()
    }

    /// The object key, shared.
    pub(crate) fn shared_key(&self) -> Option<&Arc<str>> {
        self.key.as_ref()
    }

    /// The Reference ID, shared.
    pub(crate) const fn shared_reference(&self) -> &Arc<str> {
        &self.reference
    }

    /// The further fields, exactly as they were handed to [`Secret::new`].
    #[must_use]
    pub fn extra(&self) -> &[u8] {
        &self.extra
    }

    /// What holding this secret costs an execution's memory meter: the record and its three
    /// shared allocations' headers, plus their bytes.
    pub(crate) fn footprint(&self) -> usize {
        size_of::<Self>()
            + 6 * size_of::<usize>()
            + self.reference.len()
            + self.key.as_ref().map_or(0, |key| key.len())
            + self.extra.len()
    }
}

/// The Reference ID and key, and only the size of the further fields: a secret is the Backend's,
/// and a log line has no business spelling out what it put there.
impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Secret")
            .field("reference", &self.reference)
            .field("key", &self.key)
            .field("extra_bytes", &self.extra.len())
            .field("generated", &self.generated)
            .finish()
    }
}

/// A value together with the secret of the place it sits in — what crosses the boundary at the
/// top level: a starting variable, a host call's value, the Script result.
///
/// `secret` is the *location's* secret, meaningful only for a string, number, bool or `null`. An
/// array or object carries its own ([`Value::secret`]), and a location secret beside one is
/// ignored: [`Held::effective_secret`] is the one a position travels with.
#[derive(Clone, Debug)]
pub struct Held {
    pub value: Value,
    pub secret: Option<Secret>,
}

impl Held {
    /// `value` in a place with `secret`.
    #[must_use]
    pub const fn new(value: Value, secret: Option<Secret>) -> Self {
        Self { value, secret }
    }

    /// `value` in a place with no secret.
    #[must_use]
    pub const fn plain(value: Value) -> Self {
        Self {
            value,
            secret: None,
        }
    }

    /// The secret this position travels with: a collection's own, otherwise the location's.
    #[must_use]
    pub fn effective_secret(&self) -> Option<&Secret> {
        effective(&self.value, self.secret.as_ref())
    }
}

/// One modification of a referenced value (Story 3.13, LANGUAGE-REFERENCE §8): the Reference ID
/// it names and the whole new value.
///
/// Both directions use it. A host call's reply may carry some ([`crate::HostCall::resume_with`]):
/// a Reference ID naming an array or object replaces that collection's contents in place, keeping
/// its identity and its own secret; one naming the place a string, number, bool or `null` sits in
/// replaces the value there, and the place keeps its secret. A finished Script reports the
/// referenced places and collections it wrote ([`Finished::modifications`]), each once, with its
/// final value.
///
/// `value` is the value itself, never a holder: the Reference ID names the place, and a secret
/// beside a top-level array or object here is ignored inbound. Everything nested inside it
/// carries its secrets as usual.
#[derive(Clone, Debug)]
pub struct Modification {
    /// The Reference ID the modification names.
    pub reference: Arc<str>,
    /// The whole new value.
    pub value: Value,
}

impl Modification {
    /// A modification of whatever `reference` names to `value`.
    #[must_use]
    pub fn new(reference: impl Into<Arc<str>>, value: Value) -> Self {
        Self {
            reference: reference.into(),
            value,
        }
    }
}

/// How a Script ended (Story 3.13): its result, and every referenced place or collection it
/// wrote.
#[derive(Clone, Debug)]
pub struct Finished {
    /// The result, beside the secret of the place it was returned from when the `return` names a
    /// place that has one (Story 3.11).
    pub result: Held,
    /// Each Reference ID whose place or collection the Script wrote, once, in the order of its
    /// first write, with the value there when the Script ended. A write by a Backend's
    /// modification is never among them, nor a place or collection gone by the end (a block's
    /// variable after the block exited). Empty when the Script wrote none.
    pub modifications: Vec<Modification>,
}

impl From<Value> for Held {
    fn from(value: Value) -> Self {
        Self::plain(value)
    }
}

/// The secret a position holding `value` with location secret `location` travels with.
fn effective<'a>(value: &'a Value, location: Option<&'a Secret>) -> Option<&'a Secret> {
    if value.is_collection() {
        value.secret()
    } else {
        location
    }
}

impl Array {
    pub(crate) fn new(items: Vec<Value>) -> Self {
        Self::from_parts(items, Vec::new(), None)
    }

    /// An array from its elements, their location secrets by position (empty for none; a secret
    /// beside a collection is dropped) and its own secret.
    pub(crate) fn from_parts(
        items: Vec<Value>,
        mut secrets: Vec<Option<Secret>>,
        secret: Option<Secret>,
    ) -> Self {
        secrets.truncate(items.len());
        for (item, slot) in items.iter().zip(secrets.iter_mut()) {
            if item.is_collection() {
                *slot = None;
            }
        }
        if secrets.iter().all(Option::is_none) {
            secrets = Vec::new();
        } else {
            secrets.resize(items.len(), None);
        }
        Self(Arc::new(Elements {
            items,
            secrets,
            secret,
        }))
    }

    /// Build a detached array whose elements sit in places with secrets, and which has its own
    /// `secret` — how a Backend's holders reach a Script (Story 3.11). A location secret beside
    /// an element that is itself an array or object is ignored: that element carries its own.
    #[must_use]
    pub fn from_held(items: Vec<Held>, secret: Option<Secret>) -> Self {
        let (items, secrets) = items
            .into_iter()
            .map(|held| (held.value, held.secret))
            .unzip();
        Self::from_parts(items, secrets, secret)
    }

    /// The array's own Value Secret.
    #[must_use]
    pub fn secret(&self) -> Option<&Secret> {
        self.0.secret.as_ref()
    }

    /// The secret of the place at `index`: `None` when there is none, or when the element is an
    /// array or object (which carries its own).
    #[must_use]
    pub fn element_secret(&self, index: usize) -> Option<&Secret> {
        self.0.secrets.get(index).and_then(Option::as_ref)
    }

    /// The elements, in order, each with the secret it travels with (see
    /// [`Held::effective_secret`]).
    pub fn iter_with_secrets(&self) -> impl ExactSizeIterator<Item = (&Value, Option<&Secret>)> {
        self.0
            .items
            .iter()
            .enumerate()
            .map(|(index, item)| (item, effective(item, self.element_secret(index))))
    }

    /// Build a detached array from its elements, in order.
    ///
    /// The way a caller outside this crate supplies an array as a starting variable
    /// ([`crate::evaluate_with_variables`]); the execution's own arrays are never built this way.
    #[must_use]
    pub fn from_values(items: Vec<Value>) -> Self {
        Self::new(items)
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.0.items.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.items.is_empty()
    }

    /// The element at `index`, if present.
    #[must_use]
    pub fn get(&self, index: usize) -> Option<&Value> {
        self.0.items.get(index)
    }

    /// The elements, in order.
    #[must_use]
    pub fn to_vec(&self) -> Vec<Value> {
        self.0.items.clone()
    }

    /// The elements, in order, borrowed — for a caller that walks a result without copying it.
    pub fn iter(&self) -> impl ExactSizeIterator<Item = &Value> {
        self.0.items.iter()
    }
}

impl Object {
    pub(crate) fn new(entries: IndexMap<Arc<str>, Value>) -> Self {
        Self::from_parts(entries, HashMap::new(), None)
    }

    /// An object from its entries, their location secrets (a secret beside a collection, or for
    /// a key the object does not have, is dropped) and its own secret. A `__secret` key is never
    /// part of an object (§3), so one here is dropped too.
    pub(crate) fn from_parts(
        mut entries: IndexMap<Arc<str>, Value>,
        mut secrets: HashMap<Arc<str>, Secret>,
        secret: Option<Secret>,
    ) -> Self {
        entries.shift_remove(SECRET_KEY);
        secrets.retain(|key, _| entries.get(key).is_some_and(|value| !value.is_collection()));
        Self(Arc::new(Entries {
            entries,
            secrets,
            secret,
        }))
    }

    /// Build a detached object whose entries sit in places with secrets, and which has its own
    /// `secret` — how a Backend's holders reach a Script (Story 3.11). Keys behave as in
    /// [`Object::from_entries`]; a location secret beside an array or object is ignored, as that
    /// value carries its own.
    #[must_use]
    pub fn from_held_entries<K: Into<Arc<str>>>(
        entries: impl IntoIterator<Item = (K, Held)>,
        secret: Option<Secret>,
    ) -> Self {
        let mut values = IndexMap::new();
        let mut secrets = HashMap::new();
        for (key, held) in entries {
            let key: Arc<str> = key.into();
            match held.secret {
                Some(location) => {
                    secrets.insert(Arc::clone(&key), location);
                }
                None => {
                    secrets.remove(&key);
                }
            }
            values.insert(key, held.value);
        }
        Self::from_parts(values, secrets, secret)
    }

    /// The object's own Value Secret.
    #[must_use]
    pub fn secret(&self) -> Option<&Secret> {
        self.0.secret.as_ref()
    }

    /// The secret of the place under `key`: `None` when there is none, or when the value there
    /// is an array or object (which carries its own).
    #[must_use]
    pub fn entry_secret(&self, key: &str) -> Option<&Secret> {
        self.0.secrets.get(key)
    }

    /// The entries in insertion order, each with the secret its value travels with (see
    /// [`Held::effective_secret`]).
    pub fn iter_with_secrets(
        &self,
    ) -> impl ExactSizeIterator<Item = (&str, &Value, Option<&Secret>)> {
        self.0
            .entries
            .iter()
            .map(|(key, value)| (&**key, value, effective(value, self.0.secrets.get(key))))
    }

    /// Build a detached object from its entries, which keep the order they are given in (§3).
    ///
    /// `indexmap` stays out of the signature: insertion order is part of the language, not a
    /// container a caller should have to name. A key given twice keeps its first position and
    /// takes the last value — the caller is the one who can reject a repeat, and an object
    /// literal already does (§3).
    ///
    /// The way a caller outside this crate supplies an object as a starting variable
    /// ([`crate::evaluate_with_variables`]).
    ///
    /// A `__secret` key is dropped: it never exists in a Script's view of an object (§3).
    #[must_use]
    pub fn from_entries<K: Into<Arc<str>>>(entries: impl IntoIterator<Item = (K, Value)>) -> Self {
        Self::new(entries.into_iter().map(|(k, v)| (k.into(), v)).collect())
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.0.entries.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.entries.is_empty()
    }

    /// The value under `key`, if present.
    #[must_use]
    pub fn get(&self, key: &str) -> Option<&Value> {
        self.0.entries.get(key)
    }

    /// The entries in insertion order, borrowed — for a caller that walks a result without
    /// copying it.
    pub fn iter(&self) -> impl ExactSizeIterator<Item = (&str, &Value)> {
        self.0.entries.iter().map(|(k, v)| (&**k, v))
    }

    /// The entries in insertion order.
    #[must_use]
    pub fn entries(&self) -> Vec<(Arc<str>, Value)> {
        self.0
            .entries
            .iter()
            .map(|(k, v)| (Arc::clone(k), v.clone()))
            .collect()
    }
}

// Dropping a deeply nested result must not recurse once per level: each collection that is
// about to be freed hands its children to a flat work list instead.
impl Drop for Elements {
    fn drop(&mut self) {
        drop_flat(core::mem::take(&mut self.items));
    }
}

impl Drop for Entries {
    fn drop(&mut self) {
        let values = core::mem::take(&mut self.entries).into_values().collect();
        drop_flat(values);
    }
}

fn drop_flat(mut pending: Vec<Value>) {
    while let Some(value) = pending.pop() {
        match value {
            Value::Array(Array(shared)) => {
                // `into_inner` succeeds for exactly one of the last handles, so the storage is
                // emptied here and then dropped shallowly.
                if let Some(mut elements) = Arc::into_inner(shared) {
                    pending.append(&mut elements.items);
                }
            }
            Value::Object(Object(shared)) => {
                if let Some(mut entries) = Arc::into_inner(shared) {
                    pending.extend(entries.entries.drain(..).map(|(_, v)| v));
                }
            }
            _ => {}
        }
    }
}
