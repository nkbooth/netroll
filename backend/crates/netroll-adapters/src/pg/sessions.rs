// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
//! Storage for server-side, revocable sessions. Sessions are Postgres rows —
//! revocation and expiry are judged by the domain from what is stored here.

use netroll_domain::auth::SessionState;
use sqlx::PgPool;
use uuid::Uuid;

use super::{millis_from_utc, utc_from_millis};

/// A stored session: who it authenticates plus the domain's view of its
/// lifecycle state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionRow {
    /// The authenticated account.
    pub account_id: Uuid,
    /// Lifecycle fields the domain verdict consumes.
    pub state: SessionState,
}

/// Postgres repository for `sessions`.
#[derive(Clone)]
pub struct SessionRepo {
    pool: PgPool,
}

impl SessionRepo {
    /// Wraps the shared connection pool.
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    /// Persists a new session (hash only — the raw token goes in the cookie).
    pub async fn insert(
        &self,
        account_id: Uuid,
        token_hash: [u8; 32],
        now_millis: u64,
        absolute_expires_at_millis: u64,
    ) -> Result<(), sqlx::Error> {
        sqlx::query!(
            "INSERT INTO sessions (id, account_id, token_hash, created_at, last_seen_at, absolute_expires_at)
             VALUES ($1, $2, $3, $4, $4, $5)",
            Uuid::now_v7(),
            account_id,
            &token_hash[..],
            utc_from_millis(now_millis),
            utc_from_millis(absolute_expires_at_millis),
        )
        .execute(&self.pool)
        .await
        .map(|_| ())
    }

    /// Fetches a session by token hash; returns revoked/expired rows too —
    /// judging them is the domain's job.
    pub async fn find(&self, token_hash: [u8; 32]) -> Result<Option<SessionRow>, sqlx::Error> {
        let row = sqlx::query!(
            "SELECT account_id, last_seen_at, absolute_expires_at, revoked_at
             FROM sessions WHERE token_hash = $1",
            &token_hash[..],
        )
        .fetch_optional(&self.pool)
        .await?;

        Ok(row.map(|r| SessionRow {
            account_id: r.account_id,
            state: SessionState {
                last_seen_at_millis: millis_from_utc(r.last_seen_at),
                absolute_expires_at_millis: millis_from_utc(r.absolute_expires_at),
                revoked_at_millis: r.revoked_at.map(millis_from_utc),
            },
        }))
    }

    /// Revokes a session server-side (sign-out). Idempotent — reports whether
    /// THIS call was the one that actually revoked it (`true`) versus a
    /// no-op on an already-revoked session (`false`, e.g. a concurrent racing
    /// repeat), so a caller can avoid double-counting a one-time event: the
    /// audit `signed-out` row must be written once per revocation, not once per
    /// request.
    pub async fn revoke(&self, token_hash: [u8; 32]) -> Result<bool, sqlx::Error> {
        let result = sqlx::query!(
            "UPDATE sessions SET revoked_at = now()
             WHERE token_hash = $1 AND revoked_at IS NULL",
            &token_hash[..],
        )
        .execute(&self.pool)
        .await?;
        Ok(result.rows_affected() > 0)
    }

    /// Advances the sliding idle cursor.
    pub async fn touch(&self, token_hash: [u8; 32], now_millis: u64) -> Result<(), sqlx::Error> {
        sqlx::query!(
            "UPDATE sessions SET last_seen_at = $2 WHERE token_hash = $1",
            &token_hash[..],
            utc_from_millis(now_millis),
        )
        .execute(&self.pool)
        .await
        .map(|_| ())
    }

    /// Deletes every session that died before `now_millis - retention_millis`,
    /// returning how many rows went.
    ///
    /// Two arms, and the `revoked_at` one is REQUIRED rather than
    /// belt-and-braces. The absolute session cap is 30 days, so a session
    /// revoked one minute after sign-in still carries an `absolute_expires_at`
    /// up to 30 days out; without this arm every sign-out and every bulk revoke
    /// (email change, soft-delete, admin disable) would leave its row lingering
    /// for the full original lifetime.
    ///
    /// `last_seen_at` is deliberately ABSENT. Idle expiry (when
    /// `now - last_seen_at` reaches `SESSION_IDLE_MILLIS`) is a domain verdict
    /// evaluated per request against a tunable window, and it is not this
    /// sweep's to own: pruning on it would couple physical row lifetime to a
    /// policy knob, so raising the
    /// idle window later would find rows the new policy considers live already
    /// destroyed. `absolute_expires_at` is the hard cap that can never be
    /// extended for an existing row, so that is what this prunes on. The cost is
    /// that an idle-but-uncapped session outlives its usefulness by up to the
    /// difference between the two windows — the safe direction to err.
    ///
    /// Strict `<` keeps the predicate off the boundary instant, matching the
    /// consume paths' convention of refusing at exactly `expires_at`. The cutoff
    /// comes from the caller's injected clock, never `now()` in SQL.
    pub async fn prune_dead(
        &self,
        now_millis: u64,
        retention_millis: u64,
    ) -> Result<u64, sqlx::Error> {
        let cutoff = utc_from_millis(now_millis.saturating_sub(retention_millis));
        let result = sqlx::query!(
            "DELETE FROM sessions WHERE absolute_expires_at < $1 OR revoked_at < $1",
            cutoff,
        )
        .execute(&self.pool)
        .await?;
        Ok(result.rows_affected())
    }
}
