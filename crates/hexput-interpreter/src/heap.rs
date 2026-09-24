//! The per-execution heap: every collection and scope an execution creates lives in one arena
//! owned by its `Machine`, and runtime values are plain handles into it.
//!
//! Nothing inside the heap is reference counted, so a reference cycle (`let a = []; a[0] = a;`,
//! or a closure capturing its own scope) costs nothing extra: when the execution ends — by
//! result or by error — the `Machine` drops and the whole heap goes with it. There is no
//! collection within an execution: garbage stays until the execution ends (Story 3.5's memory
//! budget bounds it). The one exception is scopes, reclaimed eagerly on block exit — and only
//! those no function value has captured (see `environment.rs`).
//!
//! Dropping the heap is flat by construction: slots hold handles, never owned children.
//!
//! # Memory metering (Story 3.5)
//!
//! The heap keeps an approximate count of the bytes the execution's values hold, which the
//! machine compares against the ceiling the Executor hands it. It is two counters:
//!
//! * **slots** — each live slot's [`footprint`]: a fixed cost per slot, plus one per array
//!   element, object entry (with its key's bytes) and scope binding (with its name's bytes).
//!   Charged when a slot is allocated or grows, credited when a slot is released.
//! * **strings** — every string the execution made, by its bytes plus a fixed overhead, for as
//!   long as it lives. A [`Text`] credits its own charge when the last handle to it drops, so a
//!   string shared by many bindings or elements counts once, and a string no longer reachable
//!   stops counting at once.
//!
//! Both are deliberately approximate — allocator overhead, spare vector capacity, the frame and
//! value stacks and the program itself are not counted — but each is monotone in what the
//! Script holds, which is what a ceiling needs.
//!
//! # Allocation counting (Story 3.6)
//!
//! The heap also counts the allocations the Script makes, for the allocation budget. The count is
//! defined by the language, not by what Rust allocates (LANGUAGE-REFERENCE §7): the machine counts
//! each string, array and object the Script constructs ([`Heap::allocated`]), and the heap itself
//! counts each *growth* of a collection — every append (an array element at its length, or a new
//! object key) that takes the collection's length past a power of two: from 1 to 2, 2 to 3, 4 to
//! 5, 8 to 9, and so on. Values copied in from outside — starting variables and host-call replies
//! — are never counted: [`Heap::attach`] builds them without touching the count.

use std::collections::HashMap;
use std::ops::Deref;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use indexmap::IndexMap;

use crate::environment::ScopeRecord;
use crate::value::{Array, Object, Value};

/// A handle to one heap slot. Meaningful only within the heap that issued it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct SlotId(usize);

/// A runtime value. Collections are handles, so cloning one aliases the same collection and
/// `==` on collections is handle identity (§4.2).
#[derive(Clone)]
pub(crate) enum RtValue {
    Null,
    Bool(bool),
    /// Always finite: every operation that would produce a non-finite result raises instead.
    Number(f64),
    /// Strings are immutable and cannot contain other values, so sharing them can never form a
    /// cycle. A [`Text`] is metered: it counts towards the heap's memory while it lives.
    String(Text),
    Array(SlotId),
    Object(SlotId),
    /// A function value: the defining scope plus which `Function` in the program it is. Like a
    /// collection, cloning one aliases it and `==` is handle identity.
    Function(SlotId),
}

impl RtValue {
    /// The type name used in diagnostics: `null`, `bool`, `number`, `string`, `array`, `object`.
    pub(crate) const fn type_name(&self) -> &'static str {
        match self {
            Self::Null => "null",
            Self::Bool(_) => "bool",
            Self::Number(_) => "number",
            Self::String(_) => "string",
            Self::Array(_) => "array",
            Self::Object(_) => "object",
            Self::Function(_) => "function",
        }
    }

    pub(crate) const fn is_null(&self) -> bool {
        matches!(self, Self::Null)
    }

    /// §4.2 language equality (`==`). Same type compares directly, collections by identity;
    /// number vs string converts the string (a non-numeric string is simply unequal); every
    /// other cross-type pair is unequal.
    pub(crate) fn equals(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Null, Self::Null) => true,
            (Self::Bool(a), Self::Bool(b)) => a == b,
            (Self::Number(a), Self::Number(b)) => a == b,
            (Self::String(a), Self::String(b)) => a == b,
            (Self::Array(a), Self::Array(b))
            | (Self::Object(a), Self::Object(b))
            | (Self::Function(a), Self::Function(b)) => a == b,
            (Self::Number(n), Self::String(s)) | (Self::String(s), Self::Number(n)) => {
                crate::convert::parse_number(s) == Some(*n)
            }
            _ => false,
        }
    }
}

