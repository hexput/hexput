//! Identifier newtypes — never bare strings or integers past the boundary.
//!
//! Story 2.2 lands [`ClientId`]. `SessionId` and `PluginId` land with the stories that need them.

use core::fmt;
use core::str::FromStr;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// The identity of a Session, issued to a Backend by the init handshake (Story 2.4).
///
/// 128 bits. Its text form — `Display`, `FromStr` and serde — is exactly 32 **lowercase** hex
/// characters: a string rather than an integer because JavaScript numbers cannot hold 128 bits,
/// and one spelling only, so two SDKs can compare ids as strings. Generation is Story 2.4's;
/// this type only carries and spells one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ClientId([u8; 16]);

impl ClientId {
    /// Length of the text form, in characters.
    pub const TEXT_LEN: usize = 32;

    /// Wrap 16 bytes as a Client ID.
    #[must_use]
    pub const fn from_bytes(bytes: [u8; 16]) -> Self {
        Self(bytes)
    }

    /// The id's 16 bytes.
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; 16] {
        &self.0
    }
}

impl fmt::Display for ClientId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for byte in self.0 {
            write!(f, "{byte:02x}")?;
        }
        Ok(())
    }
}

/// Text that is not a Client ID: wrong length, or a character outside `0-9a-f`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseClientIdError;

impl fmt::Display for ParseClientIdError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "a Client ID is exactly {} lowercase hex characters",
            ClientId::TEXT_LEN
        )
    }
}

impl core::error::Error for ParseClientIdError {}

impl FromStr for ClientId {
    type Err = ParseClientIdError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let err = || ParseClientIdError;
        let text = s.as_bytes();
        if text.len() != Self::TEXT_LEN {
            return Err(err());
        }
        let nibble = |c: u8| match c {
            b'0'..=b'9' => Some(c - b'0'),
            b'a'..=b'f' => Some(c - b'a' + 10),
            _ => None,
        };
        let mut bytes = [0_u8; 16];
        let (pairs, _) = text.as_chunks::<2>();
        for (byte, &[hi, lo]) in bytes.iter_mut().zip(pairs) {
            let hi = nibble(hi).ok_or_else(err)?;
            let lo = nibble(lo).ok_or_else(err)?;
            *byte = (hi << 4) | lo;
        }
        Ok(Self(bytes))
    }
}

impl Serialize for ClientId {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for ClientId {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct Visitor;

        impl serde::de::Visitor<'_> for Visitor {
            type Value = ClientId;

            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("a Client ID of 32 lowercase hex characters")
            }

            fn visit_str<E: serde::de::Error>(self, v: &str) -> Result<ClientId, E> {
                v.parse().map_err(E::custom)
            }
        }

        deserializer.deserialize_str(Visitor)
    }
}
