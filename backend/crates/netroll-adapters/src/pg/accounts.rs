// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
//! Storage for accounts and their attached auth methods.

use netroll_domain::model::account::{Account, ProfileFields};
use sqlx::PgPool;
use uuid::Uuid;

use super::{millis_from_utc, utc_from_millis};

/// Postgres repository for `accounts` + `auth_methods`.
#[derive(Clone)]
pub struct AccountRepo {
    pool: PgPool,
}

impl AccountRepo {
    /// Wraps the shared connection pool.
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    /// Looks up an account by normalized email.
    pub async fn find_by_email(&self, email: &str) -> Result<Option<Account>, sqlx::Error> {
        let row = sqlx::query!(
            "SELECT id, email, email_verified_at, callsign, display_name, location, grid,
                    avatar_url, deleted_at, disabled_at
             FROM accounts WHERE email = $1",
            email,
        )
        .fetch_optional(&self.pool)
        .await?;

        Ok(row.map(|r| Account {
            id: r.id,
            email: r.email,
            email_verified_at_millis: r.email_verified_at.map(millis_from_utc),
            callsign: r.callsign,
            display_name: r.display_name,
            location: r.location,
            grid: r.grid,
            avatar_url: r.avatar_url,
            deleted_at_millis: r.deleted_at.map(millis_from_utc),
            disabled_at_millis: r.disabled_at.map(millis_from_utc),
        }))
    }

    /// Looks up an account by its (already-normalized) callsign — the resolver
    /// behind adding a co-owner by callsign. Trusts its caller for
    /// normalization (the handler parses via `parse_callsign` first), exactly
    /// as [`AccountRepo::set_callsign`] does. A callsign no account holds → `None`.
    pub async fn find_by_callsign(&self, callsign: &str) -> Result<Option<Account>, sqlx::Error> {
        let row = sqlx::query!(
            "SELECT id, email, email_verified_at, callsign, display_name, location, grid,
                    avatar_url, deleted_at, disabled_at
             FROM accounts WHERE callsign = $1",
            callsign,
        )
        .fetch_optional(&self.pool)
        .await?;

        Ok(row.map(|r| Account {
            id: r.id,
            email: r.email,
            email_verified_at_millis: r.email_verified_at.map(millis_from_utc),
            callsign: r.callsign,
            display_name: r.display_name,
            location: r.location,
            grid: r.grid,
            avatar_url: r.avatar_url,
            deleted_at_millis: r.deleted_at.map(millis_from_utc),
            disabled_at_millis: r.disabled_at.map(millis_from_utc),
        }))
    }

    /// Looks up an account by id.
    pub async fn find_by_id(&self, id: Uuid) -> Result<Option<Account>, sqlx::Error> {
        let row = sqlx::query!(
            "SELECT id, email, email_verified_at, callsign, display_name, location, grid,
                    avatar_url, deleted_at, disabled_at
             FROM accounts WHERE id = $1",
            id,
        )
        .fetch_optional(&self.pool)
        .await?;

        Ok(row.map(|r| Account {
            id: r.id,
            email: r.email,
            email_verified_at_millis: r.email_verified_at.map(millis_from_utc),
            callsign: r.callsign,
            display_name: r.display_name,
            location: r.location,
            grid: r.grid,
            avatar_url: r.avatar_url,
            deleted_at_millis: r.deleted_at.map(millis_from_utc),
            disabled_at_millis: r.disabled_at.map(millis_from_utc),
        }))
    }

