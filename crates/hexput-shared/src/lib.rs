//! Cross-cutting types every other crate may depend on.
//! Internally split by concern (diagnostics, wire, ids, budget, policy) rather than left as a grab-bag module.
//! Not bound to any Architecture Decision directly; it exists so the crates that ARE bound to one
//! (hexput-check/AD-8, hexput-enforce/AD-3, hexput-port/AD-1, ...) share one vocabulary instead of
//! each defining their own ids/error shapes/budget dimensions.

pub mod budget;
pub mod diagnostics;
pub mod ids;
pub mod policy;
pub mod wire;
