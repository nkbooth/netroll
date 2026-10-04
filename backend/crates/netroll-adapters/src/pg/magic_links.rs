// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
//! Storage for single-use magic-link tokens. Only the SHA-256 hash of a
//! token is ever written; the raw token lives solely in the emailed link.

use netroll_domain::auth::MagicLinkToken;
use sqlx::PgPool;
use uuid::Uuid;

use super::{millis_from_utc, utc_from_millis};

/// Postgres repository for `magic_link_tokens`.
#[derive(Clone)]
pub struct MagicLinkRepo {
    pool: PgPool,
}

impl MagicLinkRepo {
    /// Wraps the shared connection pool.
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    /// Records a freshly issued link token for `email` (already normalized).
    pub async fn issue(
        &self,
        email: &str,
        token_hash: [u8; 32],
        expires_at_millis: u64,
    ) -> Result<(), sqlx::Error> {
        sqlx::query!(
            "INSERT INTO magic_link_tokens (id, email, token_hash, expires_at)
             VALUES ($1, $2, $3, $4)",
            Uuid::now_v7(),
            email,
            &token_hash[..],
            utc_from_millis(expires_at_millis),
        )
        .execute(&self.pool)
        .await
        .map(|_| ())
    }

    /// Fetches the domain view of a token for verdict evaluation.
    pub async fn find(&self, token_hash: [u8; 32]) -> Result<Option<MagicLinkToken>, sqlx::Error> {
        let row = sqlx::query!(
            "SELECT email, expires_at, consumed_at
             FROM magic_link_tokens WHERE token_hash = $1",
            &token_hash[..],
        )
        .fetch_optional(&self.pool)
        .await?;

        Ok(row.map(|r| MagicLinkToken {
            email: r.email,
            expires_at_millis: millis_from_utc(r.expires_at),
            consumed_at_millis: r.consumed_at.map(millis_from_utc),
        }))
    }

    /// Consumes a token and returns its email, or `None` if it was already
    /// consumed, expired at `now_millis`, or never existed.
    ///
    /// Single-use is enforced by this ONE atomic statement — race-safe under
    /// concurrent clicks, no read-then-write window. Expiry is judged against
    /// the caller's injected clock, never `now()`: Postgres' wall clock can
    /// drift from the app host's, and the domain verdict must agree with the
    /// enforcement boundary.
    pub async fn consume(
        &self,
        token_hash: [u8; 32],
        now_millis: u64,
    ) -> Result<Option<String>, sqlx::Error> {
        let row = sqlx::query!(
            "UPDATE magic_link_tokens SET consumed_at = $2
             WHERE token_hash = $1 AND consumed_at IS NULL AND expires_at > $2
             RETURNING email",
            &token_hash[..],
            utc_from_millis(now_millis),
        )
        .fetch_optional(&self.pool)
        .await?;

        Ok(row.map(|r| r.email))
    }

    /// Deletes every token that expired before `now_millis - retention_millis`,
    /// returning how many rows went.
    ///
    /// `expires_at` ALONE is the predicate, deliberately — not
    /// `consumed_at IS NOT NULL OR expires_at < cutoff`. A consumed token is
    /// dead immediately, but its `expires_at` is at most the 15-minute
    /// magic-link TTL in the future, so the single arm over-retains a consumed
    /// row by at most 15 minutes against a 30-day window and buys a predicate
    /// with no `OR` and no way to mis-order.
    ///
    /// The predicate is also account-INDEPENDENT by construction, which is the
    /// point: `magic_link_tokens` is email-keyed with no account FK at all, so
    /// `ON DELETE CASCADE` never reaches it and a finalized
    /// account's rows survive the delete. Time is the only thing this sweep
    /// needs to know about them.
    ///
    /// The cutoff is computed in Rust from the caller's injected clock, never
    /// `now()` in SQL: Postgres' wall clock can drift from the app host's, and
    /// every expiry judgement in this repo must agree with the enforcement
    /// boundary. `saturating_sub` keeps a clock near the epoch from underflowing.
    pub async fn prune_expired(
        &self,
        now_millis: u64,
        retention_millis: u64,
    ) -> Result<u64, sqlx::Error> {
        let cutoff = utc_from_millis(now_millis.saturating_sub(retention_millis));
        let result = sqlx::query!(
            "DELETE FROM magic_link_tokens WHERE expires_at < $1",
            cutoff
        )
        .execute(&self.pool)
        .await?;
        Ok(result.rows_affected())
    }
}