/// Bytes held by the strings of one execution — every live [`Text`] it made. Shared by the heap
/// and each `Text`, which credits its own charge when it drops, wherever it drops.
#[derive(Clone, Default)]
pub(crate) struct TextMeter(Arc<AtomicUsize>);

impl TextMeter {
    fn get(&self) -> usize {
        self.0.load(Ordering::Relaxed)
    }
}

/// A string value inside an execution: shared text, metered for as long as any handle to it
/// lives. Cloning one aliases the same text and charges nothing more.
#[derive(Clone)]
pub(crate) struct Text(Arc<Metered>);

struct Metered {
    text: Arc<str>,
    /// What this string added to its meter, and so what it gives back on drop.
    charge: usize,
    meter: TextMeter,
}

impl Drop for Metered {
    fn drop(&mut self) {
        self.meter.0.fetch_sub(self.charge, Ordering::Relaxed);
    }
}

/// The fixed cost of one string: its two reference-counted allocations' headers and the metered
/// record itself.
pub(crate) const TEXT_OVERHEAD: usize = size_of::<Metered>() + 4 * size_of::<usize>();

impl Text {
    /// The text as a shareable `Arc<str>`, for a detached value.
    pub(crate) fn shared(&self) -> Arc<str> {
        Arc::clone(&self.0.text)
    }

    pub(crate) fn as_str(&self) -> &str {
        &self.0.text
    }
}

impl Deref for Text {
    type Target = str;

    fn deref(&self) -> &str {
        &self.0.text
    }
}

impl PartialEq for Text {
    fn eq(&self, other: &Self) -> bool {
        self.as_str() == other.as_str()
    }
}

/// The fixed cost of one heap slot.
const SLOT: usize = size_of::<Slot>();
/// One array element.
const ELEMENT: usize = size_of::<RtValue>();
/// One object entry, excluding its key's bytes: the key handle, the value and the map's index.
const ENTRY: usize = size_of::<Arc<str>>() + size_of::<RtValue>() + 2 * size_of::<usize>();
/// One scope binding, excluding its name's bytes: the name, the value and the map's control word.
pub(crate) const BINDING: usize = size_of::<String>() + size_of::<RtValue>() + size_of::<usize>();

/// What a slot's contents cost the slots counter, strings excepted: they meter themselves.
fn footprint(slot: &Slot) -> usize {
    match slot {
        Slot::Free => 0,
        Slot::Array { items, .. } => SLOT + items.len() * ELEMENT,
        Slot::Object { entries, .. } => {
            SLOT + entries.keys().map(|key| ENTRY + key.len()).sum::<usize>()
        }
        Slot::Scope(record) => SLOT + record.footprint(),
        Slot::Function(_) => SLOT,
    }
}

/// What a function value points at: which `Function` of the program (an index into the
/// machine's table, so the heap stays free of the program's lifetime) and the scope it closes
/// over. The scope is held by handle, so the closure sees later mutations of its bindings (§6).
pub(crate) struct FunctionRecord {
    pub(crate) definition: usize,
    pub(crate) scope: SlotId,
}

/// Why a value has no detached form (see [`Heap::detach`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DetachFailure {
    /// The reachable graph refers back to itself, so it is not a finite tree.
    Cycle,
    /// The value is, or contains, a function.
    Function,
}

pub(crate) enum Slot {
    /// Reclaimed; its id is on the free list.
    Free,
    /// `version` counts mutations of this collection itself, so a `for` loop can tell that the
    /// collection it is iterating changed under it. Mutating a *nested* collection bumps that
    /// one's counter, never this one.
    Array {
        items: Vec<RtValue>,
        version: u64,
    },
    Object {
        entries: IndexMap<Arc<str>, RtValue>,
        version: u64,
    },
    Scope(ScopeRecord),
    Function(FunctionRecord),
}

#[derive(Default)]
pub(crate) struct Heap {
    slots: Vec<Slot>,
    free: Vec<SlotId>,
    /// The live slots' footprints, summed.
    slot_bytes: usize,
    /// The live strings' charges, summed.
    texts: TextMeter,
    /// Allocations the Script has made (see the module documentation).
    allocations: u64,
}

impl Heap {
    /// Approximately how many bytes the execution's values hold right now (see the module
    /// documentation).
    pub(crate) fn used(&self) -> usize {
        self.slot_bytes.saturating_add(self.texts.get())
    }

    /// How many allocations the Script has made so far.
    pub(crate) const fn allocations(&self) -> u64 {
        self.allocations
    }

