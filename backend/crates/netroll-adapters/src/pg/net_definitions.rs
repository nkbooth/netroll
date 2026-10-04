// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
//! Net definitions and their ownership links. `RETURNING` plus
//! `fetch_optional`, so a zero-row UPDATE is `RowNotFound` and never a silent
//! success. Enum columns read back through `TryFrom`, mapping an unexpected
//! value to a decode error reachable only if a non-Rust writer bypassed the
//! domain.

use chrono::{DateTime, Utc};
use netroll_domain::admin::PageCursor;
use netroll_domain::authz::would_orphan;
use netroll_domain::caps::within_cap;
use netroll_domain::net::NetDefinition;
use netroll_domain::net::connection::NetConnectionSet;
use netroll_domain::net::enums::{NetCategory, NetType, Visibility};
use netroll_domain::net::validation::NetDefinitionFields;
use sqlx::PgPool;
use uuid::Uuid;

use super::net_connections::{load_for_definition, load_for_definitions, replace_connections};
use super::{Page, into_page, millis_from_utc, utc_from_millis};

/// Postgres repository for `net_definitions` + `net_definition_owners`.
#[derive(Clone)]
pub struct NetDefinitionRepo {
    pool: PgPool,
}

/// Outcome of [`NetDefinitionRepo::remove_owner`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RemoveOwnerOutcome {
    /// `account_id` was removed from the owner set.
    Removed,
    /// Removing `account_id` would have left the net with zero owners — the
    /// delete never ran.
    WouldOrphan,
    /// `account_id` was not a member of the owner set — nothing to remove.
    NotAMember,
}

/// Outcome of [`NetDefinitionRepo::add_owner`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AddOwnerOutcome {
    /// `account_id` is now an owner — either freshly inserted, or already an
    /// owner (an idempotent re-add adds no row and is NEVER cap-refused).
    Added,
    /// The net already sits at the configured owner cap and `account_id` is not
    /// already an owner — no row was inserted.
    AtCap,
}

/// The `net_definitions` row shape, shared by create/find/update so the
/// enum-decoding mapping lives in one place.
struct DefinitionRow {
    id: Uuid,
    definition_version: i32,
    title: String,
    description: Option<String>,
    country: Option<String>,
    state: Option<String>,
    grid: Option<String>,
    net_category: String,
    net_type: String,
    expected_duration_minutes: Option<i32>,
    visibility: String,
    link_token: String,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
    archived_at: Option<DateTime<Utc>>,
}

/// Maps an out-of-vocabulary enum value read from storage to a decode error.
fn decode_error(column: &str) -> sqlx::Error {
    sqlx::Error::Decode(format!("net_definitions.{column} holds an unknown enum value").into())
}

/// The refusal a definition holding NO `net_connections` rows reads as, on a
/// read that NAMES it.
///
/// Every definition is born with at least one connection — `create` takes the
/// set as an argument and `NetConnectionSet::new` refuses an empty one — and
/// cannot be edited down to none, so a definition with zero rows is damage (a
/// hand-run DELETE, a partial restore), not a net with no way on. This case once
/// reconstructed a set from the flat mirror columns, on the reasoning that they
/// were the last thing the single writer stamped and so evidence rather than
/// invention. The columns are gone, and with them the
/// evidence. What is left is a choice between a silent EMPTY set — a net served
/// with a 200 that claims to be reachable by nothing — and a loud refusal, and
/// the refusal is the honest one for a read whose subject IS the damaged net.
///
/// A read that LISTS nets takes the other posture — see
/// [`skip_missing_connections`]. Failing a
/// whole list on one damaged row emptied an owner's page, and through favorites
/// emptied a STRANGER'S page and their personal-data export, over data they
/// cannot touch.
fn missing_connections(id: Uuid) -> sqlx::Error {
    sqlx::Error::Decode(format!("net_definitions.{id} has no net_connections rows").into())
}

/// Logs and drops a definition with NO `net_connections` rows from a LIST read.
///
/// The counterpart of [`missing_connections`]: a list answers for many nets, so
/// one damaged row is skipped and reported at `error` — the operator's signal
/// that a repair is owed — rather than rendered as reachable by nothing (a lie)
/// or allowed to take every other row down with it (a blast radius that crosses
/// account boundaries through favorites). The damaged net's OWN reads still
/// refuse, so the damage is never silent. `pub(crate)` because the favorites and
/// discovery readers take the same posture and must log the same line.
pub(crate) fn skip_missing_connections(id: Uuid) {
    tracing::error!(
        net_definition_id = %id,
        "net_definitions row has no net_connections rows; skipped from a list read — repair it"
    );
}

