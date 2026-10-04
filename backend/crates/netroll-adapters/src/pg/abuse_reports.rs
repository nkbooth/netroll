// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
//! Storage for public abuse reports.
//!
//! Free-text, recorded server-side for an administrator to action later. No
//! structured per-net or per-account target FK, and no reporter account FK —
//! the reporter may be unauthenticated.

use netroll_domain::admin::PageCursor;
use sqlx::PgPool;
use uuid::Uuid;

use super::{Page, into_page, millis_from_utc, utc_from_millis};

/// A recorded abuse report as read back for the admin review queue.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredAbuseReport {
    /// UUIDv7 primary key.
    pub id: Uuid,
    /// When the report was filed, epoch millis.
    pub created_at_millis: u64,
    /// Optional free-text contact the reporter chose to leave.
    pub reporter_contact: Option<String>,
    /// The free-text report body.
    pub body: String,
    /// Optional URL the reporter was viewing when they filed.
    pub context_url: Option<String>,
    /// When an admin resolved it, epoch millis; `None` while unresolved.
    pub resolved_at_millis: Option<u64>,
    /// The admin account id that resolved it; `None` while unresolved.
    pub resolved_by: Option<Uuid>,
}

/// Outcome of an [`AbuseReportRepo::resolve`] call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResolveOutcome {
    /// The report was unresolved and is now marked resolved by this call.
    Resolved,
    /// The report was already resolved; this call left it unmoved.
    AlreadyResolved,
}

/// Postgres repository for `abuse_reports`.
#[derive(Clone)]
pub struct AbuseReportRepo {
    pool: PgPool,
}

impl AbuseReportRepo {
    /// Wraps the shared connection pool.
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    /// Records a submitted report, returning its generated id. Times enter as
    /// the caller's injected `now` (never the DB clock). The body is bounded by
    /// the handler before it reaches here.
    pub async fn record(
        &self,
        body: &str,
        reporter_contact: Option<&str>,
        context_url: Option<&str>,
        now_millis: u64,
    ) -> Result<Uuid, sqlx::Error> {
        let id = Uuid::now_v7();
        sqlx::query!(
            "INSERT INTO abuse_reports (id, created_at, reporter_contact, body, context_url)
             VALUES ($1, $2, $3, $4, $5)",
            id,
            utc_from_millis(now_millis),
            reporter_contact,
            body,
            context_url,
        )
        .execute(&self.pool)
        .await?;
        Ok(id)
    }

    /// One page of the unresolved queue, oldest first — the only queue read
    /// this repo offers (`ViewReports`), because the admin dashboard must not
    /// serialize the whole backlog per view.
    ///
    /// Keyset, not offset: `cursor` is the previous page's last row, so a report
    /// resolved between two page fetches cannot shift rows across the boundary
    /// and hide one. The `id` tiebreak is load-bearing — `created_at` alone is
    /// not unique.
    pub async fn list_unresolved_page(
        &self,
        limit: usize,
        cursor: Option<PageCursor>,
    ) -> Result<Page<StoredAbuseReport>, sqlx::Error> {
        let after_at = cursor.map(|c| utc_from_millis(c.at_millis));
        let after_id = cursor.map(|c| c.id);
        let rows = sqlx::query!(
            "SELECT id, created_at, reporter_contact, body, context_url, resolved_at, resolved_by
             FROM abuse_reports
             WHERE resolved_at IS NULL
               AND ($1::timestamptz IS NULL OR (created_at, id) > ($1, $2))
             ORDER BY created_at ASC, id ASC
             LIMIT $3",
            after_at,
            after_id,
            // Over-fetch by one to learn whether a further page exists.
            (limit as i64) + 1,
        )
        .fetch_all(&self.pool)
        .await?;

        let rows: Vec<StoredAbuseReport> = rows
            .into_iter()
            .map(|r| StoredAbuseReport {
                id: r.id,
                created_at_millis: millis_from_utc(r.created_at),
                reporter_contact: r.reporter_contact,
                body: r.body,
                context_url: r.context_url,
                resolved_at_millis: r.resolved_at.map(millis_from_utc),
                resolved_by: r.resolved_by,
            })
            .collect();
        Ok(into_page(rows, limit, |r| (r.created_at_millis, r.id)))
    }

    /// Marks report `id` resolved by admin `resolved_by` (`ResolveReport`).
    /// Idempotent: a re-resolve is [`ResolveOutcome::AlreadyResolved`] and does
    /// not move the original `resolved_at`. A missing report is
    /// [`sqlx::Error::RowNotFound`].
    pub async fn resolve(
        &self,
        id: Uuid,
        resolved_by: Uuid,
        now_millis: u64,
    ) -> Result<ResolveOutcome, sqlx::Error> {
        let updated = sqlx::query!(
            "UPDATE abuse_reports SET resolved_at = $2, resolved_by = $3
             WHERE id = $1 AND resolved_at IS NULL
             RETURNING id",
            id,
            utc_from_millis(now_millis),
            resolved_by,
        )
        .fetch_optional(&self.pool)
        .await?;

        match updated {
            Some(_) => Ok(ResolveOutcome::Resolved),
            None => {
                let exists = sqlx::query!("SELECT id FROM abuse_reports WHERE id = $1", id)
                    .fetch_optional(&self.pool)
                    .await?;
                if exists.is_none() {
                    Err(sqlx::Error::RowNotFound)
                } else {
                    Ok(ResolveOutcome::AlreadyResolved)
                }
            }
        }
    }
}
