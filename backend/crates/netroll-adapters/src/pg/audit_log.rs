// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
//! The generic, append-only audit log: every event source shares one table and
//! one port, and there is no UPDATE or DELETE. PII/secret-free holds at the
//! DEDICATED-COLUMN level only — `metadata` is an open `serde_json::Value`, so
//! that is caller discipline checked against today's callers, never a
//! type-level guarantee.

use netroll_domain::admin::PageCursor;
use serde_json::Value;
use sqlx::PgPool;
use uuid::Uuid;

use super::{Page, into_page, millis_from_utc, utc_from_millis};

/// One audit-log record to append. The value type is intentionally narrow so it
/// CANNOT carry a secret or PII: no email/token/credential field exists on it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuditEntry {
    /// The account that performed the action (an admin, for an admin action).
    pub actor_account_id: Uuid,
    /// Stable lowercase-kebab action verb (e.g. `disable-account`) — the
    /// [`netroll_domain::admin::AdminCapability::as_str`] spelling.
    pub action: String,
    /// What kind of thing was targeted (e.g. `account`, `abuse-report`).
    pub target_type: Option<String>,
    /// The targeted entity's id, when it has one.
    pub target_id: Option<Uuid>,
    /// Optional bounded, non-secret structured context. Never PII/secrets.
    pub metadata: Option<Value>,
    /// The net session this action happened IN, when it happened in one.
    ///
    /// Distinct from [`AuditEntry::target_id`], which is WHAT was acted on: a
    /// role grant targets the grantee ACCOUNT but occurs in a session. Without
    /// this, "everything that touched this session" would silently miss every
    /// role change.
    pub context_session_id: Option<Uuid>,
    /// The net definition this action concerns, when it concerns one.
    ///
    /// Set on session events too, so "everything that touched this net" reaches
    /// its sessions without the caller first knowing all of their ids.
    pub context_definition_id: Option<Uuid>,
}

/// An appended audit record as read back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredAuditEntry {
    /// UUIDv7 primary key.
    pub id: Uuid,
    /// When the action occurred, epoch millis.
    pub occurred_at_millis: u64,
    /// The acting account.
    pub actor_account_id: Uuid,
    /// The action verb.
    pub action: String,
    /// The target kind, when set.
    pub target_type: Option<String>,
    /// The target id, when set.
    pub target_id: Option<Uuid>,
    /// The non-secret metadata blob, when set.
    pub metadata: Option<Value>,
    /// The session this action happened in, when it happened in one.
    pub context_session_id: Option<Uuid>,
    /// The net definition this action concerns, when it concerns one.
    pub context_definition_id: Option<Uuid>,
}

/// Postgres repository for `audit_log`. Append-only.
#[derive(Clone)]
pub struct AuditLogRepo {
    pool: PgPool,
}

