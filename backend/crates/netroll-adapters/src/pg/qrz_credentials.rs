// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
//! A 1:1 side table of envelope-encrypted QRZ credentials keyed on
//! `account_id`. The repo performs NO crypto: it moves the opaque
//! [`SealedQrzCredentials`] bytes the cipher produced and cannot read the
//! plaintext. Sealing and opening belong to the crypto adapter, and the KEK
//! never reaches this layer.

use netroll_domain::qrz::SealedQrzCredentials;
use sqlx::PgPool;
use uuid::Uuid;

/// Postgres repository for `qrz_credentials`.
#[derive(Clone)]
pub struct QrzCredentialRepo {
    pool: PgPool,
}

impl QrzCredentialRepo {
    /// Wraps the shared connection pool.
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    /// Upserts the 1:1 sealed credential record for `account_id`.
    ///
    /// A PUT replaces the whole row: the cipher mints a fresh DEK + nonces on
    /// every seal, so the `ON CONFLICT DO UPDATE` overwrites every artifact and
    /// bumps `updated_at` — no stale key material lingers.
    pub async fn set(
        &self,
        account_id: Uuid,
        sealed: &SealedQrzCredentials,
    ) -> Result<(), sqlx::Error> {
        sqlx::query!(
            "INSERT INTO qrz_credentials
                 (account_id, kek_version, wrapped_dek, dek_nonce,
                  credential_ciphertext, credential_nonce, created_at, updated_at)
             VALUES ($1, $2, $3, $4, $5, $6, now(), now())
             ON CONFLICT (account_id) DO UPDATE SET
                 kek_version = excluded.kek_version,
                 wrapped_dek = excluded.wrapped_dek,
                 dek_nonce = excluded.dek_nonce,
                 credential_ciphertext = excluded.credential_ciphertext,
                 credential_nonce = excluded.credential_nonce,
                 updated_at = now()",
            account_id,
            sealed.kek_version,
            sealed.wrapped_dek,
            sealed.dek_nonce,
            sealed.credential_ciphertext,
            sealed.credential_nonce,
        )
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Reads the sealed record for `account_id`, or `None` when the account has
    /// stored no credentials. Returns opaque bytes only — decryption is the
    /// cipher's job, and it needs the KEK this layer never sees.
    pub async fn get(&self, account_id: Uuid) -> Result<Option<SealedQrzCredentials>, sqlx::Error> {
        let row = sqlx::query!(
            "SELECT kek_version, wrapped_dek, dek_nonce,
                    credential_ciphertext, credential_nonce
             FROM qrz_credentials
             WHERE account_id = $1",
            account_id,
        )
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(|r| SealedQrzCredentials {
            wrapped_dek: r.wrapped_dek,
            dek_nonce: r.dek_nonce,
            credential_ciphertext: r.credential_ciphertext,
            credential_nonce: r.credential_nonce,
            kek_version: r.kek_version,
        }))
    }

    /// Clears the stored credentials for `account_id`. Idempotent: clearing a
    /// credential-less account is a no-op (the `clear`/`delete` posture).
    pub async fn delete(&self, account_id: Uuid) -> Result<(), sqlx::Error> {
        sqlx::query!(
            "DELETE FROM qrz_credentials WHERE account_id = $1",
            account_id,
        )
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Whether `account_id` has QRZ credentials stored — a boolean the
    /// write-only surface exposes as `qrzCredentialsSet`. Needs no KEK:
    /// reading existence is not decryption.
    pub async fn is_set(&self, account_id: Uuid) -> Result<bool, sqlx::Error> {
        let row = sqlx::query!(
            r#"SELECT EXISTS (
                   SELECT 1 FROM qrz_credentials WHERE account_id = $1
               ) AS "exists!""#,
            account_id,
        )
        .fetch_one(&self.pool)
        .await?;
        Ok(row.exists)
    }
}
