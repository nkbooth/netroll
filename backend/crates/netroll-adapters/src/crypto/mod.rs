// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
//! AES-256-GCM envelope encryption for QRZ credentials. Two GCM operations per
//! record: encrypt under a fresh per-record DEK, then wrap that DEK under the
//! instance KEK. The DEK is never derived or reused and each operation draws
//! its own random nonce, so no deterministic counter is needed. No KEK, a
//! wrong KEK or a tampered byte all fail closed, never garbage plaintext.

// The pinned `aes-gcm` 0.10.x line (chosen for the NCC-audited
// RustCrypto stack) re-exports `generic-array` 0.14, whose types now carry a
// blanket `#[deprecated]` note nudging toward `generic-array` 1.x. That 1.x is
// only reachable via a future `aes-gcm` major —
// so `GenericArray`/`Nonce` are the ONLY key/nonce types this API accepts.
// Scope the allow to this module so the deprecation does not fail the
// `-D warnings` gate while remaining visible everywhere else.
#![allow(deprecated)]

use aes_gcm::Aes256Gcm;
use aes_gcm::aead::generic_array::GenericArray;
use aes_gcm::aead::rand_core::RngCore;
use aes_gcm::aead::{Aead, AeadCore, KeyInit, OsRng, Payload};
use base64::Engine;
use netroll_domain::ports::CredentialCipher;
use netroll_domain::qrz::{
    CipherError, QrzCredentials, SealedQrzCredentials, parse_qrz_password, parse_qrz_username,
};
use uuid::Uuid;
use zeroize::Zeroizing;

/// The KEK generation this MVP writes and reads. The rotation forward-seam
/// (bound into the DEK-wrap AAD): future rotation re-wraps DEKs under a new KEK
/// and bumps this — no credential is ever re-encrypted (the point of envelope
/// encryption). Rotation machinery itself is not built.
const KEK_VERSION: i16 = 1;

/// GCM standard nonce length in bytes (96 bits).
const NONCE_LEN: usize = 12;

/// AES-256 key length in bytes.
const KEY_LEN: usize = 32;

/// The instance key-encryption key: 32 raw bytes (AES-256) held in memory only,
/// inside a [`Zeroizing`] wrapper so it is scrubbed on drop. Parsed from the
/// base64 `KEK` env value; never persisted, never logged (its `Debug` is not
/// derived, so it cannot be `{:?}`-printed).
pub struct InstanceKek(Zeroizing<[u8; KEY_LEN]>);

/// Why a supplied `KEK` value could not be parsed into an [`InstanceKek`]. A
/// *present* KEK must be correct — [`crate::crypto`]'s caller (`resolve_kek`)
/// turns either variant into a hard boot error (boot posture).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KekParseError {
    /// The value was not valid base64.
    NotBase64,
    /// The value decoded to something other than exactly 32 bytes.
    WrongLength,
}

impl core::fmt::Display for KekParseError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        // Fixed text only — never echo the (secret) KEK value.
        let msg = match self {
            KekParseError::NotBase64 => "KEK is not valid base64",
            KekParseError::WrongLength => "KEK must decode to exactly 32 bytes (AES-256)",
        };
        f.write_str(msg)
    }
}

impl std::error::Error for KekParseError {}

impl InstanceKek {
    /// Parses a base64-encoded 32-byte KEK (`openssl rand -base64 32`). Rejects
    /// non-base64 input and any length other than 32 bytes.
    pub fn from_base64(raw: &str) -> Result<Self, KekParseError> {
        // The decoded bytes are key material — hold them in a scrubbing buffer
        // so the intermediate `Vec` is wiped even on the length-error path.
        let decoded = Zeroizing::new(
            base64::engine::general_purpose::STANDARD
                .decode(raw.trim())
                .map_err(|_| KekParseError::NotBase64)?,
        );
        let bytes: [u8; KEY_LEN] = decoded
            .as_slice()
            .try_into()
            .map_err(|_| KekParseError::WrongLength)?;
        Ok(Self(Zeroizing::new(bytes)))
    }

    /// Builds an AES-256-GCM cipher keyed by this KEK.
    fn cipher(&self) -> Aes256Gcm {
        // Infallible: the buffer is exactly 32 bytes by construction.
        Aes256Gcm::new_from_slice(self.0.as_slice()).expect("KEK is exactly 32 bytes")
    }
}

/// Copies stored nonce bytes into a fixed 12-byte array. Callers guard
/// `bytes.len() == NONCE_LEN` first, then wrap the array with the non-deprecated
/// `GenericArray::from([u8; N])` at the `decrypt` call site (where the nonce
/// size infers), avoiding the deprecated `GenericArray::from_slice`.
fn nonce_bytes(bytes: &[u8]) -> [u8; NONCE_LEN] {
    let mut arr = [0u8; NONCE_LEN];
    arr.copy_from_slice(bytes);
    arr
}

