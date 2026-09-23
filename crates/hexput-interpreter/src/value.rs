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

use core::fmt;
use std::sync::Arc;

use indexmap::IndexMap;

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

struct Elements(Vec<Value>);
struct Entries(IndexMap<Arc<str>, Value>);

impl Array {
    pub(crate) fn new(items: Vec<Value>) -> Self {
        Self(Arc::new(Elements(items)))
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
        self.0.0.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.0.is_empty()
    }

    /// The element at `index`, if present.
    #[must_use]
    pub fn get(&self, index: usize) -> Option<&Value> {
        self.0.0.get(index)
    }

    /// The elements, in order.
    #[must_use]
    pub fn to_vec(&self) -> Vec<Value> {
        self.0.0.clone()
    }

    /// The elements, in order, borrowed — for a caller that walks a result without copying it.
    pub fn iter(&self) -> impl ExactSizeIterator<Item = &Value> {
        self.0.0.iter()
    }
}

impl Object {
    pub(crate) fn new(entries: IndexMap<Arc<str>, Value>) -> Self {
        Self(Arc::new(Entries(entries)))
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
    #[must_use]
    pub fn from_entries<K: Into<Arc<str>>>(entries: impl IntoIterator<Item = (K, Value)>) -> Self {
        Self::new(entries.into_iter().map(|(k, v)| (k.into(), v)).collect())
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.0.0.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.0.is_empty()
    }

    /// The value under `key`, if present.
    #[must_use]
    pub fn get(&self, key: &str) -> Option<&Value> {
        self.0.0.get(key)
    }

    /// The entries in insertion order, borrowed — for a caller that walks a result without
    /// copying it.
    pub fn iter(&self) -> impl ExactSizeIterator<Item = (&str, &Value)> {
        self.0.0.iter().map(|(k, v)| (&**k, v))
    }

    /// The entries in insertion order.
    #[must_use]
    pub fn entries(&self) -> Vec<(Arc<str>, Value)> {
        self.0
            .0
            .iter()
            .map(|(k, v)| (Arc::clone(k), v.clone()))
            .collect()
    }
}

// Dropping a deeply nested result must not recurse once per level: each collection that is
// about to be freed hands its children to a flat work list instead.
impl Drop for Elements {
    fn drop(&mut self) {
        drop_flat(core::mem::take(&mut self.0));
    }
}

impl Drop for Entries {
    fn drop(&mut self) {
        let values = core::mem::take(&mut self.0).into_values().collect();
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
                    pending.append(&mut elements.0);
                }
            }
            Value::Object(Object(shared)) => {
                if let Some(mut entries) = Arc::into_inner(shared) {
                    pending.extend(entries.0.drain(..).map(|(_, v)| v));
                }
            }
            _ => {}
        }
    }
}