    /// Find-or-create the account for `email`, mark the email verified, and
    /// attach the magic-link method — one transaction, executed only at
    /// link-consume time (the consumed link is the proof of email control).
    ///
    /// Concurrency-safe: `INSERT ... ON CONFLICT DO NOTHING` + select, so two
    /// racing first sign-ins converge on the same account.
    pub async fn create_verified_and_attach(
        &self,
        email: &str,
        now_millis: u64,
    ) -> Result<Account, sqlx::Error> {
        let now = utc_from_millis(now_millis);
        let mut tx = self.pool.begin().await?;

        sqlx::query!(
            "INSERT INTO accounts (id, email, created_at, updated_at)
             VALUES ($1, $2, $3, $3) ON CONFLICT (email) DO NOTHING",
            Uuid::now_v7(),
            email,
            now,
        )
        .execute(&mut *tx)
        .await?;

        let row = sqlx::query!(
            "UPDATE accounts
             SET email_verified_at = COALESCE(email_verified_at, $2), updated_at = $2,
                 deleted_at = NULL
             WHERE email = $1
             RETURNING id, email, email_verified_at, callsign, display_name, location, grid,
                       avatar_url, deleted_at, disabled_at",
            email,
            now,
        )
        .fetch_one(&mut *tx)
        .await?;

        attach_magic_link(&mut tx, row.id, now).await?;
        tx.commit().await?;

        Ok(Account {
            id: row.id,
            email: row.email,
            email_verified_at_millis: row.email_verified_at.map(millis_from_utc),
            callsign: row.callsign,
            display_name: row.display_name,
            location: row.location,
            grid: row.grid,
            avatar_url: row.avatar_url,
            deleted_at_millis: row.deleted_at.map(millis_from_utc),
            disabled_at_millis: row.disabled_at.map(millis_from_utc),
        })
    }

    /// Marks an existing account's email verified and attaches the
    /// magic-link method (idempotently). Callers must hold proof of email
    /// control — the linking invariant is decided in domain, never here.
    pub async fn verify_and_attach(
        &self,
        account_id: Uuid,
        now_millis: u64,
    ) -> Result<(), sqlx::Error> {
        let now = utc_from_millis(now_millis);
        let mut tx = self.pool.begin().await?;

        // `deleted_at = NULL` makes ANY successful sign-in clear a pending
        // deletion — this IS the undelete mechanism for an in-grace account.
        // A live account's `deleted_at` is already NULL, so the
        // clear is a harmless no-op.
        sqlx::query!(
            "UPDATE accounts
             SET email_verified_at = COALESCE(email_verified_at, $2), updated_at = $2,
                 deleted_at = NULL
             WHERE id = $1",
            account_id,
            now,
        )
        .execute(&mut *tx)
        .await?;

        attach_magic_link(&mut tx, account_id, now).await?;
        tx.commit().await?;
        Ok(())
    }

    /// Reserves or changes `account_id`'s callsign to `callsign` (already
    /// parsed/normalized by [`netroll_domain::callsign::parse_callsign`] —
    /// this method trusts its caller for format).
    ///
    /// A single `UPDATE` is the whole invariant: the old value
    /// is freed and the new one claimed atomically in one statement, so no
    /// explicit `BEGIN` is needed — there is no window between "check" and
    /// "write" for another request to land in, because there is no check.
    /// The partial unique index (`idx_accounts_callsign`) is the only source
    /// of truth for uniqueness; a violation is mapped to
    /// [`SetCallsignOutcome::Taken`] rather than propagated as a raw
    /// `sqlx::Error`, so callers never need to know the constraint name.
    pub async fn set_callsign(
        &self,
        account_id: Uuid,
        callsign: &str,
        now_millis: u64,
    ) -> Result<SetCallsignOutcome, sqlx::Error> {
        let now = utc_from_millis(now_millis);
        let result = sqlx::query!(
            "UPDATE accounts SET callsign = $2, updated_at = $3 WHERE id = $1",
            account_id,
            callsign,
            now,
        )
        .execute(&self.pool)
        .await;

        match result {
            // `account_id` is caller-supplied; a zero-row UPDATE means no such
            // account exists (or vanished mid-request). That is not a
            // successful claim and must not be reported as one.
            Ok(result) if result.rows_affected() == 1 => Ok(SetCallsignOutcome::Reserved),
            Ok(_) => Err(sqlx::Error::RowNotFound),
            Err(err) => {
                let is_callsign_conflict = err.as_database_error().and_then(|e| e.constraint())
                    == Some("idx_accounts_callsign");
                if is_callsign_conflict {
                    Ok(SetCallsignOutcome::Taken)
                } else {
                    Err(err)
                }
            }
        }
    }