/// The envelope [`CredentialCipher`]: either holds an [`InstanceKek`] or is in a
/// fail-closed no-KEK state. Cheap to clone/share behind an `Arc`.
pub struct EnvelopeCipher {
    kek: Option<InstanceKek>,
}

impl EnvelopeCipher {
    /// Builds the cipher from an optional KEK: `Some` ⇒ real envelope crypto;
    /// `None` ⇒ fail-closed (both `seal` and `open` return
    /// [`CipherError::KekUnavailable`]). `main` passes the config-resolved KEK.
    pub fn new(kek: Option<InstanceKek>) -> Self {
        Self { kek }
    }
}

/// Serializes a credential pair to a length-prefixed byte buffer for the inner
/// encrypt: `u32-LE(username_len) ‖ username ‖ u32-LE(password_len) ‖ password`.
///
/// Length-prefixed (not `serde_json`) so the buffer holds no field-name/quoting
/// overhead around the secrets and can be wiped as one contiguous
/// [`Zeroizing`] region — the intermediate plaintext never lingers after seal.
fn serialize_credentials(creds: &QrzCredentials) -> Zeroizing<Vec<u8>> {
    let username = creds.username.as_str().as_bytes();
    let password = creds.password.as_str().as_bytes();
    let mut buf = Vec::with_capacity(8 + username.len() + password.len());
    buf.extend_from_slice(&(username.len() as u32).to_le_bytes());
    buf.extend_from_slice(username);
    buf.extend_from_slice(&(password.len() as u32).to_le_bytes());
    buf.extend_from_slice(password);
    Zeroizing::new(buf)
}

/// Reads one length-prefixed field starting at `*cursor`, advancing it past the
/// field. Every bound is checked — a tampered/truncated plaintext (which the
/// GCM tag should already have rejected) still cannot index out of range.
fn read_field<'a>(bytes: &'a [u8], cursor: &mut usize) -> Result<&'a [u8], CipherError> {
    let len_end = cursor.checked_add(4).ok_or(CipherError::OpenFailed)?;
    let len_bytes = bytes.get(*cursor..len_end).ok_or(CipherError::OpenFailed)?;
    let len = u32::from_le_bytes(len_bytes.try_into().expect("4-byte slice")) as usize;
    let field_end = len_end.checked_add(len).ok_or(CipherError::OpenFailed)?;
    let field = bytes
        .get(len_end..field_end)
        .ok_or(CipherError::OpenFailed)?;
    *cursor = field_end;
    Ok(field)
}

/// Reverses [`serialize_credentials`], re-validating each field through the
/// domain parsers (idempotent on already-trimmed stored values; defensive).
fn deserialize_credentials(bytes: &[u8]) -> Result<QrzCredentials, CipherError> {
    let mut cursor = 0usize;
    let username_bytes = read_field(bytes, &mut cursor)?;
    let password_bytes = read_field(bytes, &mut cursor)?;
    if cursor != bytes.len() {
        return Err(CipherError::OpenFailed);
    }
    let username = parse_qrz_username(
        core::str::from_utf8(username_bytes).map_err(|_| CipherError::OpenFailed)?,
    )
    .map_err(|_| CipherError::OpenFailed)?;
    let password = parse_qrz_password(
        core::str::from_utf8(password_bytes).map_err(|_| CipherError::OpenFailed)?,
    )
    .map_err(|_| CipherError::OpenFailed)?;
    Ok(QrzCredentials { username, password })
}

impl CredentialCipher for EnvelopeCipher {
    fn seal(
        &self,
        account_id: Uuid,
        creds: &QrzCredentials,
    ) -> Result<SealedQrzCredentials, CipherError> {
        let kek = self.kek.as_ref().ok_or(CipherError::KekUnavailable)?;

        // Fresh per-record DEK (OS CSPRNG), filled directly into the scrubbing
        // buffer. Deliberately NOT `Aes256Gcm::generate_key` + copy: that helper
        // returns a plain `GenericArray` with no zeroize-on-drop, which would
        // leave an un-scrubbed copy of the raw DEK on the stack after this call
        // returns. Filling the `Zeroizing` buffer directly means the DEK exists
        // in exactly one place, and it is always the buffer that gets wiped.
        let mut dek = Zeroizing::new([0u8; KEY_LEN]);
        OsRng.fill_bytes(dek.as_mut_slice());
        let dek_cipher =
            Aes256Gcm::new_from_slice(dek.as_slice()).map_err(|_| CipherError::SealFailed)?;

        // Encrypt the credentials under the DEK, binding account_id as AAD.
        let plaintext = serialize_credentials(creds);
        let credential_nonce = Aes256Gcm::generate_nonce(&mut OsRng);
        let credential_ciphertext = dek_cipher
            .encrypt(
                &credential_nonce,
                Payload {
                    msg: &plaintext,
                    aad: account_id.as_bytes(),
                },
            )
            .map_err(|_| CipherError::SealFailed)?;

        // Wrap the DEK under the instance KEK, binding kek_version as AAD.
        let dek_nonce = Aes256Gcm::generate_nonce(&mut OsRng);
        let wrapped_dek = kek
            .cipher()
            .encrypt(
                &dek_nonce,
                Payload {
                    msg: dek.as_slice(),
                    aad: &KEK_VERSION.to_le_bytes(),
                },
            )
            .map_err(|_| CipherError::SealFailed)?;

        Ok(SealedQrzCredentials {
            wrapped_dek,
            dek_nonce: dek_nonce.to_vec(),
            credential_ciphertext,
            credential_nonce: credential_nonce.to_vec(),
            kek_version: KEK_VERSION,
        })
    }

