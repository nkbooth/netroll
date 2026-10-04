// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
//! The joined, ordered upcoming-occurrence query the landing page consumes.
//! SECURITY: the SELECT never takes an owner id, so no column here can leak an
//! owner identity. It DOES take `link_token`, safe only because every row is
//! `listed` — load-bearing on the `d.visibility = 'listed'` clause in BOTH
//! statements, and true only when served, since a token survives an unlist.

use chrono::{DateTime, Utc};
use netroll_domain::fold::{SessionState, replay};
use netroll_domain::net::connection::NetConnectionSet;
use netroll_domain::net::discovery::DiscoveryQuery;
use netroll_domain::net::enums::{NetCategory, NetType};
use netroll_domain::net::wire::{NetConnectionWire, connection_set_from_wire};
use sqlx::PgPool;
use uuid::Uuid;

use super::session_events::SessionEventLog;
use super::{like_contains, millis_from_utc, utc_from_millis};

/// Reads a stored session's `connections` array back into the domain set as it
/// stands NOW — the frozen list with the session's folded frequency moves laid
/// over it — or `None` when this version cannot represent what is stored.
///
/// `None` is NOT an error here, and that is the whole point: on the public
/// discovery listing a session nobody can replay costs its own CARD, never the
/// page. The named 410 belongs on that session's own surfaces, where the
/// operator asked for that log; a visitor who asked for "what is on right now"
/// gets the answer for every net that can still give one.
///
/// The overlay happens on the WIRE list, before `connection_set_from_wire`
/// builds the set, because that is the shape [`SessionState::live_connections`]
/// is defined on — the shape every session surface holds — and a moved
/// frequency lives on the wire's `planned_frequency_hz`. Overlaying after the
/// set is built would be a second implementation of the same move over the
/// domain's typed connection kinds, which is the drift this fix removes.
fn readable_connection_set(
    connections: &serde_json::Value,
    folded: &SessionState,
) -> Option<NetConnectionSet> {
    let snapshot: Vec<NetConnectionWire> = serde_json::from_value(connections.clone()).ok()?;
    connection_set_from_wire(&folded.live_connections(&snapshot)).ok()
}

/// Postgres repository for the public discovery read model.
#[derive(Clone)]
pub struct DiscoveryRepo {
    pool: PgPool,
}

/// One upcoming, discoverable occurrence joined with its definition's display
/// fields. Carries the occurrence pointer (`occurrence_id`,
/// `scheduled_start_at_millis`) plus the SAME descriptive field set the public
/// `PublicNetBody` exposes, plus the definition's `link_token` —
/// and, by construction, NO owner id or `visibility` (every row here is
/// `listed`). The HTTP layer maps this directly to the redacted
/// `DiscoveryNetBody`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscoveryUpcomingRow {
    /// The occurrence's UUIDv7 id when sourced from
    /// [`DiscoveryRepo::list_discovery_upcoming`] — or the LIVE SESSION's id
    /// when sourced from [`DiscoveryRepo::list_active_now`] (a live session
    /// need not have a scheduled occurrence at all). The wire field name
    /// (`occurrenceId`) stays the same either way; the frontend hero's
    /// "Watch live"/"Check in" links just need SOMETHING that resolves via
    /// `/live/:id`, and for an `activeNow` entry that's the session id.
    pub occurrence_id: Uuid,
    /// Absolute planned start (upcoming) or actual start (active now), epoch
    /// millis.
    pub scheduled_start_at_millis: u64,
    /// The definition's id.
    pub definition_id: Uuid,
    /// The definition's monotonic version at read time.
    pub definition_version: i32,
    /// Net title.
    pub title: String,
    /// Free-text description, or `None`.
    pub description: Option<String>,
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
    /// The definition's permalink token — the client builds
    /// `/nets/t/{linkToken}` from it. Read from `net_definitions` on BOTH reads,
    /// never from a session snapshot: a token is a property of the definition
    /// and has no frozen-at-start meaning. Serving it here is safe only because
    /// every row on this read model is `listed` — see the module doc.
    pub link_token: String,
    /// Every way this net can be reached, in the owner's order.
    ///
    /// `None` only between `into_row` and the reader that fills it: the
    /// UPCOMING read attaches the definition's set and DROPS a row that has
    /// none (a definition with zero `net_connections` rows is damage, skipped
    /// and logged — the list posture `net_definitions::skip_missing_connections`
    /// documents); the ACTIVE-NOW read fills it
    /// from the session's snapshot and drops a card it cannot read. Neither
    /// reader returns a row with `None`, so a card never renders a net as
    /// reachable by nothing.
    pub connections: Option<NetConnectionSet>,
    /// The connection that satisfied the band, mode or connection-kind filter,
    /// when one was applied — the
    /// lowest-`position` match, and `None` when none of the three was applied.
    ///
    /// Returned by the query rather than re-derived by the caller: re-deriving
    /// it means hand-writing a twin of the filter predicate, and a twin that
    /// drifts highlights a connection the server did not match on — a confident
    /// wrong answer to the one question the field exists to answer.
    pub matched_connection_id: Option<Uuid>,
}

