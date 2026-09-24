//! The six Resource Budget dimensions (FR-8) as one enum, shared by `hexput-enforce` (which
//! enforces them), `hexput-check`, and metrics.
//!
//! Each dimension is enforced on its own, never folded into a single limit, and exceeding one
//! ends the execution with an error naming that dimension. Story 3.5 enforces [`Dimension::CpuTime`]
//! and [`Dimension::Memory`]; Story 3.6 the four counted ones.

use core::fmt;

/// One Resource Budget dimension.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Dimension {
    /// Time spent running Script code, excluding every wait on the Backend.
    CpuTime,
    /// Memory the execution's values hold at once.
    Memory,
    /// Strings, arrays and objects constructed, plus resizes of a growing collection.
    Allocations,
    /// Registered Function or Method dispatches, counted whether or not they succeed.
    RpcCalls,
    /// The serialized byte length of the result.
    OutputSize,
    /// Host dispatches plus committed Global Variable writes.
    SideEffects,
}

impl Dimension {
    /// Every dimension, in the order FR-8 lists them.
    pub const ALL: &'static [Self] = &[
        Self::CpuTime,
        Self::Memory,
        Self::Allocations,
        Self::RpcCalls,
        Self::OutputSize,
        Self::SideEffects,
    ];

    /// The dimension's stable spelling: `cpu_time`, `memory`, `allocations`, `rpc_calls`,
    /// `output_size` or `side_effects` — the name a `budget` error and a log field use.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::CpuTime => "cpu_time",
            Self::Memory => "memory",
            Self::Allocations => "allocations",
            Self::RpcCalls => "rpc_calls",
            Self::OutputSize => "output_size",
            Self::SideEffects => "side_effects",
        }
    }
}

impl fmt::Display for Dimension {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}
