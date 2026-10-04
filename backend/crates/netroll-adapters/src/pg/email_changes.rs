// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
//! Single-use email-change confirmation tokens. Only the SHA-256 hash is ever
//! written; the raw token lives solely in the emailed link. Unlike a magic
//! link, consuming one never creates an account — it retargets an existing
//! one, and the swap, re-verification and session revocation are ONE
//! transaction.

use netroll_domain::auth::EmailChangeToken;
use sqlx::PgPool;
use uuid::Uuid;

use super::{millis_from_utc, utc_from_millis};

/// Result of attempting to confirm (consume) an email-change token.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfirmEmailChangeOutcome {
    /// The account's email was swapped to `new_email`, re-verified, and all
    /// of its sessions revoked.
    Changed {
        /// The account whose identifying email changed.
        account_id: Uuid,
        /// The address the account held immediately before this swap (read
        /// inside the same transaction as the swap, so a sibling pending
        /// token for the same account being confirmed concurrently can
        /// never make this stale — the courtesy notice always names the
        /// address truly replaced by THIS confirm).
        old_email: String,
        /// The address the account now identifies by.
        new_email: String,
    },
    /// The target address is already held by another account — the whole
    /// transaction rolled back (email unchanged, token still unconsumed).
    EmailTaken,
    /// The token was already consumed, expired at the injected `now`, or
    /// never existed — nothing was written.
    NotConsumable,
}

/// Postgres repository for `email_change_tokens`.
#[derive(Clone)]
pub struct EmailChangeRepo {
    pool: PgPool,
}

impl EmailChangeRepo {
    /// Wraps the shared connection pool.
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    /// Records a freshly issued confirmation token retargeting `account_id`
    /// to `new_email` (already normalized).
    pub async fn issue(
        &self,
        account_id: Uuid,
        new_email: &str,
        token_hash: [u8; 32],
        expires_at_millis: u64,
    ) -> Result<(), sqlx::Error> {
        sqlx::query!(
            "INSERT INTO email_change_tokens (id, account_id, new_email, token_hash, expires_at)
             VALUES ($1, $2, $3, $4, $5)",
            Uuid::now_v7(),
            account_id,
            new_email,
            &token_hash[..],
            utc_from_millis(expires_at_millis),
        )
        .execute(&self.pool)
        .await
        .map(|_| ())
    }

    /// Fetches the domain view of a token for verdict evaluation.
    pub async fn find(
        &self,
        token_hash: [u8; 32],
    ) -> Result<Option<EmailChangeToken>, sqlx::Error> {
        let row = sqlx::query!(
            "SELECT account_id, new_email, expires_at, consumed_at
             FROM email_change_tokens WHERE token_hash = $1",
            &token_hash[..],
        )
        .fetch_optional(&self.pool)
        .await?;

        Ok(row.map(|r| EmailChangeToken {
            account_id: r.account_id,
            new_email: r.new_email,
            expires_at_millis: millis_from_utc(r.expires_at),
            consumed_at_millis: r.consumed_at.map(millis_from_utc),
        }))
    }

