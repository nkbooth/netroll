// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
//! Per-account favorites, the "My Nets" bookmark list. SECURITY:
//! `list_for_account` lists ONLY the account's own rows and NEVER selects an
//! owner id — the read model has no owner column, so the redaction cannot be
//! forgotten. It DOES select `link_token`: this is the PRIVATE account-scoped
//! projection, and the token is the favoriter's way back.

use chrono::{DateTime, Utc};
use netroll_domain::admin::PageCursor;
use netroll_domain::net::connection::NetConnectionSet;
use netroll_domain::net::enums::{NetCategory, NetType};
use sqlx::PgPool;
use uuid::Uuid;

use super::net_connections::load_for_definitions;
use super::{Page, into_page, millis_from_utc, utc_from_millis};

/// Postgres repository for `net_favorites`.
#[derive(Clone)]
pub struct FavoritesRepo {
    pool: PgPool,
}

/// One favorited net in the "My Nets" read model: the definition's display
/// fields, its connection set, and the `favorited_at` instant. By construction it carries NO owner
/// id or `visibility` column (structural redaction), but it DOES carry
/// `link_token` (the return-link the favoriter needs) and `archived_at_millis`
/// (the archived indicator).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FavoriteRow {
    /// The favorited definition's id.
    pub id: Uuid,
    /// Net title.
    pub title: String,
    /// Free-text description, or `None`.
    pub description: Option<String>,
    /// Every way to reach this net, in the owner's order. The
    /// favorites card and the personal-data export used to carry the
    /// definition's flat `band`/`mode`, which went stale the moment a net
    /// dropped its last RF way; they now carry the same connection set every
    /// other net projection does.
    pub connections: NetConnectionSet,
    /// Country, or `None`.
    pub country: Option<String>,
    /// State/province, or `None`.
    pub state: Option<String>,
    /// Canonical Maidenhead grid, or `None`.
    pub grid: Option<String>,
    /// Net category.
    pub net_category: NetCategory,
    /// Net type.
    pub net_type: NetType,
    /// Expected duration in minutes, or `None`.
    pub expected_duration_minutes: Option<i32>,
    /// The permalink token — returned to the favoriter to build the
    /// `/nets/t/{token}` return link (safe: account-scoped).
    pub link_token: String,
    /// Archival instant in epoch millis, or `None` for an active net — the
    /// archived indicator.
    pub archived_at_millis: Option<u64>,
    /// When the account favorited this net, epoch millis.
    pub favorited_at_millis: u64,
}

/// The raw row shape decoded from the JOIN (enum columns as text, instants as
/// `timestamptz`); mapped to [`FavoriteRow`] decoding enums through the domain
/// `TryFrom`.
struct FavoriteDbRow {
    id: Uuid,
    title: String,
    description: Option<String>,
    country: Option<String>,
    state: Option<String>,
    grid: Option<String>,
    net_category: String,
    net_type: String,
    expected_duration_minutes: Option<i32>,
    link_token: String,
    archived_at: Option<DateTime<Utc>>,
    favorited_at: DateTime<Utc>,
}

/// Maps an out-of-vocabulary enum value read from storage to a decode error
/// (only reachable if a non-Rust writer bypassed the domain).
fn decode_error(column: &str) -> sqlx::Error {
    sqlx::Error::Decode(format!("net_definitions.{column} holds an unknown enum value").into())
}

impl FavoriteDbRow {
    fn into_row(self, connections: NetConnectionSet) -> Result<FavoriteRow, sqlx::Error> {
        Ok(FavoriteRow {
            id: self.id,
            title: self.title,
            description: self.description,
            connections,
            country: self.country,
            state: self.state,
            grid: self.grid,
            net_category: NetCategory::try_from(self.net_category.as_str())
                .map_err(|()| decode_error("net_category"))?,
            net_type: NetType::try_from(self.net_type.as_str())
                .map_err(|()| decode_error("net_type"))?,
            expected_duration_minutes: self.expected_duration_minutes,
            link_token: self.link_token,
            archived_at_millis: self.archived_at.map(millis_from_utc),
            favorited_at_millis: millis_from_utc(self.favorited_at),
        })
    }
}