    /// Count `count` allocations the Script made.
    pub(crate) fn allocated(&mut self, count: u64) {
        self.allocations = self.allocations.saturating_add(count);
    }

    /// Count an append to a collection whose length was `before`: a growth when it takes the
    /// length past a power of two.
    fn appended(&mut self, before: usize) {
        if before.is_power_of_two() {
            self.allocated(1);
        }
    }

    /// Record that a live slot grew by `bytes`.
    pub(crate) fn grow(&mut self, bytes: usize) {
        self.slot_bytes = self.slot_bytes.saturating_add(bytes);
    }

    /// A new string holding `text`, charged in full: its bytes are new to this execution.
    pub(crate) fn text(&self, text: impl Into<Arc<str>>) -> RtValue {
        let text = text.into();
        let charge = TEXT_OVERHEAD + text.len();
        self.metered(text, charge)
    }

    fn metered(&self, text: Arc<str>, charge: usize) -> RtValue {
        self.texts.0.fetch_add(charge, Ordering::Relaxed);
        RtValue::String(Text(Arc::new(Metered {
            text,
            charge,
            meter: self.texts.clone(),
        })))
    }

    pub(crate) fn alloc(&mut self, slot: Slot) -> SlotId {
        self.grow(footprint(&slot));
        while let Some(id) = self.free.pop() {
            if let Some(free @ Slot::Free) = self.slots.get_mut(id.0) {
                *free = slot;
                return id;
            }
        }
        self.slots.push(slot);
        SlotId(self.slots.len() - 1)
    }

    /// Return a slot to the free list, dropping its contents. Contents are handles, so this is
    /// shallow; any collection it referenced stays until the execution ends.
    pub(crate) fn release(&mut self, id: SlotId) {
        if let Some(slot) = self.slots.get_mut(id.0)
            && !matches!(slot, Slot::Free)
        {
            let freed = footprint(slot);
            *slot = Slot::Free;
            self.free.push(id);
            self.slot_bytes = self.slot_bytes.saturating_sub(freed);
        }
    }

    pub(crate) fn slot(&self, id: SlotId) -> Option<&Slot> {
        self.slots.get(id.0)
    }

    pub(crate) fn slot_mut(&mut self, id: SlotId) -> Option<&mut Slot> {
        self.slots.get_mut(id.0)
    }

    pub(crate) fn new_array(&mut self, items: Vec<RtValue>) -> RtValue {
        RtValue::Array(self.alloc(Slot::Array { items, version: 0 }))
    }

    pub(crate) fn new_object(&mut self, entries: IndexMap<Arc<str>, RtValue>) -> RtValue {
        RtValue::Object(self.alloc(Slot::Object {
            entries,
            version: 0,
        }))
    }

    /// Allocate a function value closing over `scope`.
    pub(crate) fn new_function(&mut self, definition: usize, scope: SlotId) -> RtValue {
        RtValue::Function(self.alloc(Slot::Function(FunctionRecord { definition, scope })))
    }

    /// The definition index and captured scope of the function at `id`.
    pub(crate) fn function(&self, id: SlotId) -> Option<(usize, SlotId)> {
        match self.slot(id) {
            Some(Slot::Function(record)) => Some((record.definition, record.scope)),
            _ => None,
        }
    }

    /// How many times the collection at `id` has been mutated; `0` for anything else.
    pub(crate) fn version(&self, id: SlotId) -> u64 {
        match self.slot(id) {
            Some(Slot::Array { version, .. } | Slot::Object { version, .. }) => *version,
            _ => 0,
        }
    }

    /// The object's keys in insertion order — the sequence `for (key in object)` walks.
    pub(crate) fn object_keys(&self, id: SlotId) -> Vec<Arc<str>> {
        self.entries(id)
            .map(|entries| entries.keys().cloned().collect())
            .unwrap_or_default()
    }

    /// The elements of the array at `id`; empty for a handle that is not an array (which the
    /// machine never produces).
    fn elements(&self, id: SlotId) -> &[RtValue] {
        match self.slot(id) {
            Some(Slot::Array { items, .. }) => items,
            _ => &[],
        }
    }

    fn entries(&self, id: SlotId) -> Option<&IndexMap<Arc<str>, RtValue>> {
        match self.slot(id) {
            Some(Slot::Object { entries, .. }) => Some(entries),
            _ => None,
        }
    }

    pub(crate) fn array_len(&self, id: SlotId) -> usize {
        self.elements(id).len()
    }

