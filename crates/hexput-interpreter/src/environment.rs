//! Lexical, block-level scopes (§5) as heap-resident records linked by parent handle, so a
//! closure can capture a scope by handle and see later mutations of its bindings — and a closure
//! that captures its own scope forms a cycle the per-execution heap frees for free.
//!
//! # Capture and reclaim
//!
//! A block scope is reclaimed when its block exits (`Frame::ExitScope`), which is only sound
//! while nothing outside the block can still reach it. Creating a function value breaks that:
//! the value holds the defining scope by handle, and lookups from it walk the parent chain. So
//! [`Heap::mark_captured`] marks the defining scope *and every ancestor* captured, and
//! [`Heap::release_scope`] refuses to reclaim a marked scope — it lives until the execution
//! ends, like any other heap garbage.
//!
//! The mark is deliberately conservative: it records that a function value *was created* here,
//! not that one escaped. Proving escape would need reachability analysis the interpreter does
//! not do. A scope in which no function value is ever created — which is every scope in a Script
//! that declares no functions, and every call scope whose body creates none — is still
//! reclaimed exactly as before.

use std::collections::HashMap;

use crate::heap::{Heap, RtValue, Slot, SlotId};

pub(crate) struct ScopeRecord {
    bindings: HashMap<String, RtValue>,
    parent: Option<SlotId>,
    /// Set once a function value closed over this scope (or over one nested inside it).
    captured: bool,
}

impl Heap {
    /// Allocate an empty scope nested in `parent` (or a root scope).
    pub(crate) fn push_scope(&mut self, parent: Option<SlotId>) -> SlotId {
        self.alloc(Slot::Scope(ScopeRecord {
            bindings: HashMap::new(),
            parent,
            captured: false,
        }))
    }

    fn scope(&self, id: SlotId) -> Option<&ScopeRecord> {
        match self.slot(id) {
            Some(Slot::Scope(record)) => Some(record),
            _ => None,
        }
    }

    fn scope_mut(&mut self, id: SlotId) -> Option<&mut ScopeRecord> {
        match self.slot_mut(id) {
            Some(Slot::Scope(record)) => Some(record),
            _ => None,
        }
    }

    /// Mark `scope` and its ancestors captured, so block exit stops reclaiming them. Walks the
    /// parent chain iteratively and stops at the first already-marked scope: marking always
    /// runs to the root, so everything above such a scope is marked already.
    pub(crate) fn mark_captured(&mut self, scope: SlotId) {
        let mut next = Some(scope);
        while let Some(id) = next {
            let Some(record) = self.scope_mut(id) else {
                return;
            };
            if record.captured {
                return;
            }
            record.captured = true;
            next = record.parent;
        }
    }

    /// Reclaim an exited block's scope, unless a function value captured it.
    pub(crate) fn release_scope(&mut self, id: SlotId) {
        if self.scope(id).is_some_and(|record| record.captured) {
            return;
        }
        self.release(id);
    }

    /// The names bound directly in `scope` (not its parents), sorted.
    pub(crate) fn names(&self, scope: SlotId) -> Vec<String> {
        let mut names: Vec<String> = self
            .scope(scope)
            .map(|record| record.bindings.keys().cloned().collect())
            .unwrap_or_default();
        names.sort();
        names
    }

    /// Bind `name` in `scope`. The parser has already rejected same-block redeclaration.
    pub(crate) fn declare(&mut self, scope: SlotId, name: &str, value: RtValue) {
        if let Some(record) = self.scope_mut(scope) {
            record.bindings.insert(name.to_owned(), value);
        }
    }

    /// The innermost scope, starting at `scope` and walking outward iteratively, that binds
    /// `name`.
    fn resolve(&self, mut scope: SlotId, name: &str) -> Option<SlotId> {
        loop {
            let record = self.scope(scope)?;
            if record.bindings.contains_key(name) {
                return Some(scope);
            }
            scope = record.parent?;
        }
    }

    /// Read the innermost binding of `name`.
    pub(crate) fn lookup(&self, scope: SlotId, name: &str) -> Option<RtValue> {
        let owner = self.resolve(scope, name)?;
        self.scope(owner)?.bindings.get(name).cloned()
    }

    /// Overwrite the innermost binding of `name`. Returns the value back when no scope declares
    /// it — there is no implicit global creation (§5).
    pub(crate) fn assign(
        &mut self,
        scope: SlotId,
        name: &str,
        value: RtValue,
    ) -> Result<(), RtValue> {
        let Some(owner) = self.resolve(scope, name) else {
            return Err(value);
        };
        match self
            .scope_mut(owner)
            .and_then(|record| record.bindings.get_mut(name))
        {
            Some(slot) => {
                *slot = value;
                Ok(())
            }
            None => Err(value),
        }
    }
}