/// Rows to serve, plus whether the query matched more than the bound admits
///
/// A two-field struct rather than a bare tuple at the call site, and NOT a
/// `Page<T>`: there is no cursor on either discovery read, and returning the
/// paged shape would advertise one.
#[derive(Debug)]
pub struct BoundedRows {
    /// The rows to serve, at most the bound's worth — and possibly FEWER, even
    /// none, when a row inside the served window was dropped in Rust after the
    /// query (see `truncated`).
    pub rows: Vec<DiscoveryUpcomingRow>,
    /// True when the SQL returned more raw rows than the bound. Decided on the
    /// RAW rows, BEFORE any Rust-side skip, so a dropped card can never read
    /// as an exhausted list: split AFTER the skip and one damaged row inside an
    /// exactly-full window lets the probe row be promoted onto the page, the
    /// length reads as the bound, and the answer says COMPLETE while more
    /// matched — the silence this flag exists to remove, re-created by it
    /// (measured, and inherited here).
    pub truncated: bool,
}

/// The raw row shape decoded from the JOIN (enum columns as text, instant as
/// `timestamptz`); mapped to [`DiscoveryUpcomingRow`] decoding enums through
/// the domain `TryFrom`.
struct DiscoveryRow {
    occurrence_id: Uuid,
    scheduled_start_at: DateTime<Utc>,
    definition_id: Uuid,
    definition_version: i32,
    title: String,
    description: Option<String>,
    country: Option<String>,
    state: Option<String>,
    grid: Option<String>,
    net_category: String,
    net_type: String,
    expected_duration_minutes: Option<i32>,
    link_token: String,
    matched_connection_id: Option<Uuid>,
    /// The live session's own frozen connection list, read on the SAME row as
    /// the rest of the card. `NULL` on the upcoming read, which has no session.
    snapshot_connections: Option<serde_json::Value>,
}

/// Maps an out-of-vocabulary enum value read from storage to a decode error
/// (only reachable if a non-Rust writer bypassed the domain).
fn decode_error(column: &str) -> sqlx::Error {
    sqlx::Error::Decode(format!("net_definitions.{column} holds an unknown enum value").into())
}