impl DefinitionRow {
    /// Combines the row with its resolved owner set into the domain read
    /// model, decoding enum columns through the domain `TryFrom`.
    fn into_definition(
        self,
        owner_account_ids: Vec<Uuid>,
        connections: NetConnectionSet,
    ) -> Result<NetDefinition, sqlx::Error> {
        Ok(NetDefinition {
            id: self.id,
            definition_version: self.definition_version,
            title: self.title,
            description: self.description,
            country: self.country,
            state: self.state,
            grid: self.grid,
            connections,
            net_category: NetCategory::try_from(self.net_category.as_str())
                .map_err(|()| decode_error("net_category"))?,
            net_type: NetType::try_from(self.net_type.as_str())
                .map_err(|()| decode_error("net_type"))?,
            expected_duration_minutes: self.expected_duration_minutes,
            visibility: Visibility::try_from(self.visibility.as_str())
                .map_err(|()| decode_error("visibility"))?,
            link_token: self.link_token,
            owner_account_ids,
            created_at_millis: millis_from_utc(self.created_at),
            updated_at_millis: millis_from_utc(self.updated_at),
            archived_at_millis: self.archived_at.map(millis_from_utc),
        })
    }
}

impl NetDefinitionRepo {
    /// Wraps the shared connection pool.
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    /// Creates a net definition (`definition_version = 1`, UUIDv7 id) with the
    /// connection set it is born with, AND records `owner_account_id` as its
    /// sole owner — one transaction (the `create_verified_and_attach` shape).
    /// The returned model carries `owner_account_ids == [owner_account_id]`.
    ///
    /// `connections` is an argument rather than something derived here: a
    /// definition has no connection fact of its own to derive one from, and the
    /// caller has already parsed the list through the same
    /// `parse_connection_set` the `/connections` sub-resource uses.
    pub async fn create(
        &self,
        fields: &NetDefinitionFields,
        connections: &NetConnectionSet,
        owner_account_id: Uuid,
        link_token: &str,
        now_millis: u64,
    ) -> Result<NetDefinition, sqlx::Error> {
        let now = utc_from_millis(now_millis);
        let id = Uuid::now_v7();
        let mut tx = self.pool.begin().await?;

        let row = sqlx::query_as!(
            DefinitionRow,
            "INSERT INTO net_definitions
                (id, title, description, country, state, grid,
                 net_category, net_type, expected_duration_minutes, visibility, link_token,
                 created_at, updated_at)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $12)
             RETURNING id, definition_version, title, description, country, state, grid,
                       net_category, net_type, expected_duration_minutes, visibility,
                       link_token, created_at, updated_at, archived_at",
            id,
            fields.title,
            fields.description.as_deref(),
            fields.country.as_deref(),
            fields.state.as_deref(),
            fields.grid.as_ref().map(|g| g.as_str()),
            fields.net_category.as_str(),
            fields.net_type.as_str(),
            fields.expected_duration_minutes,
            fields.visibility.as_str(),
            link_token,
            now,
        )
        .fetch_one(&mut *tx)
        .await?;

        sqlx::query!(
            "INSERT INTO net_definition_owners (net_definition_id, account_id, created_at)
             VALUES ($1, $2, $3)",
            id,
            owner_account_id,
            now,
        )
        .execute(&mut *tx)
        .await?;

