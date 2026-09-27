//! The language feature toggles (FR-3, OQ-3; Story 3.9): the closed set of constructs a Backend
//! may switch off for its Scripts, shared by the Config decoder (`hexput-port`), the interpreter
//! that refuses a disabled construct when it evaluates it, `hexput-enforce`'s limits, and the
//! static check (Story 3.10).
//!
//! The set is closed. Every other construct — declarations, scalar literals, operators, property
//! and index access, `return`, Global Variables — is always on and can never be named as a
//! toggle.

use core::fmt;

/// One language feature toggle.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Feature {
    /// `while` and `for … in`.
    Loops,
    /// `if` (with every `else if` and `else` that is part of it).
    Conditionals,
    /// Defining a function, named or anonymous.
    Callbacks,
    /// `{ … }` object literals.
    ObjectLiterals,
    /// `[ … ]` array literals.
    ArrayLiterals,
    /// Every host call, whatever grant its name holds.
    RpcCalls,
}

impl Feature {
    /// Every toggle, in the order the wire shape lists them.
    pub const ALL: &'static [Self] = &[
        Self::Loops,
        Self::Conditionals,
        Self::Callbacks,
        Self::ObjectLiterals,
        Self::ArrayLiterals,
        Self::RpcCalls,
    ];

    /// The toggle's wire name, the key under `features`: `loops`, `conditionals`, `callbacks`,
    /// `object_literals`, `array_literals` or `rpc_calls`.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Loops => "loops",
            Self::Conditionals => "conditionals",
            Self::Callbacks => "callbacks",
            Self::ObjectLiterals => "object_literals",
            Self::ArrayLiterals => "array_literals",
            Self::RpcCalls => "rpc_calls",
        }
    }

    /// The toggle named `name` on the wire, if there is one.
    #[must_use]
    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL.iter().copied().find(|f| f.as_str() == name)
    }

    /// Where the toggle is kept, in [`Features`] and in a settings table.
    #[must_use]
    pub const fn index(self) -> usize {
        match self {
            Self::Loops => 0,
            Self::Conditionals => 1,
            Self::Callbacks => 2,
            Self::ObjectLiterals => 3,
            Self::ArrayLiterals => 4,
            Self::RpcCalls => 5,
        }
    }

    const fn bit(self) -> u8 {
        1 << self.index()
    }
}

impl fmt::Display for Feature {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Which toggles are enabled. Every toggle defaults to enabled.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Features {
    enabled: u8,
}

impl Features {
    /// Every toggle enabled: the default, and what `hexput eval` always runs with.
    pub const ALL_ENABLED: Self = Self {
        enabled: (1 << Feature::ALL.len()) - 1,
    };

    /// Whether `feature` is enabled.
    #[must_use]
    pub const fn is_enabled(self, feature: Feature) -> bool {
        self.enabled & feature.bit() != 0
    }

    /// These toggles with `feature` enabled or disabled.
    #[must_use]
    pub const fn with(self, feature: Feature, enabled: bool) -> Self {
        Self {
            enabled: if enabled {
                self.enabled | feature.bit()
            } else {
                self.enabled & !feature.bit()
            },
        }
    }
}

impl Default for Features {
    fn default() -> Self {
        Self::ALL_ENABLED
    }
}

/// The static check mode (FR-26, Story 3.10): whether Direct Execution runs `hexput-check`'s pass
/// over a Script before running it, and what a finding does.
///
/// Set in a Session's Config under the root key `check`, and overridable per execution.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum CheckMode {
    /// No pass at all: the default.
    #[default]
    Off,
    /// Run the pass, run the Script whatever it finds, and return the findings with the result.
    Warn,
    /// Run the pass and reject a Script with any error-severity finding before anything runs;
    /// otherwise run it and return the remaining findings (warnings) with the result.
    Error,
}

impl CheckMode {
    /// Every mode, in the order the wire shape lists them.
    pub const ALL: &'static [Self] = &[Self::Off, Self::Warn, Self::Error];

    /// The mode's wire spelling: `off`, `warn` or `error`.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Warn => "warn",
            Self::Error => "error",
        }
    }

    /// The mode spelled `name` on the wire, if there is one.
    #[must_use]
    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL.iter().copied().find(|m| m.as_str() == name)
    }
}

impl fmt::Display for CheckMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}
