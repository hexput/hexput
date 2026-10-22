//! The reconnect credential (FR-13, Story 5.5): a secret issued once at init, and the salted
//! hash of it a Session keeps in its place.
//!
//! The secret is 256 bits from the OS CSPRNG — the same source as Client IDs. The Session keeps
//! only a per-Session random salt and `HMAC-SHA256(key = salt, message = secret)`; a presented
//! secret is checked with `ring::hmac::verify`, which compares in constant time. The secret has
//! 256 bits of entropy, so a slow password-hashing function would add nothing.
//!
//! This module is the one definition of the credential: nothing else issues, stores or checks
//! one, and its text form leaves the Daemon only in the `Init` reply.

use ring::hmac;

/// Bytes in a reconnect secret, and in a salt: 256 bits each.
const SECRET_LEN: usize = 32;

/// Characters in a reconnect secret's text form: two lowercase hex digits per byte.
const HEX_LEN: usize = SECRET_LEN * 2;

/// A reconnect secret: 256 bits a Backend presents with its Client ID to prove it owns the
/// Session (FR-13).
///
/// Never stored in a Session, never logged: its `Debug` is redacted, it has no `Display`, no
/// `Clone` and no serde. Its text form — 64 lowercase hex characters — comes only from
/// [`to_hex`](Self::to_hex), and a presented one is read back by [`from_hex`](Self::from_hex).
pub struct ReconnectSecret([u8; SECRET_LEN]);

impl ReconnectSecret {
    /// A fresh secret from the OS CSPRNG.
    ///
    /// # Panics
    ///
    /// If the OS CSPRNG cannot be read, exactly as Client ID issue does.
    fn generate() -> Self {
        let mut bytes = [0_u8; SECRET_LEN];
        // No secret may be issued without the OS CSPRNG; see `Sessions::create`.
        getrandom::fill(&mut bytes).expect("the OS CSPRNG is unavailable");
        Self(bytes)
    }

    /// The secret's text form, 64 lowercase hex characters — what the `Init` reply carries under
    /// `reconnect_secret`. Calling this is the one way the secret becomes text; the caller is
    /// responsible for sending it to its Backend and nowhere else.
    #[must_use]
    pub fn to_hex(&self) -> String {
        const DIGITS: &[u8; 16] = b"0123456789abcdef";
        let mut text = String::with_capacity(HEX_LEN);
        for byte in self.0 {
            text.push(char::from(DIGITS[usize::from(byte >> 4)]));
            text.push(char::from(DIGITS[usize::from(byte & 0x0f)]));
        }
        text
    }

    /// A presented secret read back from its text form: `Some` only for exactly 64 lowercase hex
    /// characters, `None` for anything else (uppercase included).
    #[must_use]
    pub fn from_hex(text: &str) -> Option<Self> {
        let text = text.as_bytes();
        if text.len() != HEX_LEN {
            return None;
        }
        let mut bytes = [0_u8; SECRET_LEN];
        let (pairs, _) = text.as_chunks::<2>();
        for (byte, [high, low]) in bytes.iter_mut().zip(pairs) {
            *byte = (nibble(*high)? << 4) | nibble(*low)?;
        }
        Some(Self(bytes))
    }
}

/// One lowercase hex digit's value.
const fn nibble(digit: u8) -> Option<u8> {
    match digit {
        b'0'..=b'9' => Some(digit - b'0'),
        b'a'..=b'f' => Some(digit - b'a' + 10),
        _ => None,
    }
}

impl core::fmt::Debug for ReconnectSecret {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str("ReconnectSecret(<redacted>)")
    }
}

/// What a Session keeps of its reconnect secret: a random salt and the HMAC-SHA256 of the
/// secret keyed by it — never the secret.
pub(crate) struct Credential {
    salt: [u8; SECRET_LEN],
    digest: [u8; SECRET_LEN],
}

impl Credential {
    /// A fresh secret and the credential that verifies it. The secret is returned once, to be
    /// sent in the `Init` reply; the credential is what the Session stores.
    ///
    /// # Panics
    ///
    /// If the OS CSPRNG cannot be read.
    pub(crate) fn issue() -> (Self, ReconnectSecret) {
        let secret = ReconnectSecret::generate();
        let mut salt = [0_u8; SECRET_LEN];
        getrandom::fill(&mut salt).expect("the OS CSPRNG is unavailable");
        (Self::for_secret(salt, &secret), secret)
    }

    /// The credential `secret` gets under `salt`: the salt and `HMAC-SHA256(salt, secret)`.
    fn for_secret(salt: [u8; SECRET_LEN], secret: &ReconnectSecret) -> Self {
        let tag = hmac::sign(&key(&salt), &secret.0);
        let mut digest = [0_u8; SECRET_LEN];
        digest.copy_from_slice(tag.as_ref());
        Self { salt, digest }
    }

    /// Whether `secret` is the one this credential was issued for, compared in constant time.
    pub(crate) fn verify(&self, secret: &ReconnectSecret) -> bool {
        hmac::verify(&key(&self.salt), &secret.0, &self.digest).is_ok()
    }

    /// The credential an unknown Client ID is verified against, so a lookup that finds no
    /// Session still runs one HMAC verify, as one that finds a Session does. An all-zero digest
    /// under an all-zero salt: no secret's HMAC is all zeros but with probability 2^-256.
    pub(crate) const fn dummy() -> Self {
        Self {
            salt: [0; SECRET_LEN],
            digest: [0; SECRET_LEN],
        }
    }
}

/// The HMAC-SHA256 key a salt makes.
fn key(salt: &[u8; SECRET_LEN]) -> hmac::Key {
    hmac::Key::new(hmac::HMAC_SHA256, salt)
}

impl core::fmt::Debug for Credential {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str("Credential(<redacted>)")
    }
}

// In-crate because `Credential`, its salt and its digest are private: an integration test sees
// only `Sessions::verify_reconnect`, which cannot show that two salts give two digests.
#[cfg(test)]
mod tests {
    use super::{Credential, ReconnectSecret};

    /// Lowercase hex of some bytes, to search a `Debug` output for.
    fn hex(bytes: &[u8]) -> String {
        bytes.iter().map(|byte| format!("{byte:02x}")).collect()
    }

    #[test]
    fn one_secret_under_two_salts_gives_two_digests_and_both_verify() {
        let (first, secret) = Credential::issue();
        let mut salt = [0_u8; 32];
        getrandom::fill(&mut salt).unwrap();
        assert_ne!(salt, first.salt);
        let second = Credential::for_secret(salt, &secret);
        assert_ne!(first.digest, second.digest);
        assert!(first.verify(&secret));
        assert!(second.verify(&secret));
        let other = ReconnectSecret::generate();
        assert!(!first.verify(&other));
        assert!(!second.verify(&other));
        assert!(!Credential::dummy().verify(&secret));
    }

    #[test]
    fn the_credential_debug_shows_neither_salt_nor_digest() {
        let (credential, _secret) = Credential::issue();
        let shown = format!("{credential:?} {credential:#?}");
        for part in [&credential.salt, &credential.digest] {
            assert!(!shown.contains(&hex(part)), "{shown}");
            assert!(!shown.contains(&format!("{part:?}")), "{shown}");
        }
        assert_eq!(format!("{credential:?}"), "Credential(<redacted>)");
    }
}
