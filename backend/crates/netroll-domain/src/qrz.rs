// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
//! QRZ credential value types and the sealed-credential bundle. The domain
//! defines shapes and validation only; the envelope crypto lives in the
//! adapters crate. The plaintext types carry a REDACTING `Debug` and scrub on
//! drop, so a stray `{:?}` cannot leak a credential; the sealed type is opaque
//! ciphertext, and is what the repo persists.

use thiserror::Error;
use zeroize::ZeroizeOnDrop;

/// Maximum length (chars, post-trim) of a QRZ username or password. A QRZ.com
/// login is far shorter than this; the bound matches the profile-field
/// bounded-newtype cap (`parse_location`) and stops an unbounded value from
/// flowing into the seal path and storage.
pub const MAX_QRZ_FIELD_CHARS: usize = 128;

/// Why a submitted QRZ credential field was rejected. Messages double as the
/// problem+json `detail` text, and — like every credential type here — never
/// echo the offending value.
///
/// A lowercase `"<what would make it acceptable>"` fragment: the enum serves
/// both credential fields and cannot know which one it is about, so the caller
/// prefixes `"<field>: "` exactly as the composed domain enums do.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum QrzCredentialError {
    /// Value is empty or whitespace-only after trimming.
    #[error("is required")]
    Empty,
    /// Value exceeds [`MAX_QRZ_FIELD_CHARS`] after trimming.
    #[error("must be {} characters or fewer", MAX_QRZ_FIELD_CHARS)]
    TooLong,
}

/// A validated QRZ.com username (callsign or account login). Trimmed, non-empty,
/// bounded. Its `Debug` is **redacting** — the value is a stored credential, so
/// even an accidental `{:?}` must not surface it; the bytes are scrubbed
/// on drop.
#[derive(Clone, PartialEq, Eq, ZeroizeOnDrop)]
pub struct QrzUsername(String);

impl QrzUsername {
    /// The validated username as a string slice — for the crypto adapter's
    /// serialize-then-seal path only. Callers must not log the result.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl core::fmt::Debug for QrzUsername {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        // Redacting: never render the stored credential.
        f.write_str("QrzUsername(<redacted>)")
    }
}

/// A validated QRZ.com password. Trimmed, non-empty, bounded. Same redacting
/// `Debug` + zeroize-on-drop posture as [`QrzUsername`] — the sensitive half of
/// the pair.
#[derive(Clone, PartialEq, Eq, ZeroizeOnDrop)]
pub struct QrzPassword(String);

impl QrzPassword {
    /// The validated password as a string slice — for the crypto adapter's
    /// serialize-then-seal path only. Callers must not log the result.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl core::fmt::Debug for QrzPassword {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        // Redacting: never render the stored credential.
        f.write_str("QrzPassword(<redacted>)")
    }
}

/// Trims and bounds a raw credential field: rejects empty/whitespace-only and
/// anything longer than [`MAX_QRZ_FIELD_CHARS`] post-trim. Shared by both
/// field parsers (the sole two callers — under the three-strike threshold, but
/// the validation is byte-identical, so a helper avoids a copy-paste divergence
/// where only one field enforces the bound).
fn parse_credential_field(input: &str) -> Result<String, QrzCredentialError> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return Err(QrzCredentialError::Empty);
    }
    if trimmed.chars().count() > MAX_QRZ_FIELD_CHARS {
        return Err(QrzCredentialError::TooLong);
    }
    Ok(trimmed.to_owned())
}

/// Parses a QRZ username: trimmed, non-empty, ≤ [`MAX_QRZ_FIELD_CHARS`] chars.
pub fn parse_qrz_username(input: &str) -> Result<QrzUsername, QrzCredentialError> {
    parse_credential_field(input).map(QrzUsername)
}

/// Parses a QRZ password: trimmed, non-empty, ≤ [`MAX_QRZ_FIELD_CHARS`] chars.
pub fn parse_qrz_password(input: &str) -> Result<QrzPassword, QrzCredentialError> {
    parse_credential_field(input).map(QrzPassword)
}

/// A validated QRZ credential pair, ready to seal. Its derived `Debug` prints
/// only the fields' redacting `Debug`, so it too never leaks the secrets; both
/// fields scrub on drop.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QrzCredentials {
    /// The QRZ.com username/login.
    pub username: QrzUsername,
    /// The QRZ.com password.
    pub password: QrzPassword,
}