    /// Replaces `account_id`'s profile fields with `fields` (already
    /// validated by `netroll_domain::profile`'s parse functions —
    /// this method trusts its caller for format, same contract as
    /// [`AccountRepo::set_callsign`]).
    ///
    /// PUT-replace semantics: one plain `UPDATE` writes exactly what it is
    /// given — a `None` field nulls its column, no COALESCE. No uniqueness,
    /// so no conflict mapping. A zero-row UPDATE means the account does not
    /// exist and surfaces as `RowNotFound`, never silent success.
    pub async fn update_profile(
        &self,
        account_id: Uuid,
        fields: &ProfileFields,
        now_millis: u64,
    ) -> Result<(), sqlx::Error> {
        let now = utc_from_millis(now_millis);
        let result = sqlx::query!(
            "UPDATE accounts
             SET display_name = $2, location = $3, grid = $4, avatar_url = $5, updated_at = $6
             WHERE id = $1",
            account_id,
            fields.display_name.as_deref(),
            fields.location.as_deref(),
            fields.grid.as_deref(),
            fields.avatar_url.as_deref(),
            now,
        )
        .execute(&self.pool)
        .await?;

        if result.rows_affected() == 1 {
            Ok(())
        } else {
            Err(sqlx::Error::RowNotFound)
        }
    }

    /// Soft-deletes `account_id` into the pending-deletion window and revokes
    /// every live session for it, in ONE transaction. Signing the user out
    /// everywhere is what makes the state "pending deletion" rather than a flag
    /// on a still-usable account.
    ///
    /// Idempotent and window-safe: re-deleting an already-pending account is
    /// [`SoftDeleteOutcome::AlreadyPending`] and does NOT move the original
    /// `deleted_at` (the `WHERE ... deleted_at IS NULL` guard), so a repeat
    /// cannot extend the grace window. A genuinely absent account (never a
    /// no-op) surfaces as [`sqlx::Error::RowNotFound`], disambiguated from
    /// the idempotent case by an in-transaction existence probe.
    pub async fn soft_delete(
        &self,
        account_id: Uuid,
        now_millis: u64,
    ) -> Result<SoftDeleteOutcome, sqlx::Error> {
        let now = utc_from_millis(now_millis);
        let mut tx = self.pool.begin().await?;

        let marked = sqlx::query!(
            "UPDATE accounts SET deleted_at = $2, updated_at = $2
             WHERE id = $1 AND deleted_at IS NULL
             RETURNING id",
            account_id,
            now,
        )
        .fetch_optional(&mut *tx)
        .await?;

        let outcome = match marked {
            Some(_) => SoftDeleteOutcome::Deleted,
            None => {
                // Either already pending, or no such account — distinguish
                // so an absent account is never reported as a successful
                // idempotent delete (the zero-row-write lesson).
                let exists = sqlx::query!("SELECT id FROM accounts WHERE id = $1", account_id)
                    .fetch_optional(&mut *tx)
                    .await?;
                if exists.is_none() {
                    tx.rollback().await?;
                    return Err(sqlx::Error::RowNotFound);
                }
                SoftDeleteOutcome::AlreadyPending
            }
        };

        // Revoke every live session on both paths (a re-delete re-asserting
        // "no live sessions" is harmless) — the `EmailChangeRepo::confirm`
        // bulk-revoke precedent.
        sqlx::query!(
            "UPDATE sessions SET revoked_at = $2 WHERE account_id = $1 AND revoked_at IS NULL",
            account_id,
            now,
        )
        .execute(&mut *tx)
        .await?;

        tx.commit().await?;
        Ok(outcome)
    }

    /// Hard-deletes every pending account whose grace window has elapsed
    /// (`deleted_at <= now - grace`), returning how many were removed. Child
    /// rows in `auth_methods`/`sessions`/
    /// `account_consents`/`email_change_tokens` cascade via the migration
    /// FKs. Expiry is judged against the caller's injected `now`, never the
    /// database clock (the clock-drift posture).
    ///
    /// Skips a `disabled_at`-set account: "disabling does not hard-delete
    /// data" would otherwise be silently undone the moment an
    /// independently-running self-deletion
    /// grace window elapses — hard-deleting the row erases `disabled_at`
    /// entirely and frees the email for a fresh signup, with no `reenable`
    /// action ever having happened. A disabled account's row is retained
    /// until an admin explicitly reenables it; the ordinary grace-window
    /// deletion resumes from there if the account (now re-enabled) is later
    /// re-deleted or was already pending.
    pub async fn finalize_deletions(
        &self,
        now_millis: u64,
        grace_millis: u64,
    ) -> Result<u64, sqlx::Error> {
        let erased = self
            .finalize_deletions_erasing(now_millis, grace_millis)
            .await?;
        Ok(erased.len() as u64)
    }

