// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
//! Bounded admin object search: a name or partial callsign to the id every
//! other admin surface needs. Matching is ASYMMETRIC ON PURPOSE — callsign,
//! name and title match by substring (public radio data); email and every id
//! match EXACTLY (a targeting tool, not a harvesting sweep). Capped per type;
//! touches no per-user secret — QRZ credentials stay envelope-encrypted here.

use sqlx::PgPool;
use uuid::Uuid;

use super::{like_contains, like_literal, millis_from_utc};

/// One matched object, flattened to what an admin needs to identify it and
/// pivot from it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchHit {
    /// The object's id — the value the audit filters and action endpoints take.
    pub id: Uuid,
    /// Primary human label (callsign, net title, …). May be absent when the
    /// object has none (an account with no callsign yet).
    pub label: Option<String>,
    /// Secondary detail (email, lifecycle, …).
    pub sublabel: Option<String>,
    /// When an admin disabled this account, epoch millis. Accounts only.
    pub disabled_at_millis: Option<u64>,
    /// When this object was archived/deleted, epoch millis.
    pub inactive_at_millis: Option<u64>,
}

/// Postgres reads behind the admin search.
#[derive(Clone)]
pub struct AdminSearchRepo {
    pool: PgPool,
}

impl AdminSearchRepo {
    /// Wraps the shared connection pool.
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    /// Accounts whose callsign or display name STARTS WITH `term`.
    ///
    /// Prefix, never substring, and never over email. Uses
    /// `lower(col) LIKE 'x%'` so the `text_pattern_ops` indexes apply — a plain
    /// `lower(col)` btree does not serve `LIKE` under a non-C collation, which
    /// would turn this bounded lookup into a full scan.
    ///
    /// NOT the trigram form the two title searches below use, and
    /// deliberately so: `idx_accounts_callsign_prefix` and
    /// `idx_accounts_display_name_prefix` are btrees over `lower(col)`, so this
    /// read must keep both the `lower()` and the `LIKE` — rewriting it to
    /// `ILIKE` or dropping the wrapper would orphan both indexes. A prefix is
    /// what a btree serves; a substring is what a trigram index serves.
    pub async fn accounts_by_prefix(
        &self,
        term: &str,
        limit: i64,
    ) -> Result<Vec<SearchHit>, sqlx::Error> {
        let pattern = format!("{}%", like_literal(&term.to_lowercase()));
        let rows = sqlx::query!(
            "SELECT id, email, callsign, display_name, disabled_at, deleted_at
               FROM accounts
              WHERE lower(callsign) LIKE $1 ESCAPE '\\'
                 OR lower(display_name) LIKE $1 ESCAPE '\\'
              ORDER BY callsign NULLS LAST, display_name NULLS LAST, id
              LIMIT $2",
            pattern,
            limit,
        )
        .fetch_all(&self.pool)
        .await?;

        Ok(rows
            .into_iter()
            .map(|r| SearchHit {
                id: r.id,
                label: r.callsign.or(r.display_name),
                sublabel: Some(r.email),
                disabled_at_millis: r.disabled_at.map(millis_from_utc),
                inactive_at_millis: r.deleted_at.map(millis_from_utc),
            })
            .collect())
    }

    /// The account with EXACTLY this (already-normalized) email.
    ///
    /// Exact, never prefix — that asymmetry against the callsign/name search is
    /// the anti-harvesting fence. Callsigns and display names are public radio
    /// data; an address is not, so it must be known in full to be found.
    pub async fn account_by_exact_email(
        &self,
        email: &str,
    ) -> Result<Option<SearchHit>, sqlx::Error> {
        let row = sqlx::query!(
            "SELECT id, email, callsign, display_name, disabled_at, deleted_at
               FROM accounts WHERE email = $1",
            email,
        )
        .fetch_optional(&self.pool)
        .await?;

        Ok(row.map(|r| SearchHit {
            id: r.id,
            label: r.callsign.or(r.display_name),
            sublabel: Some(r.email),
            disabled_at_millis: r.disabled_at.map(millis_from_utc),
            inactive_at_millis: r.deleted_at.map(millis_from_utc),
        }))
    }