/// The opaque, at-rest form of a credential record: two AES-256-GCM
/// ciphertexts (the DEK wrapped under the KEK, and the credentials encrypted
/// under the DEK) each paired with its own fresh 96-bit nonce, plus the
/// KEK-version tag bound into the wrap. Holds **no plaintext** — this is what
/// the `qrz_credentials` repo persists verbatim. The bytes are not
/// secret on their own; only the absent KEK can unwrap them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SealedQrzCredentials {
    /// The per-record DEK, AES-256-GCM-encrypted under the instance KEK.
    pub wrapped_dek: Vec<u8>,
    /// The 96-bit nonce used for the DEK-wrap operation.
    pub dek_nonce: Vec<u8>,
    /// The serialized credentials, AES-256-GCM-encrypted under the DEK.
    pub credential_ciphertext: Vec<u8>,
    /// The 96-bit nonce used for the credential-encrypt operation.
    pub credential_nonce: Vec<u8>,
    /// Which KEK generation wrapped the DEK (the rotation forward-seam; MVP is
    /// always `1`). Bound into the wrap AAD so a version mismatch fails the tag.
    pub kek_version: i16,
}

/// A failure from the [`crate::ports::CredentialCipher`] seal/open path. Unit
/// variants only — the underlying AES-GCM error detail is deliberately dropped
/// so an attacker-influenced blob's failure carries no oracle.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum CipherError {
    /// No KEK is available on this instance — the cipher is fail-closed. Both
    /// `seal` and `open` return this when the instance booted without a `KEK`
    /// (existing credentials cannot be decrypted without the key).
    #[error("key-encryption key is unavailable on this instance")]
    KekUnavailable,
    /// A seal operation failed (e.g. the AEAD encrypt errored).
    #[error("sealing credentials failed")]
    SealFailed,
    /// An open operation failed: a GCM tag-verification failure (wrong KEK,
    /// tampered/ swapped blob, or AAD mismatch) or malformed sealed bytes.
    #[error("opening credentials failed")]
    OpenFailed,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn valid_username_and_password_parse_and_trim() {
        let u = parse_qrz_username("  W1AW  ").expect("valid username");
        assert_eq!(u.as_str(), "W1AW");
        let p = parse_qrz_password(" s3cretpass ").expect("valid password");
        assert_eq!(p.as_str(), "s3cretpass");
    }

    #[test]
    fn empty_and_whitespace_only_are_rejected() {
        assert_eq!(parse_qrz_username(""), Err(QrzCredentialError::Empty));
        assert_eq!(parse_qrz_username("   "), Err(QrzCredentialError::Empty));
        assert_eq!(parse_qrz_password(""), Err(QrzCredentialError::Empty));
        assert_eq!(parse_qrz_password("  \n "), Err(QrzCredentialError::Empty));
    }

    #[test]
    fn over_length_values_are_rejected_and_the_cap_itself_is_accepted() {
        assert_eq!(
            parse_qrz_username(&"x".repeat(129)),
            Err(QrzCredentialError::TooLong)
        );
        assert_eq!(
            parse_qrz_password(&"x".repeat(129)),
            Err(QrzCredentialError::TooLong)
        );
        assert!(parse_qrz_username(&"x".repeat(128)).is_ok());
        assert!(parse_qrz_password(&"x".repeat(128)).is_ok());
    }

    #[test]
    fn debug_impl_redacts_the_secret_never_printing_it() {
        let p = parse_qrz_password("hunter2super").expect("valid");
        let rendered = format!("{p:?}");
        assert!(
            !rendered.contains("hunter2super"),
            "password Debug must not leak the secret"
        );
        assert!(rendered.contains("redacted"));

        let u = parse_qrz_username("W1AWSECRET").expect("valid");
        let rendered_u = format!("{u:?}");
        assert!(
            !rendered_u.contains("W1AWSECRET"),
            "username Debug must not leak the value"
        );
    }

    #[test]
    fn credentials_debug_does_not_leak_either_field() {
        let creds = QrzCredentials {
            username: parse_qrz_username("W1AWUSER").unwrap(),
            password: parse_qrz_password("topsecretpw").unwrap(),
        };
        let rendered = format!("{creds:?}");
        assert!(!rendered.contains("W1AWUSER"));
        assert!(!rendered.contains("topsecretpw"));
    }
}