    /// The same sweep, reporting each erased account's `avatar_url` so the
    /// caller can delete the uploaded file too.
    ///
    /// Blob storage has no foreign keys: without this, hard-deleting the row
    /// would leave the account's avatar image on the volume forever — a privacy
    /// problem (the erased user's photo survives their erasure), not just wasted
    /// disk. `RETURNING` rather than a pre-SELECT so the read and the delete
    /// cannot disagree under a concurrent sweep.
    pub async fn finalize_deletions_erasing(
        &self,
        now_millis: u64,
        grace_millis: u64,
    ) -> Result<Vec<Option<String>>, sqlx::Error> {
        let cutoff = utc_from_millis(now_millis.saturating_sub(grace_millis));
        let rows = sqlx::query!(
            "DELETE FROM accounts
             WHERE deleted_at IS NOT NULL AND deleted_at <= $1 AND disabled_at IS NULL
             RETURNING avatar_url",
            cutoff,
        )
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.into_iter().map(|row| row.avatar_url).collect())
    }

    /// Hard-deletes one pending account by id, returning whether a row was
    /// removed. Used by the sign-in finalize-on-access branch.
    /// The `deleted_at IS NOT NULL` guard prevents finalizing a live account
    /// by mistake; children cascade via the migration FKs. Also skips a
    /// `disabled_at`-set account, for the same reason as
    /// [`Self::finalize_deletions`]: finalize-on-access must not erase an
    /// admin disable either, even though the caller already refuses the
    /// sign-in itself once the account is found disabled.
    pub async fn finalize_account(&self, account_id: Uuid) -> Result<bool, sqlx::Error> {
        Ok(self.finalize_account_erasing(account_id).await?.is_some())
    }

    /// The same finalize-on-access delete, reporting the erased account's
    /// `avatar_url` (outer `None` = no row removed) so the caller can delete the
    /// uploaded file. See [`Self::finalize_deletions_erasing`] for why the blob
    /// must go with the row.
    pub async fn finalize_account_erasing(
        &self,
        account_id: Uuid,
    ) -> Result<Option<Option<String>>, sqlx::Error> {
        let row = sqlx::query!(
            "DELETE FROM accounts
             WHERE id = $1 AND deleted_at IS NOT NULL AND disabled_at IS NULL
             RETURNING avatar_url",
            account_id,
        )
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(|row| row.avatar_url))
    }

    /// Disables `account_id` (admin enforcement) and revokes
    /// every live session for it — ONE transaction. Mirrors [`Self::soft_delete`]'s
    /// "set-mark + bulk-revoke-sessions in one tx" shape, but WITHOUT any
    /// `deleted_at`/hard-delete semantics: `disabled_at` is a separate column an
    /// admin `reenable` clears, never a self-sign-in.
    ///
    /// Idempotent: re-disabling an already-disabled account is
    /// [`DisableOutcome::AlreadyDisabled`] and leaves the original `disabled_at`
    /// unmoved (the `WHERE ... disabled_at IS NULL` guard). A genuinely absent
    /// account surfaces as [`sqlx::Error::RowNotFound`], disambiguated by an
    /// in-transaction existence probe (the `soft_delete` posture).
    pub async fn disable(
        &self,
        account_id: Uuid,
        now_millis: u64,
        reason: Option<&str>,
    ) -> Result<DisableOutcome, sqlx::Error> {
        let now = utc_from_millis(now_millis);
        let mut tx = self.pool.begin().await?;

        let marked = sqlx::query!(
            "UPDATE accounts SET disabled_at = $2, disabled_reason = $3, updated_at = $2
             WHERE id = $1 AND disabled_at IS NULL
             RETURNING id",
            account_id,
            now,
            reason,
        )
        .fetch_optional(&mut *tx)
        .await?;

        let outcome = match marked {
            Some(_) => DisableOutcome::Disabled,
            None => {
                let exists = sqlx::query!("SELECT id FROM accounts WHERE id = $1", account_id)
                    .fetch_optional(&mut *tx)
                    .await?;
                if exists.is_none() {
                    tx.rollback().await?;
                    return Err(sqlx::Error::RowNotFound);
                }
                DisableOutcome::AlreadyDisabled
            }
        };

        // Revoke every live session on both paths (a re-disable re-asserting
        // "no live sessions" is harmless) — the `soft_delete` bulk-revoke.
        sqlx::query!(
            "UPDATE sessions SET revoked_at = $2 WHERE account_id = $1 AND revoked_at IS NULL",
            account_id,
            now,
        )
        .execute(&mut *tx)
        .await?;

        tx.commit().await?;
        Ok(outcome)
    }

    /// Re-enables a disabled account — the ONLY path that clears
    /// `disabled_at`. Idempotent: re-enabling a live account is
    /// [`ReenableOutcome::AlreadyEnabled`]. A genuinely absent account surfaces
    /// as [`sqlx::Error::RowNotFound`]. Does NOT restore sessions — the account
    /// signs back in normally (its `deleted_at` is untouched throughout).
    pub async fn reenable(
        &self,
        account_id: Uuid,
        now_millis: u64,
    ) -> Result<ReenableOutcome, sqlx::Error> {
        let now = utc_from_millis(now_millis);
        let cleared = sqlx::query!(
            "UPDATE accounts SET disabled_at = NULL, disabled_reason = NULL, updated_at = $2
             WHERE id = $1 AND disabled_at IS NOT NULL
             RETURNING id",
            account_id,
            now,
        )
        .fetch_optional(&self.pool)
        .await?;

        match cleared {
            Some(_) => Ok(ReenableOutcome::Reenabled),
            None => {
                let exists = sqlx::query!("SELECT id FROM accounts WHERE id = $1", account_id)
                    .fetch_optional(&self.pool)
                    .await?;
                if exists.is_none() {
                    Err(sqlx::Error::RowNotFound)
                } else {
                    Ok(ReenableOutcome::AlreadyEnabled)
                }
            }
        }
    }

    /// Whether `account_id` is currently disabled — the cheap,
    /// PK-indexed read `require_session` runs to refuse a disabled account's
    /// surviving session without loading the whole `Account`. A non-existent
    /// account reads as not-disabled (the caller's session/account checks handle
    /// absence).
    pub async fn is_disabled(&self, account_id: Uuid) -> Result<bool, sqlx::Error> {
        let row = sqlx::query!(
            "SELECT EXISTS(SELECT 1 FROM accounts WHERE id = $1 AND disabled_at IS NOT NULL)
                 AS \"disabled!\"",
            account_id,
        )
        .fetch_one(&self.pool)
        .await?;
        Ok(row.disabled)
    }
}