    fn open(
        &self,
        account_id: Uuid,
        sealed: &SealedQrzCredentials,
    ) -> Result<QrzCredentials, CipherError> {
        let kek = self.kek.as_ref().ok_or(CipherError::KekUnavailable)?;
        // Guard nonce lengths before `from_slice` (which would otherwise panic
        // on a malformed stored blob) — an attacker-influenced row must Err.
        if sealed.dek_nonce.len() != NONCE_LEN || sealed.credential_nonce.len() != NONCE_LEN {
            return Err(CipherError::OpenFailed);
        }

        // Unwrap the DEK under the KEK (kek_version AAD). A wrong KEK or a
        // tampered wrap fails the tag here.
        let dek = Zeroizing::new(
            kek.cipher()
                .decrypt(
                    &GenericArray::from(nonce_bytes(&sealed.dek_nonce)),
                    Payload {
                        msg: &sealed.wrapped_dek,
                        aad: &sealed.kek_version.to_le_bytes(),
                    },
                )
                .map_err(|_| CipherError::OpenFailed)?,
        );
        if dek.len() != KEY_LEN {
            return Err(CipherError::OpenFailed);
        }
        let dek_cipher = Aes256Gcm::new_from_slice(&dek).map_err(|_| CipherError::OpenFailed)?;

        // Decrypt the credentials under the DEK (account_id AAD). A row-swap
        // into another account fails the tag here.
        let plaintext = Zeroizing::new(
            dek_cipher
                .decrypt(
                    &GenericArray::from(nonce_bytes(&sealed.credential_nonce)),
                    Payload {
                        msg: &sealed.credential_ciphertext,
                        aad: account_id.as_bytes(),
                    },
                )
                .map_err(|_| CipherError::OpenFailed)?,
        );

        deserialize_credentials(&plaintext)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::Engine;
    use netroll_domain::ports::CredentialCipher;
    use netroll_domain::qrz::{
        CipherError, QrzCredentials, parse_qrz_password, parse_qrz_username,
    };
    use uuid::Uuid;

    fn creds(username: &str, password: &str) -> QrzCredentials {
        QrzCredentials {
            username: parse_qrz_username(username).expect("valid username"),
            password: parse_qrz_password(password).expect("valid password"),
        }
    }

    fn kek_from(bytes: [u8; 32]) -> InstanceKek {
        let b64 = base64::engine::general_purpose::STANDARD.encode(bytes);
        InstanceKek::from_base64(&b64).expect("32-byte base64 KEK")
    }

    fn enabled_cipher(bytes: [u8; 32]) -> EnvelopeCipher {
        EnvelopeCipher::new(Some(kek_from(bytes)))
    }

    #[test]
    fn seal_then_open_round_trips_the_exact_credentials() {
        let cipher = enabled_cipher([7u8; 32]);
        let account = Uuid::now_v7();
        let original = creds("W1AW", "s3cret-passphrase");

        let sealed = cipher.seal(account, &original).expect("seal succeeds");
        let opened = cipher.open(account, &sealed).expect("open succeeds");

        assert_eq!(opened, original, "round-trip returns the exact credentials");
    }

    #[test]
    fn two_seals_of_the_same_credentials_differ_in_ciphertext_and_nonces() {
        let cipher = enabled_cipher([7u8; 32]);
        let account = Uuid::now_v7();
        let original = creds("W1AW", "s3cret-passphrase");

        let a = cipher.seal(account, &original).expect("seal a");
        let b = cipher.seal(account, &original).expect("seal b");

        // Fresh DEK + fresh nonces per seal ⇒ every stored artifact differs.
        assert_ne!(a.credential_ciphertext, b.credential_ciphertext);
        assert_ne!(a.credential_nonce, b.credential_nonce);
        assert_ne!(a.wrapped_dek, b.wrapped_dek);
        assert_ne!(a.dek_nonce, b.dek_nonce);
        // Yet both still open to the same plaintext.
        assert_eq!(cipher.open(account, &a).unwrap(), original);
        assert_eq!(cipher.open(account, &b).unwrap(), original);
    }

    #[test]
    fn nonces_are_the_gcm_standard_96_bits() {
        let sealed = enabled_cipher([1u8; 32])
            .seal(Uuid::now_v7(), &creds("W1AW", "pw-here"))
            .unwrap();
        assert_eq!(sealed.dek_nonce.len(), 12);
        assert_eq!(sealed.credential_nonce.len(), 12);
        assert_eq!(sealed.kek_version, 1);
    }

    #[test]
    fn opening_with_a_different_kek_fails_the_tag_check() {
        let account = Uuid::now_v7();
        let sealed = enabled_cipher([1u8; 32])
            .seal(account, &creds("W1AW", "s3cret"))
            .expect("seal under KEK-A");

        // KEK-B is a different valid 32-byte key; unwrapping the DEK fails the
        // GCM tag — Err, never silent garbage plaintext.
        let wrong = enabled_cipher([2u8; 32]);
        assert_eq!(wrong.open(account, &sealed), Err(CipherError::OpenFailed));
    }

    #[test]
    fn a_no_kek_cipher_fails_closed_on_both_seal_and_open() {
        let account = Uuid::now_v7();
        let sealed = enabled_cipher([1u8; 32])
            .seal(account, &creds("W1AW", "s3cret"))
            .expect("seal under a real KEK");

        let no_kek = EnvelopeCipher::new(None);
        // Absent KEK ⇒ existing credentials cannot be decrypted.
        assert_eq!(
            no_kek.open(account, &sealed),
            Err(CipherError::KekUnavailable)
        );
        assert_eq!(
            no_kek.seal(account, &creds("W1AW", "s3cret")),
            Err(CipherError::KekUnavailable)
        );
    }

    #[test]
    fn opening_under_a_different_account_id_fails_the_aad_bind() {
        let sealed = enabled_cipher([1u8; 32])
            .seal(Uuid::now_v7(), &creds("W1AW", "s3cret"))
            .expect("seal for account one");

        // A row transplanted into another account's row: the account_id AAD no
        // longer matches, so the credential decrypt fails the tag.
        let other_account = Uuid::now_v7();
        assert_eq!(
            enabled_cipher([1u8; 32]).open(other_account, &sealed),
            Err(CipherError::OpenFailed)
        );
    }

    #[test]
    fn tampered_ciphertext_fails_to_open() {
        let account = Uuid::now_v7();
        let mut sealed = enabled_cipher([1u8; 32])
            .seal(account, &creds("W1AW", "s3cret"))
            .unwrap();
        // Flip a byte of the credential ciphertext — the GCM tag must reject it.
        sealed.credential_ciphertext[0] ^= 0xFF;
        assert_eq!(
            enabled_cipher([1u8; 32]).open(account, &sealed),
            Err(CipherError::OpenFailed)
        );
    }

    #[test]
    fn tampered_wrapped_dek_fails_to_open() {
        // Distinct from `tampered_ciphertext_fails_to_open` and the wrong-KEK
        // test: this flips a byte in the WRAPPED DEK itself (same KEK, same
        // wrap AAD) to prove the DEK-unwrap tag check independently rejects
        // tampering, rather than that guarantee only being covered indirectly
        // via a different KEK.
        let account = Uuid::now_v7();
        let mut sealed = enabled_cipher([3u8; 32])
            .seal(account, &creds("W1AW", "s3cret"))
            .unwrap();
        sealed.wrapped_dek[0] ^= 0xFF;
        assert_eq!(
            enabled_cipher([3u8; 32]).open(account, &sealed),
            Err(CipherError::OpenFailed)
        );
    }

    #[test]
    fn instance_kek_from_base64_requires_exactly_32_bytes_of_valid_base64() {
        // Valid 32-byte key.
        let ok = base64::engine::general_purpose::STANDARD.encode([9u8; 32]);
        assert!(InstanceKek::from_base64(&ok).is_ok());

        // 31 bytes and 33 bytes are both rejected (AES-256 needs exactly 32).
        let short = base64::engine::general_purpose::STANDARD.encode([9u8; 31]);
        assert!(InstanceKek::from_base64(&short).is_err());
        let long = base64::engine::general_purpose::STANDARD.encode([9u8; 33]);
        assert!(InstanceKek::from_base64(&long).is_err());

        // Not valid base64 at all.
        assert!(InstanceKek::from_base64("not valid base64!!!").is_err());
    }
}