        // The ONE connection writer, in this same transaction: a definition row
        // never exists without its connection rows.
        replace_connections(&mut tx, id, connections, now).await?;
        tx.commit().await?;
        row.into_definition(vec![owner_account_id], connections.clone())
    }

    /// Looks up a definition by id, populating its owner set (a second query
    /// in the same method). A missing id → `None`.
    pub async fn find_by_id(&self, id: Uuid) -> Result<Option<NetDefinition>, sqlx::Error> {
        let row = sqlx::query_as!(
            DefinitionRow,
            "SELECT id, definition_version, title, description, country, state, grid,
                    net_category, net_type, expected_duration_minutes, visibility, link_token,
                    created_at, updated_at, archived_at
             FROM net_definitions WHERE id = $1",
            id,
        )
        .fetch_optional(&self.pool)
        .await?;

        match row {
            None => Ok(None),
            Some(row) => {
                let owners = self.owner_account_ids(id).await?;
                let connections = load_for_definition(&self.pool, id)
                    .await?
                    .ok_or_else(|| missing_connections(id))?;
                Ok(Some(row.into_definition(owners, connections)?))
            }
        }
    }

    /// Looks up a definition by its unguessable `link_token`, populating its
    /// owner set — the resolver behind the PUBLIC read-by-token endpoint
    /// Works for a net of any visibility (the token is a
    /// permalink). A nonexistent/garbage token → `None`. Inherits the
    /// accepted two-query non-transactional read posture.
    pub async fn find_by_link_token(
        &self,
        token: &str,
    ) -> Result<Option<NetDefinition>, sqlx::Error> {
        let row = sqlx::query_as!(
            DefinitionRow,
            "SELECT id, definition_version, title, description, country, state, grid,
                    net_category, net_type, expected_duration_minutes, visibility, link_token,
                    created_at, updated_at, archived_at
             FROM net_definitions WHERE link_token = $1",
            token,
        )
        .fetch_optional(&self.pool)
        .await?;

        match row {
            None => Ok(None),
            Some(row) => {
                let owners = self.owner_account_ids(row.id).await?;
                let connections = load_for_definition(&self.pool, row.id)
                    .await?
                    .ok_or_else(|| missing_connections(row.id))?;
                Ok(Some(row.into_definition(owners, connections)?))
            }
        }
    }

    /// The discovery-scoped read: `Listed` nets only, newest first (proves an
    /// `Unlisted` net can never leak into discovery). This is the minimal
    /// seam the discovery read consumes; the rich ordering/filtering/pagination
    /// is deliberately NOT here.
    pub async fn list_discoverable(&self) -> Result<Vec<NetDefinition>, sqlx::Error> {
        let rows = sqlx::query_as!(
            DefinitionRow,
            "SELECT id, definition_version, title, description, country, state, grid,
                    net_category, net_type, expected_duration_minutes, visibility, link_token,
                    created_at, updated_at, archived_at
             FROM net_definitions
             WHERE visibility = 'listed' AND archived_at IS NULL
             ORDER BY created_at DESC",
        )
        .fetch_all(&self.pool)
        .await?;

        self.resolve_definitions(rows).await
    }

    /// One page of the ACTIVE (non-archived) nets `account_id` owns, newest
    /// first, keyset over `(created_at DESC, id DESC)` — the "My Nets" Owned
    /// tab (replaces the unbounded `list_owned`). Same owner-set +
    /// `archived_at IS NULL` scope as
    /// [`Self::owned_active_definitions`]/[`Self::count_active_owned_nets`],
    /// but returning the full [`NetDefinition`] the Owned-tab cards render
    /// (connections, visibility, etc.), not just `(id, title)`.
    ///
    /// Ordered by creation, not title: the house cursor carries a
    /// `(timestamp, id)` pair and nothing else, and a title cursor would be a
    /// third paging idiom. The owner set is unbounded through co-ownership (the
    /// target's cap is never checked on `add_owner`), which is why this read
    /// needs a bound at all.
    pub async fn list_owned_page(
        &self,
        account_id: Uuid,
        limit: usize,
        cursor: Option<PageCursor>,
    ) -> Result<Page<NetDefinition>, sqlx::Error> {
        let before_at = cursor.map(|c| utc_from_millis(c.at_millis));
        let before_id = cursor.map(|c| c.id);
        let raw = sqlx::query_as!(
            DefinitionRow,
            "SELECT d.id, d.definition_version, d.title, d.description, d.country, d.state,
                    d.grid, d.net_category, d.net_type, d.expected_duration_minutes,
                    d.visibility, d.link_token, d.created_at, d.updated_at, d.archived_at
             FROM net_definitions d
             JOIN net_definition_owners o ON o.net_definition_id = d.id
             WHERE o.account_id = $1 AND d.archived_at IS NULL
               AND ($2::timestamptz IS NULL OR (d.created_at, d.id) < ($2, $3))
             ORDER BY d.created_at DESC, d.id DESC
             LIMIT $4",
            account_id,
            before_at,
            before_id,
            // Over-fetch by one to learn whether a further page exists.
            (limit as i64) + 1,
        )
        .fetch_all(&self.pool)
        .await?;

        // Split on the RAW rows BEFORE `resolve_definitions` skips a damaged
        // one, so `next` is the SERVED page's last raw row (`into_page`
        // truncates to `limit` before reading `rows.last()`; the (limit+1)th
        // probe row is never the cursor — issued from it, the walk would skip
        // one row per page) and the walk stays total. Split AFTER the skip and
        // `into_page` computes `has_more` over the survivors: a damaged row
        // inside an exactly-full page lets the probe row be promoted onto the
        // page, `has_more` reads false, and the row past it is LOST — silently,
        // under a null cursor (the owned-nets fence measures
        // exactly that). The chosen order's cost is visible instead: a page may
        // come back short by its damaged rows — even empty — with a non-null
        // cursor, and "Load more" fetches the next page. A row is never lost,
        // never repeated, and the cursor always advances.
        let page = into_page(raw, limit, |r| (millis_from_utc(r.created_at), r.id));
        let rows = self.resolve_definitions(page.rows).await?;
        Ok(Page {
            rows,
            next: page.next,
        })
    }

    /// Resolves a page of rows into domain read models, loading EVERY row's
    /// connections in one query keyed on the returned ids.
    ///
    /// One query, not one per row: a per-row connection read would turn the
    /// "my nets" page and the discovery source into an N+1. (The pre-existing
    /// per-row owner read below is already one; adding a second would double
    /// the round trips on the same pages.)
    ///
    /// A row with no connection set is SKIPPED, not fatal — the list posture
    /// [`skip_missing_connections`] documents.
    async fn resolve_definitions(
        &self,
        rows: Vec<DefinitionRow>,
    ) -> Result<Vec<NetDefinition>, sqlx::Error> {
        let ids: Vec<Uuid> = rows.iter().map(|r| r.id).collect();
        let sets = load_for_definitions(&self.pool, &ids).await?;

        let mut definitions = Vec::with_capacity(rows.len());
        for row in rows {
            let Some((_, connections)) = sets.iter().find(|(id, _)| *id == row.id) else {
                skip_missing_connections(row.id);
                continue;
            };
            let owners = self.owner_account_ids(row.id).await?;
            definitions.push(row.into_definition(owners, connections.clone())?);
        }
        Ok(definitions)
    }

    /// The owner set for `id` — the pre-mutation authz check reads this. A
    /// missing or owner-less net yields an empty vec.
    ///
    /// Ordered by `(created_at, account_id)`: the `account_id` tiebreaker is
    /// required because multiple owners can share an
    /// instant, so `ownerAccountIds` in API bodies is deterministic.
    pub async fn owner_account_ids(&self, id: Uuid) -> Result<Vec<Uuid>, sqlx::Error> {
        let rows = sqlx::query!(
            "SELECT account_id FROM net_definition_owners WHERE net_definition_id = $1
             ORDER BY created_at, account_id",
            id,
        )
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.into_iter().map(|r| r.account_id).collect())
    }

    /// Applies `fields` to the definition's SCALAR columns and increments
    /// `definition_version` server-side — one statement (last-write-wins, no
    /// CAS). Owner rows are untouched (owner management is separate), and so
    /// is the connection set: a scalar edit cannot name a connection, so it
    /// cannot add, retune or remove one. A zero-row UPDATE
    /// (missing id) → `RowNotFound`, never silent success.
    pub async fn update(
        &self,
        id: Uuid,
        fields: &NetDefinitionFields,
        now_millis: u64,
    ) -> Result<NetDefinition, sqlx::Error> {
        let now = utc_from_millis(now_millis);
        let row = sqlx::query_as!(
            DefinitionRow,
            "UPDATE net_definitions SET
                title = $2, description = $3, country = $4, state = $5, grid = $6,
                net_category = $7, net_type = $8, expected_duration_minutes = $9,
                visibility = $10,
                definition_version = definition_version + 1, updated_at = $11
             WHERE id = $1
             RETURNING id, definition_version, title, description, country, state, grid,
                       net_category, net_type, expected_duration_minutes, visibility,
                       link_token, created_at, updated_at, archived_at",
            id,
            fields.title,
            fields.description.as_deref(),
            fields.country.as_deref(),
            fields.state.as_deref(),
            fields.grid.as_ref().map(|g| g.as_str()),
            fields.net_category.as_str(),
            fields.net_type.as_str(),
            fields.expected_duration_minutes,
            fields.visibility.as_str(),
            now,
        )
        .fetch_optional(&self.pool)
        .await?
        .ok_or(sqlx::Error::RowNotFound)?;

        let owners = self.owner_account_ids(id).await?;
        let connections = load_for_definition(&self.pool, id)
            .await?
            .ok_or_else(|| missing_connections(id))?;
        row.into_definition(owners, connections)
    }

    /// Replaces the connection list under an optimistic-concurrency check, and
    /// bumps `definition_version`.
    ///
    /// `Ok(None)` means `expected_version` was no longer current — the caller
    /// has already established that the definition exists and is theirs, so a
    /// zero-row CAS is a lost race, not a missing net.
    ///
    /// The version bump is a deliberate contrast with the `/schedule` and
    /// `/delivery-config` sub-resources, which do not bump: connections WERE
    /// fields on the definition, and a session's provenance stamp is
    /// `(id, definition_version)`. A connection edit that left the version
    /// alone would let two sessions started either side of it claim the same
    /// stamp, and the stamp would be a lie.
    pub async fn update_connections(
        &self,
        id: Uuid,
        expected_version: i32,
        connections: &NetConnectionSet,
        now_millis: u64,
    ) -> Result<Option<NetDefinition>, sqlx::Error> {
        let now = utc_from_millis(now_millis);
        let mut tx = self.pool.begin().await?;
        let bumped = sqlx::query!(
            "UPDATE net_definitions
                SET definition_version = definition_version + 1, updated_at = $3
              WHERE id = $1 AND definition_version = $2
              RETURNING id",
            id,
            expected_version,
            now,
        )
        .fetch_optional(&mut *tx)
        .await?;
        if bumped.is_none() {
            tx.rollback().await?;
            return Ok(None);
        }
        replace_connections(&mut tx, id, connections, now).await?;
        tx.commit().await?;

        // Re-read through the ordinary read so the caller gets the same shape
        // every other read serves. A missing row here would mean the definition
        // was deleted between the committed CAS and this read — never a lost
        // race, so it must not masquerade as one.
        self.find_by_id(id)
            .await?
            .ok_or(sqlx::Error::RowNotFound)
            .map(Some)
    }

    /// Counts the ACTIVE (non-archived) nets `account_id` owns.
    ///
    /// Counts nets where the account is in the owner set AND `archived_at IS
    /// NULL`: archived nets have left discovery and are retired,
    /// so they must not permanently consume a user's slots — archiving a net
    /// frees one. Co-ownership counts: an account in a net's owner set counts
    /// that net toward its own cap.
    pub async fn count_active_owned_nets(&self, account_id: Uuid) -> Result<i64, sqlx::Error> {
        let row = sqlx::query!(
            "SELECT COUNT(*) AS count
             FROM net_definitions d
             JOIN net_definition_owners o ON o.net_definition_id = d.id
             WHERE o.account_id = $1 AND d.archived_at IS NULL",
            account_id,
        )
        .fetch_one(&self.pool)
        .await?;
        Ok(row.count.unwrap_or(0))
    }

    /// Lists the ACTIVE (non-archived) nets `account_id` owns as `(id, title)`
    /// pairs — the "nets I own" section of the personal-data
    /// export. Same owner-set + `archived_at IS NULL` scope as
    /// [`Self::count_active_owned_nets`], selecting the display title instead of
    /// a count. Ordered by title then id for a deterministic total order.
    pub async fn owned_active_definitions(
        &self,
        account_id: Uuid,
    ) -> Result<Vec<(Uuid, String)>, sqlx::Error> {
        let rows = sqlx::query!(
            "SELECT d.id, d.title
             FROM net_definitions d
             JOIN net_definition_owners o ON o.net_definition_id = d.id
             WHERE o.account_id = $1 AND d.archived_at IS NULL
             ORDER BY d.title, d.id",
            account_id,
        )
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.into_iter().map(|r| (r.id, r.title)).collect())
    }

    /// Adds `account_id` as a co-owner of `net_definition_id`,
    /// enforcing the per-net owner cap.
    ///
    /// Idempotent: a re-add of an EXISTING owner adds no row and returns
    /// [`AddOwnerOutcome::Added`] — never cap-refused (it costs no slot). A new
    /// owner is refused with [`AddOwnerOutcome::AtCap`] when the net already sits
    /// at `max_owners`.
    ///
    /// The count-and-decide runs INSIDE a transaction that first `SELECT ... FOR
    /// UPDATE`-locks the net's OWN row (always exactly one, regardless of the
    /// current owner count), THEN re-reads the owner set. Locking the net row
    /// — not just the owner rows — is the fix for a genuine race a
    /// concurrency test caught: `SELECT ... FOR UPDATE` only locks rows already
    /// in its result set, so it does NOT block a concurrent transaction's
    /// `INSERT` of a brand-new owner row (a phantom-read hazard under READ
    /// COMMITTED). Two concurrent adds of DIFFERENT accounts against a
    /// single-owner net previously both locked that SAME one pre-existing row,
    /// serialized on it, but each still decided against its OWN stale
    /// `owners.len() == 1` snapshot — both admitted past a cap of 2. Locking the
    /// always-present net row instead forces genuine serialization: the second
    /// transaction blocks until the first commits, and its owner-count query —
    /// a fresh statement, so a fresh READ COMMITTED snapshot — then correctly
    /// observes the first transaction's insert.
    pub async fn add_owner(
        &self,
        net_definition_id: Uuid,
        account_id: Uuid,
        now_millis: u64,
        max_owners: usize,
    ) -> Result<AddOwnerOutcome, sqlx::Error> {
        let now = utc_from_millis(now_millis);
        let mut tx = self.pool.begin().await?;
        // Locks the net's own row so a concurrent add_owner for the SAME net
        // blocks here until this transaction commits or rolls back, regardless
        // of how many owner rows currently exist (see the doc comment above).
        sqlx::query!(
            "SELECT id FROM net_definitions WHERE id = $1 FOR UPDATE",
            net_definition_id,
        )
        .fetch_optional(&mut *tx)
        .await?;

        // A fresh statement past the lock above — under READ COMMITTED this
        // takes a new snapshot, so a racer that just unblocked sees the
        // winner's insert rather than the pre-race owner set.
        let owners: Vec<Uuid> = sqlx::query!(
            "SELECT account_id FROM net_definition_owners
             WHERE net_definition_id = $1
             ORDER BY created_at, account_id
             FOR UPDATE",
            net_definition_id,
        )
        .fetch_all(&mut *tx)
        .await?
        .into_iter()
        .map(|r| r.account_id)
        .collect();

        // A re-add of an existing owner costs no slot — idempotent no-op, never
        // cap-refused.
        if owners.contains(&account_id) {
            tx.rollback().await?;
            return Ok(AddOwnerOutcome::Added);
        }
        if !within_cap(owners.len(), max_owners) {
            tx.rollback().await?;
            return Ok(AddOwnerOutcome::AtCap);
        }

        sqlx::query!(
            "INSERT INTO net_definition_owners (net_definition_id, account_id, created_at)
             VALUES ($1, $2, $3)
             ON CONFLICT (net_definition_id, account_id) DO NOTHING",
            net_definition_id,
            account_id,
            now,
        )
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(AddOwnerOutcome::Added)
    }

    /// Removes `account_id` from the owner set, refusing when doing so would
    /// leave the net with zero owners (the last-owner
    /// protection).
    ///
    /// The guard check and the delete run INSIDE one transaction that
    /// `SELECT ... FOR UPDATE`-locks the net's current owner rows before
    /// deciding. This closes a TOCTOU race a caller-side-only guard (read the
    /// owner set once in the HTTP handler, decide, then call an unconditional
    /// delete) is vulnerable to: two concurrent removals of DIFFERENT owners
    /// of the same 2-owner net could otherwise each observe the same
    /// pre-delete 2-owner snapshot, each independently pass the guard, and
    /// both deletes commit — leaving zero owners. Here, the
    /// second concurrent call blocks on the row lock until the first commits,
    /// then re-observes the POST-delete owner set before deciding.
    pub async fn remove_owner(
        &self,
        net_definition_id: Uuid,
        account_id: Uuid,
    ) -> Result<RemoveOwnerOutcome, sqlx::Error> {
        let mut tx = self.pool.begin().await?;
        // Row-locks every current owner of this net; a concurrent call
        // targeting the same net_definition_id blocks here until this
        // transaction commits or rolls back.
        let owners: Vec<Uuid> = sqlx::query!(
            "SELECT account_id FROM net_definition_owners
             WHERE net_definition_id = $1
             ORDER BY created_at, account_id
             FOR UPDATE",
            net_definition_id,
        )
        .fetch_all(&mut *tx)
        .await?
        .into_iter()
        .map(|r| r.account_id)
        .collect();

        if !owners.contains(&account_id) {
            tx.rollback().await?;
            return Ok(RemoveOwnerOutcome::NotAMember);
        }
        if would_orphan(&owners, account_id) {
            tx.rollback().await?;
            return Ok(RemoveOwnerOutcome::WouldOrphan);
        }

        sqlx::query!(
            "DELETE FROM net_definition_owners
             WHERE net_definition_id = $1 AND account_id = $2",
            net_definition_id,
            account_id,
        )
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(RemoveOwnerOutcome::Removed)
    }

    /// The owner set for the owner-facing HTTP body, each owner paired with its
    /// (nullable) callsign for display. Same `(created_at, account_id)` order
    /// as [`NetDefinitionRepo::owner_account_ids`].
    pub async fn owners_with_callsign(
        &self,
        net_definition_id: Uuid,
    ) -> Result<Vec<(Uuid, Option<String>)>, sqlx::Error> {
        let rows = sqlx::query!(
            "SELECT o.account_id, a.callsign
             FROM net_definition_owners o
             JOIN accounts a ON a.id = o.account_id
             WHERE o.net_definition_id = $1
             ORDER BY o.created_at, o.account_id",
            net_definition_id,
        )
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .into_iter()
            .map(|r| (r.account_id, r.callsign))
            .collect())
    }

    /// Archives every non-archived net that currently has ZERO owner rows, in
    /// one idempotent UPDATE, returning how many were archived. Cause-agnostic:
    /// it enforces the invariant "a zero-owner net is
    /// archived" regardless of WHY the net reached zero owners. A net only ever
    /// reaches zero owners via the account-finalize cascade (`create`
    /// always inserts one owner atomically and `remove_owner` is gated by the
    /// last-owner guard), so this is safe to run on every finalizer tick
    /// unconditionally — it archives nothing when no net is orphaned and never
    /// re-touches an already-archived row (`archived_at IS NULL` guard).
    pub async fn archive_ownerless(&self, now_millis: u64) -> Result<u64, sqlx::Error> {
        let now = utc_from_millis(now_millis);
        let result = sqlx::query!(
            "UPDATE net_definitions SET archived_at = $1
             WHERE archived_at IS NULL
               AND NOT EXISTS (
                   SELECT 1 FROM net_definition_owners o
                   WHERE o.net_definition_id = net_definitions.id
               )",
            now,
        )
        .execute(&self.pool)
        .await?;
        Ok(result.rows_affected())
    }

    /// Archives the definition (an owner-initiated delete is an archive),
    /// setting `archived_at` on the same column the owner-removal path
    /// shipped (NOT a second archive mechanism). Returns whether a row was
    /// newly archived. Idempotent: a second archive is a no-op (the
    /// `archived_at IS NULL` guard), not an error — the `archive_ownerless`
    /// posture.
    ///
    /// This REPLACES the earlier hard delete: the definition row and its
    /// `net_occurrences`/owner rows survive for provenance and are never
    /// hard-deleted, so a definition with
    /// history can never be deleted out from under a started session.
    pub async fn archive(&self, id: Uuid, now_millis: u64) -> Result<bool, sqlx::Error> {
        let now = utc_from_millis(now_millis);
        let result = sqlx::query!(
            "UPDATE net_definitions SET archived_at = $2
             WHERE id = $1 AND archived_at IS NULL",
            id,
            now,
        )
        .execute(&self.pool)
        .await?;
        Ok(result.rows_affected() == 1)
    }
}