impl DiscoveryRow {
    fn into_row(self) -> Result<DiscoveryUpcomingRow, sqlx::Error> {
        Ok(DiscoveryUpcomingRow {
            occurrence_id: self.occurrence_id,
            scheduled_start_at_millis: millis_from_utc(self.scheduled_start_at),
            definition_id: self.definition_id,
            definition_version: self.definition_version,
            title: self.title,
            description: self.description,
            country: self.country,
            state: self.state,
            grid: self.grid,
            net_category: NetCategory::try_from(self.net_category.as_str())
                .map_err(|()| decode_error("net_category"))?,
            net_type: NetType::try_from(self.net_type.as_str())
                .map_err(|()| decode_error("net_type"))?,
            expected_duration_minutes: self.expected_duration_minutes,
            link_token: self.link_token,
            // Left empty here, and filled differently by the two readers:
            // the UPCOMING read calls `attach_connections`, one batched query
            // over the definitions just returned (a per-row load would turn a
            // public, unauthenticated endpoint into an N+1);
            // the ACTIVE-NOW read fills it from `snapshot_connections`, which
            // rode in on this same row, and never consults the definition.
            connections: None,
            matched_connection_id: self.matched_connection_id,
        })
    }
}

/// The bound as a slice length.
///
/// Panics rather than saturating on a `limit` that is not a valid length: the
/// two callers pass module constants, and the fallback that would make this
/// "safe" — `unwrap_or(usize::MAX)` — inverts the bound instead of reporting
/// it, leaving `truncated` false and `truncate` a no-op. That is the silent
/// false-COMPLETE `BoundedRows` exists to remove, reintroduced in the error
/// path. A caller that cannot guarantee a
/// non-negative limit must clamp before calling, not after.
fn bound_of(limit: i64) -> usize {
    usize::try_from(limit).expect("discovery bound must be a non-negative limit")
}

impl DiscoveryRepo {
    /// Wraps the shared connection pool.
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    /// Fills every row's connection set in ONE query keyed on the definition
    /// ids just returned.
    ///
    /// `load_for_definitions` exists for exactly this — its own doc names "the
    /// discovery source" as a caller it was built for. One query per collection,
    /// never one per row: this endpoint is public and unauthenticated, so a
    /// per-row load multiplies by the page size for every visitor. (It IS
    /// governed per IP in production — `api_router_ip_limited` layers the
    /// read governor over `public_read_routes()`, which merges the
    /// discovery routes. This comment used to say the endpoint carried no rate
    /// limiter; corrected once it was found that the new
    /// `ACTIVE_NOW_LIMIT` docstring depending on the opposite. A limiter bounds
    /// cost per CALLER and never per REQUEST, so the argument for batching here
    /// is unchanged by the correction.)
    ///
    /// A definition with no connection rows is DROPPED from the answer, not
    /// served with an empty list: the same posture the active-now read takes
    /// for a snapshot it cannot read, and the same one the owned-nets and
    /// favorites lists take for this exact row.
    async fn attach_connections(
        &self,
        rows: Vec<DiscoveryUpcomingRow>,
    ) -> Result<Vec<DiscoveryUpcomingRow>, sqlx::Error> {
        let ids: Vec<Uuid> = rows.iter().map(|row| row.definition_id).collect();
        let sets = super::net_connections::load_for_definitions(&self.pool, &ids).await?;
        let mut attached = Vec::with_capacity(rows.len());
        for mut row in rows {
            let Some((_, set)) = sets.iter().find(|(id, _)| *id == row.definition_id) else {
                super::net_definitions::skip_missing_connections(row.definition_id);
                continue;
            };
            row.connections = Some(set.clone());
            attached.push(row);
        }
        Ok(attached)
    }

