//! The six Resource Budget dimensions (FR-8) as one enum, shared by `hexput-enforce` (which
//! enforces them), `hexput-check`, and metrics.
//!
//! Each dimension is enforced on its own, never folded into a single limit, and exceeding one
//! ends the execution with an error naming that dimension. Story 3.5 enforces [`Dimension::CpuTime`]
//! and [`Dimension::Memory`]; Story 3.6 the four counted ones.
//!
//! Story 3.7 makes every limit tunable: [`Setting`] is the table of the eight values a Backend may
//! set in its Config and override per execution — each dimension's limit, the argument depth and
//! the per-call handler's timeout — with each one's wire path, allowed range and default.
//! [`Settings`] holds some of them, and can hold only values within range.

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

/// One tunable execution limit a Backend may set in its Config and override per execution
/// (Story 3.7): the six Resource Budget limits, the argument depth limit and the per-call
/// handler's timeout.
///
/// This table is the one place a setting's wire path, allowed range and default live. The
/// range is the Daemon's ceiling, inclusive: a value outside it is refused, never clamped.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Setting {
    /// [`Dimension::CpuTime`], in milliseconds.
    CpuTimeMs,
    /// [`Dimension::Memory`], in bytes.
    MemoryBytes,
    /// [`Dimension::Allocations`].
    Allocations,
    /// [`Dimension::RpcCalls`].
    RpcCalls,
    /// [`Dimension::OutputSize`], in bytes.
    OutputSizeBytes,
    /// [`Dimension::SideEffects`].
    SideEffects,
    /// How many arrays or objects deep a host call's argument may nest.
    ArgumentDepth,
    /// How long the Backend's per-call handler has to answer an `Authorize` question, in
    /// milliseconds.
    AuthorizationTimeoutMs,
}

/// The largest output size a setting may allow: the maximum frame, 16 MiB — a result past it
/// cannot be sent at all. `hexput-port` asserts at compile time that its `MAX_FRAME_LEN` agrees.
const MAX_OUTPUT_SIZE: u64 = 16 * 1024 * 1024;

impl Setting {
    /// Every setting, in the order the wire shape lists them.
    pub const ALL: &'static [Self] = &[
        Self::CpuTimeMs,
        Self::MemoryBytes,
        Self::Allocations,
        Self::RpcCalls,
        Self::OutputSizeBytes,
        Self::SideEffects,
        Self::ArgumentDepth,
        Self::AuthorizationTimeoutMs,
    ];

    /// Where the setting sits in a Config or override map, dotted: `budget.cpu_time_ms`,
    /// `budget.memory_bytes`, `budget.allocations`, `budget.rpc_calls`,
    /// `budget.output_size_bytes`, `budget.side_effects`, `argument_depth` or
    /// `authorization_timeout_ms`.
    #[must_use]
    pub const fn path(self) -> &'static str {
        match self {
            Self::CpuTimeMs => "budget.cpu_time_ms",
            Self::MemoryBytes => "budget.memory_bytes",
            Self::Allocations => "budget.allocations",
            Self::RpcCalls => "budget.rpc_calls",
            Self::OutputSizeBytes => "budget.output_size_bytes",
            Self::SideEffects => "budget.side_effects",
            Self::ArgumentDepth => "argument_depth",
            Self::AuthorizationTimeoutMs => "authorization_timeout_ms",
        }
    }

    /// The smallest value allowed. Zero is allowed only for the counted dimensions, where it
    /// means "none at all" (no host calls, say); a zero time, memory, output or depth could
    /// never run anything.
    #[must_use]
    pub const fn min(self) -> u64 {
        match self {
            Self::CpuTimeMs
            | Self::OutputSizeBytes
            | Self::ArgumentDepth
            | Self::AuthorizationTimeoutMs => 1,
            Self::MemoryBytes => 1024,
            Self::Allocations | Self::RpcCalls | Self::SideEffects => 0,
        }
    }

    /// The largest value allowed: the Daemon's ceiling, which no Config or override can raise.
    #[must_use]
    pub const fn max(self) -> u64 {
        match self {
            Self::CpuTimeMs | Self::AuthorizationTimeoutMs => 60_000,
            Self::MemoryBytes => 1024 * 1024 * 1024,
            Self::Allocations => 100_000_000,
            Self::RpcCalls | Self::SideEffects => 100_000,
            Self::OutputSizeBytes => MAX_OUTPUT_SIZE,
            Self::ArgumentDepth => 64,
        }
    }

    /// The value in force when neither Config nor an override sets it.
    #[must_use]
    pub const fn default(self) -> u64 {
        match self {
            Self::CpuTimeMs => 1_000,
            Self::MemoryBytes => 64 * 1024 * 1024,
            Self::Allocations => 1_000_000,
            Self::RpcCalls | Self::SideEffects => 100,
            Self::OutputSizeBytes => 1024 * 1024,
            Self::ArgumentDepth => 12,
            Self::AuthorizationTimeoutMs => 5_000,
        }
    }

    /// Where the setting's value is kept in [`Settings`].
    const fn index(self) -> usize {
        match self {
            Self::CpuTimeMs => 0,
            Self::MemoryBytes => 1,
            Self::Allocations => 2,
            Self::RpcCalls => 3,
            Self::OutputSizeBytes => 4,
            Self::SideEffects => 5,
            Self::ArgumentDepth => 6,
            Self::AuthorizationTimeoutMs => 7,
        }
    }
}