    pub(crate) fn array_get(&self, id: SlotId, index: usize) -> Option<RtValue> {
        self.elements(id).get(index).cloned()
    }

    /// Set `index`, or append when `index` is exactly the length. Returns `false` otherwise.
    pub(crate) fn array_store(&mut self, id: SlotId, index: usize, value: RtValue) -> bool {
        let Some(Slot::Array { items, version }) = self.slot_mut(id) else {
            return false;
        };
        let before = items.len();
        let grew = if let Some(slot) = items.get_mut(index) {
            *slot = value;
            0
        } else if index == before {
            items.push(value);
            ELEMENT
        } else {
            return false;
        };
        *version = version.wrapping_add(1);
        if grew > 0 {
            self.appended(before);
        }
        self.grow(grew);
        true
    }

    pub(crate) fn object_get(&self, id: SlotId, key: &str) -> Option<RtValue> {
        self.entries(id)?.get(key).cloned()
    }

    /// Replace an existing key in place, or append a new one at the end.
    pub(crate) fn object_store(&mut self, id: SlotId, key: &str, value: RtValue) {
        if let Some(Slot::Object { entries, version }) = self.slot_mut(id) {
            let before = entries.len();
            let grew = if let Some(slot) = entries.get_mut(key) {
                *slot = value;
                0
            } else {
                entries.insert(Arc::from(key), value);
                ENTRY + key.len()
            };
            *version = version.wrapping_add(1);
            if grew > 0 {
                self.appended(before);
            }
            self.grow(grew);
        }
    }

    /// §4.1 truthiness: `null`, `false`, `0`, `""`, and empty collections are falsy.
    pub(crate) fn is_truthy(&self, value: &RtValue) -> bool {
        match value {
            RtValue::Null => false,
            RtValue::Bool(b) => *b,
            RtValue::Number(n) => *n != 0.0,
            RtValue::String(s) => !s.is_empty(),
            RtValue::Array(id) => !self.elements(*id).is_empty(),
            RtValue::Object(id) => self.entries(*id).is_some_and(|e| !e.is_empty()),
            // A function is a thing, never an empty collection, so it is always truthy.
            RtValue::Function(_) => true,
        }
    }

    /// Copy `root` and everything reachable from it out of the heap into an owned [`Value`].
    /// Fails when the reachable graph contains a cycle, or reaches a function — neither has a
    /// finite detached form the Backend can receive (§7, and Story 1.7 decision 1).
    ///
    /// Walks with an explicit stack, so nesting depth never grows the host stack. A collection
    /// is "on the path" from when it is entered until its children are done; meeting one again
    /// in that window is a cycle. Meeting one again after it is done is sharing (`[x, x]`), not
    /// a cycle, and reuses the memoized detached form, so shared structure is detached once.
    pub(crate) fn detach(&self, root: &RtValue) -> Result<Value, DetachFailure> {
        enum Visit {
            Enter(RtValue),
            Finish(SlotId),
        }
        enum Mark {
            OnPath,
            Done(Value),
        }
        let mut work = vec![Visit::Enter(root.clone())];
        let mut out: Vec<Value> = Vec::new();
        let mut marks: HashMap<SlotId, Mark> = HashMap::new();
        while let Some(visit) = work.pop() {
            match visit {
                Visit::Enter(value) => {
                    let id = match value {
                        RtValue::Null => {
                            out.push(Value::Null);
                            continue;
                        }
                        RtValue::Bool(b) => {
                            out.push(Value::Bool(b));
                            continue;
                        }
                        RtValue::Number(n) => {
                            out.push(Value::Number(n));
                            continue;
                        }
                        RtValue::String(s) => {
                            out.push(Value::String(s.shared()));
                            continue;
                        }
                        RtValue::Function(_) => return Err(DetachFailure::Function),
                        RtValue::Array(id) | RtValue::Object(id) => id,
                    };
                    match marks.get(&id) {
                        Some(Mark::Done(done)) => {
                            out.push(done.clone());
                            continue;
                        }
                        Some(Mark::OnPath) => return Err(DetachFailure::Cycle),
                        None => {}
                    }
                    marks.insert(id, Mark::OnPath);
                    work.push(Visit::Finish(id));
                    // Reversed so children are entered, and land on `out`, in order.
                    match self.slot(id) {
                        Some(Slot::Array { items, .. }) => {
                            work.extend(items.iter().rev().cloned().map(Visit::Enter));
                        }
                        Some(Slot::Object { entries, .. }) => {
                            work.extend(entries.values().rev().cloned().map(Visit::Enter));
                        }
                        _ => {}
                    }
                }
                Visit::Finish(id) => {
                    let detached = match self.slot(id) {
                        Some(Slot::Array { items, .. }) => {
                            let at = out.len().saturating_sub(items.len());
                            Value::Array(Array::new(out.split_off(at)))
                        }
                        Some(Slot::Object { entries, .. }) => {
                            let at = out.len().saturating_sub(entries.len());
                            let values = out.split_off(at);
                            Value::Object(Object::new(
                                entries.keys().cloned().zip(values).collect(),
                            ))
                        }
                        _ => Value::Null,
                    };
                    marks.insert(id, Mark::Done(detached.clone()));
                    out.push(detached);
                }
            }
        }
        out.pop().ok_or(DetachFailure::Cycle)
    }