    /// Confirms a token in ONE transaction: consume it (single-use, atomic),
    /// swap the account's email + re-verify it, and revoke every live
    /// session for that account.
    ///
    /// Expiry is judged against the caller's injected `now_millis`, never the
    /// database clock (the clock-drift posture). A target email already
    /// held by another account rolls the whole transaction back and returns
    /// [`ConfirmEmailChangeOutcome::EmailTaken`], so the token stays
    /// unconsumed and the user can retry once the address frees.
    pub async fn confirm(
        &self,
        token_hash: [u8; 32],
        now_millis: u64,
    ) -> Result<ConfirmEmailChangeOutcome, sqlx::Error> {
        let now = utc_from_millis(now_millis);
        let mut tx = self.pool.begin().await?;

        // Atomic single-use consume, expiry judged against the injected now.
        let consumed = sqlx::query!(
            "UPDATE email_change_tokens SET consumed_at = $2
             WHERE token_hash = $1 AND consumed_at IS NULL AND expires_at > $2
             RETURNING account_id, new_email",
            &token_hash[..],
            now,
        )
        .fetch_optional(&mut *tx)
        .await?;

        // No row consumed: nothing has been written, so there is nothing to
        // roll back — the caller re-judges from storage for the true reason.
        let Some(consumed) = consumed else {
            return Ok(ConfirmEmailChangeOutcome::NotConsumable);
        };

        let account_id = consumed.account_id;
        let new_email = consumed.new_email;

        // Lock the account row and read its CURRENT email in the SAME
        // transaction as the swap below. A plain pre-transaction SELECT
        // (issued by an earlier caller before invoking `confirm`) would race
        // the "old address" snapshot against a sibling pending token for
        // this account being confirmed concurrently — multiple pending
        // tokens per account coexist by design (Dev Notes). `FOR UPDATE`
        // also means a missing row (account gone) is caught here, before
        // any write, rather than silently no-op'ing the swap below.
        let account = sqlx::query!(
            "SELECT email FROM accounts WHERE id = $1 FOR UPDATE",
            account_id,
        )
        .fetch_optional(&mut *tx)
        .await?;
        let Some(account) = account else {
            // The token's account no longer exists — not a successful
            // change (the zero-row-write lesson, applied here as
            // a locked existence check rather than a post-hoc row count).
            tx.rollback().await?;
            return Err(sqlx::Error::RowNotFound);
        };
        let old_email = account.email;

        let swap = sqlx::query!(
            "UPDATE accounts SET email = $2, email_verified_at = $3, updated_at = $3
             WHERE id = $1",
            account_id,
            new_email,
            now,
        )
        .execute(&mut *tx)
        .await;

        if let Err(err) = swap {
            // The target address is already an account's identity — the
            // uniqueness invariant (constraint `accounts_email_key`) is the
            // truth behind the request-time 409. Roll back whole.
            let is_email_conflict =
                err.as_database_error().and_then(|e| e.constraint()) == Some("accounts_email_key");
            if is_email_conflict {
                tx.rollback().await?;
                return Ok(ConfirmEmailChangeOutcome::EmailTaken);
            }
            return Err(err);
        }

        // Every live session dies in the same transaction as the swap:
        // "re-authentication required" — including the session that
        // may be driving this confirm.
        sqlx::query!(
            "UPDATE sessions SET revoked_at = $2 WHERE account_id = $1 AND revoked_at IS NULL",
            account_id,
            now,
        )
        .execute(&mut *tx)
        .await?;

        tx.commit().await?;
        Ok(ConfirmEmailChangeOutcome::Changed {
            account_id,
            old_email,
            new_email,
        })
    }

    /// Deletes every token that expired before `now_millis - retention_millis`,
    /// returning how many rows went.
    ///
    /// `expires_at` alone, for the same reason as
    /// [`MagicLinkRepo::prune_expired`](super::magic_links::MagicLinkRepo::prune_expired):
    /// these tokens reuse the 15-minute magic-link TTL, so a consumed row is
    /// over-retained by at most that against a 30-day window — cheaper than a
    /// second arm that could be mis-ordered.
    ///
    /// This table DOES cascade from `accounts`, so a finalized account's tokens
    /// are already gone. That covers nothing for a live account, which is the
    /// leak this sweep exists for: the cascade fires only on finalization, and
    /// for an account that is never deleted it never fires at all.
    ///
    /// The cutoff comes from the caller's injected clock, never `now()` in SQL.
    pub async fn prune_expired(
        &self,
        now_millis: u64,
        retention_millis: u64,
    ) -> Result<u64, sqlx::Error> {
        let cutoff = utc_from_millis(now_millis.saturating_sub(retention_millis));
        let result = sqlx::query!(
            "DELETE FROM email_change_tokens WHERE expires_at < $1",
            cutoff,
        )
        .execute(&self.pool)
        .await?;
        Ok(result.rows_affected())
    }
}