/// Result of attempting to reserve/change a callsign.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SetCallsignOutcome {
    /// The callsign is now held by the requesting account.
    Reserved,
    /// Another account already holds this (normalized) callsign.
    Taken,
}

/// Outcome of an [`AccountRepo::soft_delete`] call. The handler treats both as a
/// successful 204 (idempotent DELETE); the distinction exists so a future
/// audit line can tell a real deletion from a repeat.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SoftDeleteOutcome {
    /// The account was newly marked pending-deletion by this call.
    Deleted,
    /// The account was already pending; this call left `deleted_at` unmoved.
    AlreadyPending,
}

/// Outcome of an [`AccountRepo::disable`] call. The handler treats
/// both as a successful disable (idempotent); the distinction lets the audit
/// line tell a real disable from a repeat.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DisableOutcome {
    /// The account was newly disabled by this call.
    Disabled,
    /// The account was already disabled; this call left `disabled_at` unmoved.
    AlreadyDisabled,
}

/// Outcome of an [`AccountRepo::reenable`] call. Both are a
/// successful re-enable (idempotent).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReenableOutcome {
    /// The account was disabled and is now live again.
    Reenabled,
    /// The account was already live; this call cleared nothing.
    AlreadyEnabled,
}

async fn attach_magic_link(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    account_id: Uuid,
    now: chrono::DateTime<chrono::Utc>,
) -> Result<(), sqlx::Error> {
    sqlx::query!(
        "INSERT INTO auth_methods (id, account_id, kind, created_at, last_used_at)
         VALUES ($1, $2, 'magic-link', $3, $3)
         ON CONFLICT (account_id, kind) DO UPDATE SET last_used_at = $3",
        Uuid::now_v7(),
        account_id,
        now,
    )
    .execute(&mut **tx)
    .await
    .map(|_| ())
}
