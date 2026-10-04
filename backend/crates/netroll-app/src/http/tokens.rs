// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
//! Bearer-token generation and decoding. Entropy is an effect, so it lives
//! here in the app layer, not in domain.

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use netroll_domain::auth::hash_token;
use rand::TryRngCore;
use rand::rngs::OsRng;

/// Raw token length before encoding (pinned: 32 bytes, OS CSPRNG).
const TOKEN_BYTES: usize = 32;

/// A freshly minted token: the base64url wire form (goes in the link or
/// cookie, never in storage or logs) plus its SHA-256 storage hash.
pub struct GeneratedToken {
    /// base64url-no-pad encoding of the raw bytes.
    pub wire: String,
    /// The only form that is ever persisted.
    pub hash: [u8; 32],
}

/// Mints a token from the OS CSPRNG.
pub fn generate() -> GeneratedToken {
    let mut raw = [0u8; TOKEN_BYTES];
    OsRng
        .try_fill_bytes(&mut raw)
        .expect("OS CSPRNG must be available");
    GeneratedToken {
        wire: URL_SAFE_NO_PAD.encode(raw),
        hash: hash_token(&raw),
    }
}

/// Mints a net link token: a 32-byte OS-CSPRNG value as a base64url-no-pad
/// string, returned in PLAINTEXT (never hashed).
///
/// Deliberately UNLIKE [`generate`], whose auth/session/magic-link tokens are
/// stored as a SHA-256 `hash` (the raw `wire` never persisted) because they
/// are single-use login secrets a DB leak must not grant. A net link token is
/// a long-lived, re-readable, read-only bearer permalink (the "unguessable
/// URL" pattern): the owner must fetch and copy it repeatedly, and it grants
/// only read access to a net definition, so it is stored and served in the
/// clear. Reuses the same 256-bit entropy source.
pub fn generate_link_token() -> String {
    let mut raw = [0u8; TOKEN_BYTES];
    OsRng
        .try_fill_bytes(&mut raw)
        .expect("OS CSPRNG must be available");
    URL_SAFE_NO_PAD.encode(raw)
}

/// Mints a per-net webhook HMAC secret: a 32-byte OS-CSPRNG value as a
/// base64url-no-pad string, returned in PLAINTEXT.
///
/// Entropy is an app-layer effect, so the mint lives here (out of the pure
/// domain), reusing [`generate_link_token`]'s exact 256-bit source and
/// encoding. Like a link token — and UNLIKE [`generate`]'s hashed auth tokens —
/// it is stored RECOVERABLE (plaintext at rest): delivery must read the raw
/// key back to compute `HMAC(secret, payload)` when it signs an outgoing
/// webhook, so a hash would make it useless. It is MORE sensitive than a link
/// token, though: it is revealed to the client exactly ONCE at mint and never
/// again (GET reports only a boolean), and it is **never logged**.
pub fn generate_webhook_secret() -> String {
    let mut raw = [0u8; TOKEN_BYTES];
    OsRng
        .try_fill_bytes(&mut raw)
        .expect("OS CSPRNG must be available");
    URL_SAFE_NO_PAD.encode(raw)
}

/// Decodes a wire token to its storage hash; `None` for anything that is not
/// exactly a base64url-encoded 32-byte value.
pub fn decode_to_hash(wire: &str) -> Option<[u8; 32]> {
    let raw = URL_SAFE_NO_PAD.decode(wire).ok()?;
    if raw.len() != TOKEN_BYTES {
        return None;
    }
    Some(hash_token(&raw))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_tokens_round_trip_and_are_unique() {
        let a = generate();
        let b = generate();

        assert_ne!(a.wire, b.wire, "two tokens must never collide");
        assert_eq!(decode_to_hash(&a.wire), Some(a.hash));
    }

    #[test]
    fn link_tokens_are_unique_plaintext_and_not_decodable_as_hashed_tokens() {
        let a = generate_link_token();
        let b = generate_link_token();
        assert_ne!(a, b, "two link tokens must never collide");
        assert!(!a.is_empty(), "the plaintext token is returned directly");
        // It is a valid base64url-no-pad 32-byte value (decodes to a hash the
        // same way a wire token would — proving the shared entropy source).
        assert!(decode_to_hash(&a).is_some());
    }

    #[test]
    fn webhook_secrets_are_unique_non_empty_and_high_entropy_encoded() {
        let a = generate_webhook_secret();
        let b = generate_webhook_secret();
        assert_ne!(a, b, "two webhook secrets must never collide");
        assert!(!a.is_empty(), "the plaintext secret is returned directly");
        // base64url-no-pad of 32 OS-CSPRNG bytes decodes to exactly 32 bytes —
        // the same entropy/shape as a link token (proving the shared source).
        assert_eq!(
            URL_SAFE_NO_PAD.decode(&a).expect("valid base64url").len(),
            TOKEN_BYTES,
            "32 bytes of entropy before encoding"
        );
    }

    #[test]
    fn malformed_or_wrong_length_tokens_do_not_decode() {
        assert_eq!(decode_to_hash("%%%not-base64%%%"), None);
        assert_eq!(decode_to_hash("aGVsbG8"), None, "5 bytes is not a token");
        assert_eq!(decode_to_hash(""), None);
    }
}