impl FavoritesRepo {
    /// Wraps the shared connection pool.
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    /// Favorites `net_definition_id` for `account_id`, stamping
    /// `created_at` from `now_millis` on the FIRST favorite only. Idempotent: a
    /// re-favorite is a no-op via the composite PK (`ON CONFLICT DO NOTHING`) —
    /// it keeps its original `created_at`, so it does not move to the top of
    /// the newest-first list — never a second row or an error (the `add_owner`
    /// precedent).
    ///
    /// `created_at` is clock-written like every other ordering timestamp in
    /// this adapter: the paged read's cursor names a
    /// millisecond, and a `DEFAULT now()` microsecond would fall between two of
    /// them — a genuinely older row sharing the last served row's millisecond
    /// would be skipped by the keyset predicate.
    pub async fn add(
        &self,
        account_id: Uuid,
        net_definition_id: Uuid,
        now_millis: u64,
    ) -> Result<(), sqlx::Error> {
        sqlx::query!(
            "INSERT INTO net_favorites (account_id, net_definition_id, created_at)
             VALUES ($1, $2, $3)
             ON CONFLICT (account_id, net_definition_id) DO NOTHING",
            account_id,
            net_definition_id,
            utc_from_millis(now_millis),
        )
        .execute(&self.pool)
        .await
        .map(|_| ())
    }

    /// Unfavorites `net_definition_id` for `account_id`. Idempotent:
    /// removing a favorite that is not present is a success no-op
    /// (`rows_affected() == 0` is still `Ok`), never an error.
    pub async fn remove(
        &self,
        account_id: Uuid,
        net_definition_id: Uuid,
    ) -> Result<(), sqlx::Error> {
        sqlx::query!(
            "DELETE FROM net_favorites WHERE account_id = $1 AND net_definition_id = $2",
            account_id,
            net_definition_id,
        )
        .execute(&self.pool)
        .await
        .map(|_| ())
    }

    /// The "My Nets" read: every net `account_id` has favorited —
    /// Listed AND Unlisted, active AND archived — newest-favorited first. Joins
    /// the definition's display fields and the permalink token; NEVER selects
    /// an owner id (structural redaction). Ordered by `(favorited_at DESC,
    /// net_definition_id DESC)` — the SAME total order as
    /// [`Self::list_for_account_page`], because `add` writes `created_at` at
    /// millisecond precision, so two favorites sharing an instant is routine,
    /// and the export and the My Nets tab are two views of one collection that
    /// must not disagree about which of the pair came first.
    ///
    /// UNBOUNDED on purpose, and the personal-data export's whole read — an
    /// export is complete by definition. The HTTP list pages
    /// through [`Self::list_for_account_page`] instead.
    pub async fn list_for_account(
        &self,
        account_id: Uuid,
    ) -> Result<Vec<FavoriteRow>, sqlx::Error> {
        let rows = sqlx::query_as!(
            FavoriteDbRow,
            // Both columns run DESC to match the paged sibling's row-value
            // cursor order; see `list_for_account_page`.
            r#"SELECT
                    d.id,
                    d.title,
                    d.description,
                    d.country,
                    d.state,
                    d.grid,
                    d.net_category,
                    d.net_type,
                    d.expected_duration_minutes,
                    d.link_token,
                    d.archived_at,
                    f.created_at AS favorited_at
               FROM net_favorites f
               JOIN net_definitions d ON d.id = f.net_definition_id
               WHERE f.account_id = $1
               ORDER BY f.created_at DESC, f.net_definition_id DESC"#,
            account_id,
        )
        .fetch_all(&self.pool)
        .await?;

        // One batched query for every favorite's connections, keyed on the ids
        // just returned — the same shape `load_for_definitions` was built for,
        // never one query per row on a page that lists a whole account's nets.
        let ids: Vec<Uuid> = rows.iter().map(|row| row.id).collect();
        let sets = load_for_definitions(&self.pool, &ids).await?;
        let mut favorites = Vec::with_capacity(rows.len());
        for row in rows {
            // A favorited net with no connection rows is damage, not a net
            // with no way on: every definition is born with at least one
            // and cannot be edited down to none. It is SKIPPED
            // and logged rather than rendered as reachable by nothing — and
            // rather than failing the list: the favoriter cannot repair a net
            // they do not own, and this same read feeds the personal-data
            // export (`skip_missing_connections` says why).
            let Some((_, connections)) = sets.iter().find(|(id, _)| *id == row.id) else {
                super::net_definitions::skip_missing_connections(row.id);
                continue;
            };
            favorites.push(row.into_row(connections.clone())?);
        }
        Ok(favorites)
    }