    /// The upcoming discovery read: every FUTURE occurrence
    /// (`scheduled_start_at > now`) of a LISTED, non-archived definition,
    /// narrowed by `query.filters` (each `AND`ed), ordered by `query.sort`
    /// (default soonest-first), capped at `limit` — and SAYING SO when the cap
    /// bit: `truncated` is decided on the raw rows the query
    /// returned, before [`Self::attach_connections`] drops a definition with no
    /// connection rows, so the served list may come back short of `limit` while
    /// the flag still says more matched. Returns the redacted
    /// [`DiscoveryUpcomingRow`] — never owner ids; the `link_token` it carries
    /// is a Listed net's.
    ///
    /// The ordering is already total on every sort arm (`… o.scheduled_start_at
    /// ASC, o.id ASC`), so the bound needs no tiebreak added here.
    pub async fn list_discovery_upcoming(
        &self,
        query: &DiscoveryQuery,
        now_millis: u64,
        limit: i64,
    ) -> Result<BoundedRows, sqlx::Error> {
        let now = utc_from_millis(now_millis);
        let f = &query.filters;
        // Enum filters bind their kebab token (the exact column value); the
        // domain already validated them in-vocabulary, so `col = $n` is a
        // straight equality. Every filter uses the `($n IS NULL OR ...)` idiom
        // so a single prepared statement covers all filter combinations —
        // NEVER dynamic SQL string-building (injection-safe, query!-checked).
        // The `name` filter is a case-insensitive substring test via
        // `d.title ILIKE $2 ESCAPE '\'`, with `$2` bound to the `%`-wrapped
        // term whose own `%`, `_` and `\` are escaped (`like_contains`), so a
        // wildcard character in the user's text stays a literal. `ILIKE` on the
        // BARE column, never `lower(d.title)`: the trigram GIN index
        // `idx_net_definitions_title_trgm` serves `title ILIKE`
        // and does NOT serve `lower(title) ILIKE`, a different expression —
        // wrapping it back keeps every test green over a full scan. pg_trgm's
        // comparisons are case-insensitive on their own. The pattern is built
        // HERE; `applied.q` on the wire still echoes the user's raw term.
        // Geography (country/state/grid) is owner-typed free text — `grid` is
        // canonicalized at write time (Grid field uppercase / subsquare
        // lowercase) but `country`/`state` are not — so an exact case-
        // sensitive `=` would silently return an empty set for any case
        // mismatch a casual visitor types. `lower(col) = lower($n)` matches
        // the `name` filter's case-insensitive posture.
        //
        // Band and mode are MATCH-ANY over `net_connections`: a net
        // now has a SET of ways to get on it, and a 2m search must find the net
        // whose 2m repeater is one of five. They live in ONE `EXISTS`, not two —
        // two independent subqueries would return a net with a 2m FM way and a
        // 20m SSB way for `?band=2m&mode=ssb`, which describes no way to reach
        // it at all, and would leave the matched connection with no answer.
        // Nothing COALESCEs back to `d.band`/`d.mode`: those go stale the moment
        // a net drops its last RF way, and consulting them "so nothing
        // regresses" reproduces the exact defect this predicate ended.
        //
        // Connection KIND joins that same `EXISTS`: a net whose
        // only way in is an EchoLink node has no band and no mode, so under
        // match-any it was findable by neither, and the kind is the one fact
        // every connection has. It is a third conjunct inside the ONE `EXISTS`,
        // never a subquery of its own — `?kind=echolink&band=20m` must describe
        // a single connection that is both, or the net is not a way onto 20m
        // by EchoLink — and it appears in BOTH guarded places below, the
        // matched-connection subquery and the `EXISTS`, with both `IS NULL`
        // guards widened to three-way. Each half alone answers `200`:
        // subquery-only narrows nothing, `EXISTS`-only marks nothing.
        //
        // `20260830120000_net_connections_network.sql` says "discovery's
        // predicates are band/mode only". That is SUPERSEDED here and cannot be
        // corrected there: it is an applied migration, and sqlx refuses to boot
        // on a checksum mismatch (`migrations/README.md`). This comment is the
        // live authority.
        //
        // NO INDEX is added for this, and the decision is recorded rather than
        // left implicit: the correlation is on `c.definition_id`, the LEADING
        // column of `UNIQUE (definition_id, position)`, so the subquery is
        // index-supported; `band`/`mode`/`kind` are then filtered across at
        // most `MAX_CONNECTIONS` (32) rows. Neither `net_definitions.band` nor
        // `.mode` was ever indexed either, so this is not a regression.
        //
        // `kind` is bound LAST, as `$12`, rather than renumbered in beside band
        // and mode: every filter bind here is `text`, so sqlx's compile-time
        // check would accept two of them swapped during a renumber and return
        // wrong data with a `200`. Appending cannot go wrong that way.
        let name_pattern = f.name.as_deref().map(like_contains);
        let band = f.band.map(|b| b.as_str());
        let mode = f.mode.map(|m| m.as_str());
        let kind = f.kind.map(|k| k.as_str());
        let category = f.net_category.map(|c| c.as_str());
        let net_type = f.net_type.map(|t| t.as_str());
        let sort = query.sort.as_query_token();

        let rows = sqlx::query_as!(
            DiscoveryRow,
            r#"SELECT
                    o.id AS occurrence_id,
                    o.scheduled_start_at,
                    d.id AS definition_id,
                    d.definition_version,
                    d.title,
                    d.description,
                    d.country,
                    d.state,
                    d.grid,
                    d.net_category,
                    d.net_type,
                    d.expected_duration_minutes,
                    d.link_token,
                    (SELECT c.id
                       FROM net_connections c
                      WHERE c.definition_id = d.id
                        AND NOT ($3::text IS NULL AND $4::text IS NULL AND $12::text IS NULL)
                        AND ($3::text IS NULL OR c.band = $3)
                        AND ($4::text IS NULL OR c.mode = $4)
                        AND ($12::text IS NULL OR c.kind = $12)
                      ORDER BY c.position
                      LIMIT 1) AS "matched_connection_id?",
                    NULL::jsonb AS "snapshot_connections?"
               FROM net_occurrences o
               JOIN net_definitions d ON d.id = o.definition_id
               WHERE d.visibility = 'listed'
                 AND d.archived_at IS NULL
                 AND o.scheduled_start_at > $1
                 AND ($2::text IS NULL OR d.title ILIKE $2 ESCAPE '\')
                 AND (($3::text IS NULL AND $4::text IS NULL AND $12::text IS NULL)
                      OR EXISTS (
                           SELECT 1
                             FROM net_connections c
                            WHERE c.definition_id = d.id
                              AND ($3::text IS NULL OR c.band = $3)
                              AND ($4::text IS NULL OR c.mode = $4)
                              AND ($12::text IS NULL OR c.kind = $12)))
                 AND ($5::text IS NULL OR lower(d.country) = lower($5))
                 AND ($6::text IS NULL OR lower(d.state) = lower($6))
                 AND ($7::text IS NULL OR lower(d.grid) = lower($7))
                 AND ($8::text IS NULL OR d.net_category = $8)
                 AND ($9::text IS NULL OR d.net_type = $9)
               ORDER BY
                    (CASE WHEN $10 = 'time' THEN o.scheduled_start_at END) ASC,
                    (CASE
                        WHEN $10 = 'name' THEN d.title
                        WHEN $10 = 'category' THEN d.net_category
                        WHEN $10 = 'type' THEN d.net_type
                    END) ASC,
                    o.scheduled_start_at ASC,
                    o.id ASC
               LIMIT $11"#,
            now,
            name_pattern.as_deref(),
            band,
            mode,
            f.country.as_deref(),
            f.state.as_deref(),
            f.grid.as_deref(),
            category,
            net_type,
            sort,
            // One more than is served, so a full page can be told from an
            // exhausted one without a second query. The SQL text is unchanged
            // by this — only the value bound to `$11` — so the offline query
            // cache and the trigram-index plan test are untouched.
            limit + 1,
            kind,
        )
        .fetch_all(&self.pool)
        .await?;

        // Split BEFORE `attach_connections`, which drops rows (see `BoundedRows`).
        // `>`, never `==`: exactly the bound is complete.
        let bound = bound_of(limit);
        let truncated = rows.len() > bound;
        let mut rows = rows;
        rows.truncate(bound);
        let rows: Vec<DiscoveryUpcomingRow> = rows
            .into_iter()
            .map(DiscoveryRow::into_row)
            .collect::<Result<_, _>>()?;
        let rows = self.attach_connections(rows).await?;
        Ok(BoundedRows { rows, truncated })
    }

    /// The "Live now" read (design handoff `1a`): every currently-live session
    /// (`net_sessions.lifecycle = 'live'`) of a LISTED, non-archived
    /// definition, oldest-started-first. Reuses [`DiscoveryUpcomingRow`]'s
    /// shape rather than a new type — additive per this module's original
    /// design, which promised that live sessions would fill `activeNow` from
    /// real `net_sessions` with zero change to this shape, and this finally
    /// does. `occurrence_id` carries the LIVE SESSION's id (not a scheduled
    /// occurrence — a live session need not have one), and
    /// `scheduled_start_at` carries the session's actual start instant: the
    /// frontend hero's "Watch live"/"Check in" links build `/live/:id` from
    /// this field regardless of whether the entry came from here or from
    /// [`Self::list_discovery_upcoming`], so it must resolve to something
    /// `GET /api/live-sessions/{id}` (the public session read) accepts —
    /// that's the session id, not an occurrence id.
    ///
    /// **Its connections are the SESSION's, taken by value from the session's
    /// own frozen `definition_snapshot`.** A session copies its connections
    /// precisely because they may legitimately
    /// differ from the definition's on the night: an owner editing a net's
    /// connection list mid-run must not silently repaint the live card for a
    /// session that is still on the old ways. The card now says what the net
    /// is actually running on, which is the question a visitor clicking "watch
    /// live" is asking. A live card is the ONE discovery row with a session to
    /// read; the upcoming rows have none and still read the definition.
    ///
    /// **Frozen is not the same as stale.** The snapshot is the list of ways in
    /// as they were at start; a `frequency.changed` mid-run moves one of them,
    /// and the move lives in the session's event log, never in the snapshot.
    /// Every other surface that serves a session's connections — the
    /// summary, the public session view, the close webhook, the exports —
    /// replays that log and overlays it via [`SessionState::live_connections`],
    /// and until this read did the same a net that QSY'd was on one frequency
    /// on its own page and another on the card a visitor clicks to get there.
    /// The overlay is applied HERE, per card, from the same fold those surfaces
    /// use, so there is one answer to "where is this net right now".
    ///
    /// That is one events query per live card, not one batched read, and the
    /// choice is deliberate: the batched alternative is a query over
    /// `frequency.changed` rows alone with last-write-wins done in SQL — a
    /// second, private implementation of the fold whose drift would be
    /// invisible until a card disagreed with its session. The fan-out is
    /// bounded by the count of nets live at once on a LISTED definition, not
    /// by a page size or a visitor's query, which is a different order of
    /// exposure from the `upcoming` path that `attach_connections` batches for.
    ///
    /// A snapshot this version cannot read DROPS ITS CARD rather than showing a
    /// live net with no way to reach it — `attach_connections` is deliberately
    /// NOT called on this path, so nothing falls back to the definition. A log
    /// this version cannot read drops the card the same way: `events_since`
    /// refuses a historical `frequency.changed` payload, and that
    /// is the same "nobody can replay this session" the snapshot case names,
    /// reached through the other half of the same session's record.
    ///
    /// **The card is dropped; the LISTING is not.** What is unreplayable is a
    /// SESSION, and after migration every session that already
    /// existed is an older one — so failing the whole function on one of them would
    /// take `activeNow`, `upcoming` and `applied` down together for every
    /// anonymous visitor, and the owner could not clear it by hand either,
    /// because `close` authorizes through the same refusing read. The operator
    /// still gets the named 410 on the session's OWN surfaces, which is where
    /// the refusal belongs.
    ///
    /// The snapshot rides the SAME row as the rest of the card, not a second
    /// id-keyed pass: two non-transactional reads let a session close between
    /// them, and "absent from the second result set" is then indistinguishable
    /// from "unreplayable" — a routine event reported as permanent loss. The
    /// events read that follows is keyed by the session id the row already
    /// carries and a log is append-only, so a session closing between the two
    /// reads still replays cleanly: the card shows the net that WAS live when
    /// the row was read, and nothing routine is mistaken for loss.
    ///
    /// `matched_connection_id` is always `NULL` here: `activeNow` takes no query
    /// and is never filtered, so no connection ever "matched".
    ///
    /// **Bounded at `limit`, in SQL, BEFORE the per-row replay.**
    /// The cost of this read is the replay loop below — one events query and one
    /// full fold per live card — so a bound that lands after the loop bounds
    /// only the row count and none of the work; the `LIMIT` is in the query and
    /// the loop iterates the already-cut vector. Over-fetched by one and split
    /// on the raw rows, as `BoundedRows` documents, so a card this loop drops
    /// can never make the listing read complete.
    ///
    /// The ORDER BY is `(started_at, id)`: `id` is a UUIDv7 primary key, so the
    /// pair is total and the same request twice cannot serve two different
    /// bound's-worths when sessions share a start instant. `started_at` is only
    /// ever compared against itself inside this one query — there is no cursor
    /// to re-anchor on — so its precision needs no clamping; the keyset
    /// reads' millisecond write does NOT transfer here.
    pub async fn list_active_now(&self, limit: i64) -> Result<BoundedRows, sqlx::Error> {
        let rows = sqlx::query_as!(
            DiscoveryRow,
            r#"SELECT
                    s.id AS occurrence_id,
                    s.started_at AS "scheduled_start_at!",
                    d.id AS definition_id,
                    d.definition_version,
                    d.title,
                    d.description,
                    d.country,
                    d.state,
                    d.grid,
                    d.net_category,
                    d.net_type,
                    d.expected_duration_minutes,
                    d.link_token,
                    NULL::uuid AS "matched_connection_id?",
                    s.definition_snapshot->'connections' AS "snapshot_connections?"
               FROM net_sessions s
               JOIN net_definitions d ON d.id = s.definition_id
               WHERE s.lifecycle = 'live'
                 AND d.visibility = 'listed'
                 AND d.archived_at IS NULL
               ORDER BY s.started_at ASC, s.id ASC
               LIMIT $1"#,
            limit + 1,
        )
        .fetch_all(&self.pool)
        .await?;

        // Split BEFORE the replay loop, which drops cards (see `BoundedRows`).
        // Cutting here is also what keeps the (limit + 1)th row's log out of
        // the loop — the bound applies to the WORK, not to the folded output.
        let bound = bound_of(limit);
        let truncated = rows.len() > bound;
        let mut rows = rows;
        rows.truncate(bound);

        let events = SessionEventLog::new(self.pool.clone());
        let mut out: Vec<DiscoveryUpcomingRow> = Vec::with_capacity(rows.len());
        for row in rows {
            let snapshot = row.snapshot_connections.clone();
            let mut row = row.into_row()?;
            // `occurrence_id` IS the session id on this read (see the field's
            // doc), which is what the log is keyed by.
            let Ok(log) = events.events_since(row.occurrence_id, 0).await else {
                // Same posture as the unreadable snapshot below: this session
                // cannot be replayed, so its card — and only its card — drops.
                continue;
            };
            let folded = replay(&log, 0);
            let Some(set) = snapshot.and_then(|value| readable_connection_set(&value, &folded))
            else {
                // The one card this visitor cannot be shown, skipped so the
                // other cards — and the two sections that never depended on
                // this session — still answer.
                continue;
            };
            row.connections = Some(set);
            out.push(row);
        }
        Ok(BoundedRows {
            rows: out,
            truncated,
        })
    }
}