    /// Copy an owned [`Value`] into this heap and return the handle — the inverse of
    /// [`Heap::detach`], used to bind a caller-supplied starting variable before the Script runs.
    ///
    /// Walks with an explicit stack, so an adversarially nested input never grows the host stack.
    /// Structure the input shares (the same `Arc` reached twice) expands into separate slots, so
    /// the two are distinct collections inside the execution. That is unobservable in the same
    /// way sharing is on the way out: a detached value has no identity (§4.2 identity exists only
    /// within one execution), so nothing could have told the caller they were the same.
    pub(crate) fn attach(&mut self, root: &Value) -> RtValue {
        enum Visit {
            Enter(Value),
            /// `[e0 … eN-1]` on the output stack → one array handle.
            FinishArray(usize),
            /// `[v0 … vN-1]` → one object handle, taking the keys back in order.
            FinishObject(Vec<Arc<str>>),
        }
        let mut work = vec![Visit::Enter(root.clone())];
        let mut out: Vec<RtValue> = Vec::new();
        while let Some(visit) = work.pop() {
            match visit {
                Visit::Enter(value) => match value {
                    Value::Null => out.push(RtValue::Null),
                    Value::Bool(b) => out.push(RtValue::Bool(b)),
                    Value::Number(n) => out.push(RtValue::Number(n)),
                    Value::String(s) => out.push(self.text(s)),
                    Value::Array(array) => {
                        let items = array.to_vec();
                        work.push(Visit::FinishArray(items.len()));
                        // Reversed so children are entered, and land on `out`, in order.
                        work.extend(items.into_iter().rev().map(Visit::Enter));
                    }
                    Value::Object(object) => {
                        let entries = object.entries();
                        work.push(Visit::FinishObject(
                            entries.iter().map(|(key, _)| Arc::clone(key)).collect(),
                        ));
                        work.extend(entries.into_iter().rev().map(|(_, v)| Visit::Enter(v)));
                    }
                },
                Visit::FinishArray(len) => {
                    let at = out.len().saturating_sub(len);
                    let items = out.split_off(at);
                    let array = self.new_array(items);
                    out.push(array);
                }
                Visit::FinishObject(keys) => {
                    let at = out.len().saturating_sub(keys.len());
                    let values = out.split_off(at);
                    let object = self.new_object(keys.into_iter().zip(values).collect());
                    out.push(object);
                }
            }
        }
        out.pop().unwrap_or(RtValue::Null)
    }

    /// Number of slots ever allocated (live or free).
    #[cfg(test)]
    pub(crate) fn capacity(&self) -> usize {
        self.slots.len()
    }

    /// Number of slots currently in use.
    #[cfg(test)]
    pub(crate) fn live(&self) -> usize {
        self.slots
            .iter()
            .filter(|s| !matches!(s, Slot::Free))
            .count()
    }

    /// Every string held directly by a collection slot — lets a test keep a strong handle to a
    /// heap-resident string and watch it be released when the heap drops.
    #[cfg(test)]
    pub(crate) fn strings(&self) -> Vec<Arc<str>> {
        let mut found = Vec::new();
        for slot in &self.slots {
            let values: Vec<&RtValue> = match slot {
                Slot::Array { items, .. } => items.iter().collect(),
                Slot::Object { entries, .. } => entries.values().collect(),
                _ => Vec::new(),
            };
            for value in values {
                if let RtValue::String(s) = value {
                    found.push(s.shared());
                }
            }
        }
        found
    }

    /// Whether some array slot holds a handle to itself.
    #[cfg(test)]
    pub(crate) fn has_self_containing_array(&self) -> bool {
        self.slots.iter().enumerate().any(|(i, slot)| {
            matches!(slot, Slot::Array { items, .. }
                if items.iter().any(|v| matches!(v, RtValue::Array(id) if id.0 == i)))
        })
    }
}