    /// One page of the "My Nets" read, newest-favorited first, keyset over
    /// `(created_at DESC, net_definition_id DESC)` — the bounded sibling of
    /// [`Self::list_for_account`] that `GET /api/favorites` serves.
    /// Same scope, same structural redaction, same skip posture for a damaged
    /// row.
    pub async fn list_for_account_page(
        &self,
        account_id: Uuid,
        limit: usize,
        cursor: Option<PageCursor>,
    ) -> Result<Page<FavoriteRow>, sqlx::Error> {
        let before_at = cursor.map(|c| utc_from_millis(c.at_millis));
        let before_id = cursor.map(|c| c.id);
        let raw = sqlx::query_as!(
            FavoriteDbRow,
            // Both columns run DESC: a row-value comparison `(a, b) < (x, y)` is
            // only a total-order cursor when both halves share a direction.
            r#"SELECT
                    d.id,
                    d.title,
                    d.description,
                    d.country,
                    d.state,
                    d.grid,
                    d.net_category,
                    d.net_type,
                    d.expected_duration_minutes,
                    d.link_token,
                    d.archived_at,
                    f.created_at AS favorited_at
               FROM net_favorites f
               JOIN net_definitions d ON d.id = f.net_definition_id
               WHERE f.account_id = $1
                 AND ($2::timestamptz IS NULL OR (f.created_at, f.net_definition_id) < ($2, $3))
               ORDER BY f.created_at DESC, f.net_definition_id DESC
               LIMIT $4"#,
            account_id,
            before_at,
            before_id,
            // Over-fetch by one to learn whether a further page exists.
            (limit as i64) + 1,
        )
        .fetch_all(&self.pool)
        .await?;

        // The page is split on the RAW rows, BEFORE the damaged-row skip below,
        // so `next` is the SERVED page's last raw row (`into_page` truncates to
        // `limit` before reading `rows.last()`; the (limit+1)th probe row is
        // never the cursor — issued from it, the walk would skip one row per
        // page) and the walk stays total. Split AFTER the skip and `into_page`
        // computes `has_more` over the survivors: a damaged row inside an
        // exactly-full page lets the probe row be promoted onto the page,
        // `has_more` reads false, and the row past it is LOST — silently, under
        // a null cursor, which was measured. The
        // chosen order's cost is visible instead: a page may come back short by
        // its damaged rows — even empty — with a non-null cursor, and "Load
        // more" fetches the next page. A row is never lost, never repeated, and
        // the cursor always advances.
        let page = into_page(raw, limit, |r| (millis_from_utc(r.favorited_at), r.id));

        let ids: Vec<Uuid> = page.rows.iter().map(|row| row.id).collect();
        let sets = load_for_definitions(&self.pool, &ids).await?;
        let mut rows = Vec::with_capacity(page.rows.len());
        for row in page.rows {
            let Some((_, connections)) = sets.iter().find(|(id, _)| *id == row.id) else {
                super::net_definitions::skip_missing_connections(row.id);
                continue;
            };
            rows.push(row.into_row(connections.clone())?);
        }
        Ok(Page {
            rows,
            next: page.next,
        })
    }

    /// The subset of `net_definition_ids` that `account_id` has favorited, in
    /// id order — the batch membership question behind the star on a discovery
    /// card or public net page, asked once for a whole page of nets instead of
    /// walking every favorite the account has.
    /// An id the account has not favorited, or that names no net, is simply
    /// absent. Scoped to `account_id` in SQL: another account's favorite of the
    /// same net is never reported.
    pub async fn membership(
        &self,
        account_id: Uuid,
        net_definition_ids: &[Uuid],
    ) -> Result<Vec<Uuid>, sqlx::Error> {
        let rows = sqlx::query!(
            "SELECT net_definition_id
               FROM net_favorites
              WHERE account_id = $1 AND net_definition_id = ANY($2)
              ORDER BY net_definition_id",
            account_id,
            net_definition_ids,
        )
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.into_iter().map(|r| r.net_definition_id).collect())
    }
}