impl fmt::Display for Setting {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.path())
    }
}

/// Values for some of the [`Setting`]s — a Session's Config, or one execution's overrides —
/// each unset one left to whatever lies beneath it.
///
/// Every value held is within its setting's range: the only way to put one in is
/// [`Settings::set`], which checks it. The default holds nothing, so every setting falls back to
/// [`Setting::default`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Settings {
    values: [Option<u64>; Setting::ALL.len()],
}

impl Settings {
    /// No setting set.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            values: [None; Setting::ALL.len()],
        }
    }

    /// Set `setting` to `value`.
    ///
    /// # Errors
    /// [`OutOfRange`] when `value` lies outside the setting's range; nothing changes.
    pub fn set(&mut self, setting: Setting, value: u64) -> Result<(), OutOfRange> {
        if !(setting.min()..=setting.max()).contains(&value) {
            return Err(OutOfRange {
                setting,
                found: value,
            });
        }
        self.values[setting.index()] = Some(value);
        Ok(())
    }

    /// The value set for `setting`, if any.
    #[must_use]
    pub const fn get(&self, setting: Setting) -> Option<u64> {
        self.values[setting.index()]
    }

    /// The value in force for `setting`: the one set, or else its [`Setting::default`].
    #[must_use]
    pub const fn effective(&self, setting: Setting) -> u64 {
        match self.get(setting) {
            Some(value) => value,
            None => setting.default(),
        }
    }

    /// These settings with `over` laid on top: each setting `over` sets takes its value, every
    /// other keeps this one's. Neither input changes — an override never touches the Config
    /// beneath it.
    #[must_use]
    pub fn overlay(&self, over: &Self) -> Self {
        let mut values = self.values;
        for (value, over) in values.iter_mut().zip(over.values) {
            if over.is_some() {
                *value = over;
            }
        }
        Self { values }
    }
}

/// A value outside its [`Setting`]'s range, refused by [`Settings::set`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OutOfRange {
    setting: Setting,
    found: u64,
}

impl OutOfRange {
    /// The setting refused.
    #[must_use]
    pub const fn setting(&self) -> Setting {
        self.setting
    }

    /// The value refused.
    #[must_use]
    pub const fn found(&self) -> u64 {
        self.found
    }
}

impl fmt::Display for OutOfRange {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "`{}` must be an integer from {} to {}; found {}",
            self.setting,
            self.setting.min(),
            self.setting.max(),
            self.found
        )
    }
}

impl core::error::Error for OutOfRange {}