    /// Net definitions whose title CONTAINS `term`.
    ///
    /// `title ILIKE $1 ESCAPE '\'` with the term wrapped and escaped by
    /// `like_contains`, so a `%` or `_` a user typed stays a literal — the
    /// property the earlier `strpos` form bought at the price of being
    /// unindexable at any row count. Served by `idx_net_definitions_title_trgm`
    /// The same index the discovery `q` filter uses. `ILIKE` on
    /// the BARE column: an index on `title` does not serve `lower(title) ILIKE`,
    /// and `pg_trgm`'s comparisons are case-insensitive on their own.
    pub async fn net_definitions_by_title(
        &self,
        term: &str,
        limit: i64,
    ) -> Result<Vec<SearchHit>, sqlx::Error> {
        let rows = sqlx::query!(
            "SELECT id, title, archived_at
               FROM net_definitions
              WHERE title ILIKE $1 ESCAPE '\\'
              ORDER BY archived_at NULLS FIRST, title, id
              LIMIT $2",
            like_contains(term),
            limit,
        )
        .fetch_all(&self.pool)
        .await?;

        Ok(rows
            .into_iter()
            .map(|r| SearchHit {
                id: r.id,
                label: Some(r.title),
                sublabel: None,
                disabled_at_millis: None,
                inactive_at_millis: r.archived_at.map(millis_from_utc),
            })
            .collect())
    }

    /// Net sessions whose FROZEN snapshot title contains `term`.
    ///
    /// Deliberately the snapshot, not a join to the net's current title: a
    /// session must be findable and labeled by the name it actually ran under.
    /// Joining `net_definitions` would make a renamed net's old sessions
    /// unfindable by their real name and label them with one they never had —
    /// a correctness bug in an investigation tool. The expression is served by
    /// `idx_net_sessions_snapshot_title_trgm`, so searching the
    /// snapshot no longer costs a scan either; same `ILIKE`-plus-escaping form
    /// as `net_definitions_by_title`, same bare-expression rule.
    pub async fn net_sessions_by_snapshot_title(
        &self,
        term: &str,
        limit: i64,
    ) -> Result<Vec<SearchHit>, sqlx::Error> {
        let rows = sqlx::query!(
            "SELECT id, definition_snapshot->>'title' AS title, lifecycle, closed_at
               FROM net_sessions
              WHERE (definition_snapshot->>'title') ILIKE $1 ESCAPE '\\'
              ORDER BY started_at DESC NULLS LAST, id
              LIMIT $2",
            like_contains(term),
            limit,
        )
        .fetch_all(&self.pool)
        .await?;

        Ok(rows
            .into_iter()
            .map(|r| SearchHit {
                id: r.id,
                label: r.title,
                sublabel: Some(r.lifecycle),
                disabled_at_millis: None,
                inactive_at_millis: r.closed_at.map(millis_from_utc),
            })
            .collect())
    }

    /// Resolves a bare id against every searchable type, so a uuid copied out of
    /// the audit log resolves back to a named object.
    ///
    /// Reports the type it matched; `None` when the id names nothing. Abuse
    /// reports are reachable ONLY this way — a report body is free text a
    /// reporter wrote, and is never search-matched.
    pub async fn by_id(&self, id: Uuid) -> Result<Option<(&'static str, SearchHit)>, sqlx::Error> {
        if let Some(r) = sqlx::query!(
            "SELECT id, email, callsign, display_name, disabled_at, deleted_at
               FROM accounts WHERE id = $1",
            id
        )
        .fetch_optional(&self.pool)
        .await?
        {
            return Ok(Some((
                "account",
                SearchHit {
                    id: r.id,
                    label: r.callsign.or(r.display_name),
                    sublabel: Some(r.email),
                    disabled_at_millis: r.disabled_at.map(millis_from_utc),
                    inactive_at_millis: r.deleted_at.map(millis_from_utc),
                },
            )));
        }
        if let Some(r) = sqlx::query!(
            "SELECT id, title, archived_at FROM net_definitions WHERE id = $1",
            id
        )
        .fetch_optional(&self.pool)
        .await?
        {
            return Ok(Some((
                "net-definition",
                SearchHit {
                    id: r.id,
                    label: Some(r.title),
                    sublabel: None,
                    disabled_at_millis: None,
                    inactive_at_millis: r.archived_at.map(millis_from_utc),
                },
            )));
        }
        if let Some(r) = sqlx::query!(
            "SELECT id, definition_snapshot->>'title' AS title, lifecycle, closed_at
               FROM net_sessions WHERE id = $1",
            id
        )
        .fetch_optional(&self.pool)
        .await?
        {
            return Ok(Some((
                "net-session",
                SearchHit {
                    id: r.id,
                    label: r.title,
                    sublabel: Some(r.lifecycle),
                    disabled_at_millis: None,
                    inactive_at_millis: r.closed_at.map(millis_from_utc),
                },
            )));
        }
        if let Some(r) = sqlx::query!(
            "SELECT id, created_at, resolved_at FROM abuse_reports WHERE id = $1",
            id
        )
        .fetch_optional(&self.pool)
        .await?
        {
            return Ok(Some((
                "abuse-report",
                SearchHit {
                    id: r.id,
                    // The body is a reporter's free text — never a label here.
                    label: None,
                    sublabel: None,
                    disabled_at_millis: None,
                    inactive_at_millis: r.resolved_at.map(millis_from_utc),
                },
            )));
        }
        Ok(None)
    }
}
