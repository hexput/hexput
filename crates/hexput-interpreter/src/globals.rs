//! A Plugin's Global Variables as an execution sees them (Story 6.7).
//!
//! The interpreter reaches no store itself: it defines [`Globals`], and the Executor hands an
//! execution one implementation of it per invocation. With one set
//! ([`Execution::with_globals`](crate::Execution::with_globals)), the top-level `let`s of the
//! Program are its Global Variables:
//!
//! * a top-level `let` binds the Global's stored value ([`Globals::current`]) instead of running
//!   its initializer; only when the store has none yet — the one seeding run at registration —
//!   does the initializer run, and its value seeds the store ([`Globals::seed`]);
//! * every read of a Global's name — one no inner binding shadows — first brings the execution's
//!   copy up to date with what other invocations wrote since ([`Globals::changes`]);
//! * every assignment whose target starts at a Global's name — `n = …`, `g.k = …`, `g[k] = …`,
//!   `g.k.x = …` — writes the lock unit it changed back to the store ([`Globals::write`]) right
//!   after the heap write, once the Executor has charged it as a side effect;
//! * an assignment to a Global whose value expression (and target key) makes no call and creates
//!   no function holds the unit's writer intent ([`Globals::begin_update`]) from its start until
//!   its write, so a concurrent `g.c = g.c + 1` loses no update.
//!
//! A value read out of a Global is the execution's own copy: writing through another name
//! (`let x = g.k; x.y = 1;`), a function's parameter, or a Backend's modification by Reference ID
//! changes that copy, never the store — until a later assignment by the Global's name writes the
//! unit holding it back, as the copy then is.
//!
//! Catching up keeps a copy's identity: an object stays the same object, its contents replaced
//! (see `Machine::sync_global`), so an assignment whose receiver was evaluated before a catch-up
//! still writes the Global.

use std::sync::Arc;

use hexput_ast::{AccessKind, ExprId, ExpressionKind, Literal, Program};

use crate::value::Held;

/// A Plugin's Global Variable store, as one execution reaches it: implemented by the Executor
/// over `hexput-globalvar`, one value per invocation. Every method is a short synchronous
/// critical section, except [`Globals::begin_update`], which may wait for another invocation's
/// update of the same unit to finish.
///
/// A lock unit is `(name, key)`: `key` is `None` for the whole variable and `Some(property)` for
/// one top-level property of a Global holding an object.
pub trait Globals: Send {
    /// The Global `name`'s whole current value, assembled from its units — `None` when the store
    /// holds none yet. The execution's copy is up to date with it afterwards.
    fn current(&mut self, name: &str) -> Option<Held>;

    /// What changed in the Global `name` since the execution's copy was last brought up to date —
    /// `None` when nothing did.
    fn changes(&mut self, name: &str) -> Option<Changes>;

    /// Give the Global `name` its initial value, unless the store already holds one.
    fn seed(&mut self, name: &str, value: Held);

    /// Write the unit `(name, key)`: `value` is the whole variable for `None`, and the property
    /// `key` of a Global holding an object otherwise.
    fn write(&mut self, name: &str, key: Option<&str>, value: Held);

    /// Take the writer intent of the unit `(name, key)` — the property `key` of the Global
    /// `name`, or for `None` the whole variable, which overlaps every one of its properties —
    /// waiting while another invocation holds an overlapping one. Released when the returned
    /// guard drops.
    fn begin_update(&mut self, name: &str, key: Option<&str>) -> Box<dyn Send>;
}

/// What changed in a Global since an execution last looked (see [`Globals::changes`]).
#[derive(Debug, Clone)]
pub enum Changes {
    /// Its whole value: the execution's copy is replaced.
    Whole(Held),
    /// Some top-level properties of a Global holding an object, each with its new value; the
    /// others are unchanged.
    Properties(Vec<(Arc<str>, Held)>),
}

/// The top-level `let` names of `program`: its Global Variables when it is a Plugin's.
pub(crate) fn names(program: &Program) -> Vec<String> {
    program
        .statements
        .iter()
        .filter_map(|statement| match &statement.kind {
            hexput_ast::StatementKind::Let { name, .. } => Some(name.name.clone()),
            _ => None,
        })
        .collect()
}

/// Whether evaluating the expression `id` can make no call — of a function, the host or a
/// method — and creates no function: what lets an assignment hold a writer intent across it.
pub(crate) fn calls_nothing(tree: &Program, id: ExprId) -> bool {
    let mut pending = vec![id];
    while let Some(id) = pending.pop() {
        match &tree.expression(id).kind {
            ExpressionKind::Function(_) => return false,
            ExpressionKind::Literal(_) | ExpressionKind::Identifier(_) => {}
            ExpressionKind::Group { expression, .. } => pending.push(*expression),
            ExpressionKind::Unary { operand, .. } => pending.push(*operand),
            ExpressionKind::Binary { left, right, .. } => {
                pending.push(*left);
                pending.push(*right);
            }
            ExpressionKind::Array { elements, .. } => pending.extend(elements.iter().copied()),
            ExpressionKind::Object { entries, .. } => {
                pending.extend(entries.iter().map(|entry| entry.value));
            }
            ExpressionKind::Access { base, links } => {
                pending.push(*base);
                for link in links {
                    match &link.kind {
                        AccessKind::Property(_) => {}
                        AccessKind::Index { expression, .. } => pending.push(*expression),
                        AccessKind::Call { .. } => return false,
                    }
                }
            }
        }
    }
    true
}

/// The property a link names statically: a `.name`, or an index by a string literal.
pub(crate) fn static_key<'t>(tree: &'t Program, kind: &'t AccessKind) -> Option<&'t str> {
    match kind {
        AccessKind::Property(name) => Some(name.name.as_str()),
        AccessKind::Index { expression, .. } => match &tree.expression(*expression).kind {
            ExpressionKind::Literal(Literal::String(key)) => Some(key.as_str()),
            _ => None,
        },
        AccessKind::Call { .. } => None,
    }
}