impl AuditLogRepo {
    /// Wraps the shared connection pool.
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    /// Appends one audit record, returning its generated id. Times enter as the
    /// caller's injected `now` (never the DB clock). There is no update or
    /// delete counterpart — the log is append-only.
    pub async fn append(&self, entry: &AuditEntry, now_millis: u64) -> Result<Uuid, sqlx::Error> {
        let id = Uuid::now_v7();
        sqlx::query!(
            "INSERT INTO audit_log
                 (id, occurred_at, actor_account_id, action, target_type, target_id, metadata,
                  context_session_id, context_definition_id)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)",
            id,
            utc_from_millis(now_millis),
            entry.actor_account_id,
            entry.action,
            entry.target_type,
            entry.target_id,
            entry.metadata,
            entry.context_session_id,
            entry.context_definition_id,
        )
        .execute(&self.pool)
        .await?;
        Ok(id)
    }

    /// One page of the audit log, newest first — the only whole-log read this
    /// repo offers, because the admin dashboard's review surface must not
    /// serialize the entire (append-only, ever-growing) history per view.
    ///
    /// Keyset, not offset: the log only ever grows at the newest end, so an
    /// append between two page fetches cannot shift an older row past a
    /// boundary. The `id` tiebreak is load-bearing — `occurred_at` alone is not
    /// unique.
    pub async fn list_page(
        &self,
        limit: usize,
        cursor: Option<PageCursor>,
    ) -> Result<Page<StoredAuditEntry>, sqlx::Error> {
        let before_at = cursor.map(|c| utc_from_millis(c.at_millis));
        let before_id = cursor.map(|c| c.id);
        let rows = sqlx::query!(
            "SELECT id, occurred_at, actor_account_id, action, target_type, target_id, metadata,
                    context_session_id, context_definition_id
             FROM audit_log
             WHERE ($1::timestamptz IS NULL OR (occurred_at, id) < ($1, $2))
             ORDER BY occurred_at DESC, id DESC
             LIMIT $3",
            before_at,
            before_id,
            // Over-fetch by one to learn whether a further page exists.
            (limit as i64) + 1,
        )
        .fetch_all(&self.pool)
        .await?;

        let rows: Vec<StoredAuditEntry> = rows
            .into_iter()
            .map(|r| StoredAuditEntry {
                id: r.id,
                occurred_at_millis: millis_from_utc(r.occurred_at),
                actor_account_id: r.actor_account_id,
                action: r.action,
                target_type: r.target_type,
                target_id: r.target_id,
                metadata: r.metadata,
                context_session_id: r.context_session_id,
                context_definition_id: r.context_definition_id,
            })
            .collect();
        Ok(into_page(rows, limit, |e| (e.occurred_at_millis, e.id)))
    }

    /// One page of the audit log narrowed by any combination of actor, action,
    /// and object — the admin review surface's investigation filters.
    ///
    /// `object` matches an audit row three ways: as the thing acted ON
    /// (`target_id`), as the session it happened IN (`context_session_id`), or
    /// as the net it concerns (`context_definition_id`). One uuid therefore
    /// answers "everything that touched this" whether it names an account, a
    /// report, a session, or a net — including role grants, whose `target_id`
    /// is the grantee account rather than the session.
    ///
    /// # Why `UNION ALL` and not one `OR`
    ///
    /// A three-way `OR` does not force a sequential scan — Postgres builds a
    /// `BitmapOr` across the indexes — but a bitmap scan **discards ordering**.
    /// With `ORDER BY occurred_at DESC, id DESC LIMIT n+1` that means the whole
    /// matching set is sorted before the limit applies, and the composite
    /// indexes' ordering is wasted. Two separately-ordered branches each take
    /// their own top-(n+1) via their own index, and the outer sort merges two
    /// tiny inputs.
    ///
    /// The over-fetch probe stays exact: the global top-(n+1) is always
    /// contained in the union of the per-branch top-(n+1)s, so `into_page` can
    /// still decide `next` from the extra row. The
    /// `target_id IS DISTINCT FROM` guard makes the branches disjoint, which is
    /// what lets this be a cheap `UNION ALL` rather than a `UNION` that would
    /// have to hash whole rows (including the jsonb) to deduplicate.
    ///
    /// When `object` is `None` the second branch is provably empty and folds
    /// away, leaving the same plan as [`AuditLogRepo::list_page`].
    pub async fn list_page_filtered(
        &self,
        limit: usize,
        cursor: Option<PageCursor>,
        actor: Option<Uuid>,
        action: Option<&str>,
        object: Option<Uuid>,
    ) -> Result<Page<StoredAuditEntry>, sqlx::Error> {
        let before_at = cursor.map(|c| utc_from_millis(c.at_millis));
        let before_id = cursor.map(|c| c.id);
        let over_fetch = (limit as i64) + 1;
        let rows = sqlx::query!(
            // sqlx cannot prove NOT NULL through a derived table, so it infers
            // every column of the union as nullable. The `!` overrides restore
            // the four columns the table itself declares NOT NULL; the rest are
            // genuinely nullable and need no annotation.
            "SELECT id AS \"id!\", occurred_at AS \"occurred_at!\",
                    actor_account_id AS \"actor_account_id!\", action AS \"action!\",
                    target_type, target_id, metadata,
                    context_session_id, context_definition_id
             FROM (
                 (SELECT a.id, a.occurred_at, a.actor_account_id, a.action, a.target_type,
                         a.target_id, a.metadata, a.context_session_id, a.context_definition_id
                    FROM audit_log a
                   WHERE ($1::timestamptz IS NULL OR (a.occurred_at, a.id) < ($1, $2))
                     AND ($4::uuid IS NULL OR a.actor_account_id = $4)
                     AND ($5::text IS NULL OR a.action = $5)
                     AND ($6::uuid IS NULL OR a.target_id = $6)
                   ORDER BY a.occurred_at DESC, a.id DESC
                   LIMIT $3)
                 UNION ALL
                 (SELECT a.id, a.occurred_at, a.actor_account_id, a.action, a.target_type,
                         a.target_id, a.metadata, a.context_session_id, a.context_definition_id
                    FROM audit_log a
                   WHERE $6::uuid IS NOT NULL
                     AND (a.context_session_id = $6 OR a.context_definition_id = $6)
                     AND a.target_id IS DISTINCT FROM $6
                     AND ($1::timestamptz IS NULL OR (a.occurred_at, a.id) < ($1, $2))
                     AND ($4::uuid IS NULL OR a.actor_account_id = $4)
                     AND ($5::text IS NULL OR a.action = $5)
                   ORDER BY a.occurred_at DESC, a.id DESC
                   LIMIT $3)
             ) u
             ORDER BY u.occurred_at DESC, u.id DESC
             LIMIT $3",
            before_at,
            before_id,
            over_fetch,
            actor,
            action,
            object,
        )
        .fetch_all(&self.pool)
        .await?;

        let rows: Vec<StoredAuditEntry> = rows
            .into_iter()
            .map(|r| StoredAuditEntry {
                id: r.id,
                occurred_at_millis: millis_from_utc(r.occurred_at),
                actor_account_id: r.actor_account_id,
                action: r.action,
                target_type: r.target_type,
                target_id: r.target_id,
                metadata: r.metadata,
                context_session_id: r.context_session_id,
                context_definition_id: r.context_definition_id,
            })
            .collect();
        Ok(into_page(rows, limit, |e| (e.occurred_at_millis, e.id)))
    }

    /// One page of the records appended at or after `since_millis`, newest
    /// first (windowed review read).
    ///
    /// Windowed but still paged: a time window is not a bound. "Everything
    /// since last Tuesday" is unbounded in exactly the way "everything" is, and
    /// on an audit log a silently-truncated answer reads as "this is all that
    /// happened" — the way of being wrong most likely to be believed. Returning
    /// a [`Page`] puts the truncation in the type, where the caller cannot miss
    /// that more may remain.
    ///
    /// Keyset, not offset: the log only ever grows at the newest end, so an
    /// append between two page fetches cannot shift an older row across a
    /// boundary. The `id` tiebreak is load-bearing — `occurred_at` alone is not
    /// unique, and two rows sharing a millisecond at a page edge is precisely
    /// where a timestamp-only cursor drops one or serves it twice.
    ///
    /// The window floor is inclusive, matching "at or after".
    pub async fn since_page(
        &self,
        limit: usize,
        cursor: Option<PageCursor>,
        since_millis: u64,
    ) -> Result<Page<StoredAuditEntry>, sqlx::Error> {
        let before_at = cursor.map(|c| utc_from_millis(c.at_millis));
        let before_id = cursor.map(|c| c.id);
        let rows = sqlx::query!(
            "SELECT id, occurred_at, actor_account_id, action, target_type, target_id, metadata,
                    context_session_id, context_definition_id
             FROM audit_log
             WHERE occurred_at >= $4
               AND ($1::timestamptz IS NULL OR (occurred_at, id) < ($1, $2))
             ORDER BY occurred_at DESC, id DESC
             LIMIT $3",
            before_at,
            before_id,
            // Over-fetch by one to learn whether a further page exists.
            (limit as i64) + 1,
            utc_from_millis(since_millis),
        )
        .fetch_all(&self.pool)
        .await?;

        let rows: Vec<StoredAuditEntry> = rows
            .into_iter()
            .map(|r| StoredAuditEntry {
                id: r.id,
                occurred_at_millis: millis_from_utc(r.occurred_at),
                actor_account_id: r.actor_account_id,
                action: r.action,
                target_type: r.target_type,
                target_id: r.target_id,
                metadata: r.metadata,
                context_session_id: r.context_session_id,
                context_definition_id: r.context_definition_id,
            })
            .collect();
        Ok(into_page(rows, limit, |e| (e.occurred_at_millis, e.id)))
    }
}
