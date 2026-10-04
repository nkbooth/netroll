// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
//! Storage for per-net session role grants. Mirrors the
//! `net_definition_owners` join-table shape, session-scoped: a grant names
//! exactly one `net_session_id`, so cross-net isolation falls out by
//! construction. Owner is NOT stored here — it is derived from the definition
//! owner set; this repo holds only the granted staff tiers.

use std::str::FromStr;

use netroll_domain::authz::Role;
use sqlx::PgPool;
use uuid::Uuid;

use super::millis_from_utc;

/// One explicit role grant paired with its target's callsign for display.
/// The roles-list read joins `net_session_roles` to `accounts`
/// so the console can show WHO holds a role by their ham-facing callsign, not a
/// bare account id. `callsign` is `None` for an account that has not reserved
/// one (callsign is nullable); `granted_by` is `None` when the grantor's
/// account was later deleted (the FK is `ON DELETE SET NULL`). Owner is
/// NOT represented here — it is derived from the definition owner set, never a
/// stored grant, so this is the set of EXPLICIT grants only.
#[derive(Debug, PartialEq, Eq)]
pub struct RoleGrant {
    /// The account the role is granted to.
    pub account_id: Uuid,
    /// The target account's reserved callsign, or `None` if it holds none.
    pub callsign: Option<String>,
    /// The granted role (never `Owner`).
    pub role: Role,
    /// The account that made the grant, or `None` if that account was deleted.
    pub granted_by: Option<Uuid>,
    /// When the grant was created, as epoch millis (the adapter seam converts
    /// the stored `timestamptz` to millis; the HTTP layer renders RFC 3339).
    pub granted_at_millis: u64,
}

/// Postgres repository for `net_session_roles`.
#[derive(Clone)]
pub struct NetSessionRoleRepo {
    pool: PgPool,
}

/// The outcome of [`NetSessionRoleRepo::revoke`] — success names whether a
/// grant was actually removed, so the API can answer a revoke of a
/// non-existent grant with a 404 rather than a silent success (mirroring
/// `RemoveOwnerOutcome`).
#[derive(Debug, PartialEq, Eq)]
pub enum RevokeOutcome {
    /// A grant existed for `(session, account)` and was deleted.
    Revoked,
    /// No grant existed for `(session, account)`.
    NotAMember,
}

impl NetSessionRoleRepo {
    /// Wraps the shared connection pool.
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    /// Grants `role` to `account_id` on `session_id`, recording `granted_by`.
    ///
    /// Upserts on the `(net_session_id, account_id)` key: a re-grant overwrites
    /// the existing role (one explicit role per account per session).
    pub async fn grant(
        &self,
        session_id: Uuid,
        account_id: Uuid,
        role: Role,
        granted_by: Uuid,
    ) -> Result<(), sqlx::Error> {
        sqlx::query!(
            "INSERT INTO net_session_roles (net_session_id, account_id, role, granted_by)
             VALUES ($1, $2, $3, $4)
             ON CONFLICT (net_session_id, account_id)
             DO UPDATE SET role = EXCLUDED.role, granted_by = EXCLUDED.granted_by",
            session_id,
            account_id,
            role.as_str(),
            granted_by,
        )
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Revokes any role for `(session_id, account_id)`. Reports whether a grant
    /// was actually removed.
    pub async fn revoke(
        &self,
        session_id: Uuid,
        account_id: Uuid,
    ) -> Result<RevokeOutcome, sqlx::Error> {
        let result = sqlx::query!(
            "DELETE FROM net_session_roles WHERE net_session_id = $1 AND account_id = $2",
            session_id,
            account_id,
        )
        .execute(&self.pool)
        .await?;
        Ok(if result.rows_affected() > 0 {
            RevokeOutcome::Revoked
        } else {
            RevokeOutcome::NotAMember
        })
    }

    /// Revokes the grant for `(session_id, account_id)` ONLY if its current
    /// role still equals `expected_role` — a compare-and-delete that closes a
    /// TOCTOU window in the HTTP handler: it reads the current role, decides
    /// `can_manage_role(actor, existing)` from that read, then calls this to
    /// act. An unconditional `DELETE` (the plain [`revoke`](Self::revoke))
    /// would remove whatever role is present at delete time even if a
    /// concurrent [`grant`](Self::grant) changed it after the authorization
    /// decision — e.g. an NCS permitted to revoke a Logger must not end up
    /// deleting a NetControl grant that landed on the same row a moment
    /// later. A mismatch reports [`RevokeOutcome::NotAMember`], the same
    /// signal a sequential re-revoke gets: the caller cannot (and need not)
    /// distinguish "already gone" from "changed out from under the check."
    pub async fn revoke_if_role(
        &self,
        session_id: Uuid,
        account_id: Uuid,
        expected_role: Role,
    ) -> Result<RevokeOutcome, sqlx::Error> {
        let result = sqlx::query!(
            "DELETE FROM net_session_roles
             WHERE net_session_id = $1 AND account_id = $2 AND role = $3",
            session_id,
            account_id,
            expected_role.as_str(),
        )
        .execute(&self.pool)
        .await?;
        Ok(if result.rows_affected() > 0 {
            RevokeOutcome::Revoked
        } else {
            RevokeOutcome::NotAMember
        })
    }

    /// The granted role for `(session_id, account_id)`, or `None` when the
    /// account holds no grant on that session — the scoped lookup that makes
    /// cross-net isolation free.
    pub async fn find_role(
        &self,
        session_id: Uuid,
        account_id: Uuid,
    ) -> Result<Option<Role>, sqlx::Error> {
        let row = sqlx::query!(
            "SELECT role FROM net_session_roles
             WHERE net_session_id = $1 AND account_id = $2",
            session_id,
            account_id,
        )
        .fetch_optional(&self.pool)
        .await?;

        match row {
            // A stored value that no longer names a known role is a
            // data-integrity fault, surfaced as a decode error rather than
            // silently dropped.
            Some(r) => Ok(Some(
                Role::from_str(&r.role).map_err(|e| sqlx::Error::Decode(Box::new(e)))?,
            )),
            None => Ok(None),
        }
    }

    /// The EXPLICIT role grants on `session_id`, each paired with its target's
    /// callsign for display. Joins
    /// `net_session_roles` to `accounts` for the callsign (mirroring
    /// `owners_with_callsign`), stably ordered by `(created_at, account_id)` so
    /// the console list never reshuffles (the owner-list tiebreaker precedent). Owner
    /// is never stored here, so the result is the granted staff tiers only.
    pub async fn list_grants(&self, session_id: Uuid) -> Result<Vec<RoleGrant>, sqlx::Error> {
        let rows = sqlx::query!(
            "SELECT r.account_id, a.callsign, r.role, r.granted_by, r.created_at
             FROM net_session_roles r
             JOIN accounts a ON a.id = r.account_id
             WHERE r.net_session_id = $1
             ORDER BY r.created_at, r.account_id",
            session_id,
        )
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter()
            .map(|r| {
                Ok(RoleGrant {
                    account_id: r.account_id,
                    callsign: r.callsign,
                    // A stored value that no longer names a known role is a
                    // data-integrity fault, surfaced as a decode error.
                    role: Role::from_str(&r.role).map_err(|e| sqlx::Error::Decode(Box::new(e)))?,
                    granted_by: r.granted_by,
                    granted_at_millis: millis_from_utc(r.created_at),
                })
            })
            .collect()
    }
}
