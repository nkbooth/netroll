// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
//! The public discovery landing endpoint: `GET /api/discovery`.
//!
//! Unauthenticated, with no cookie consulted. `applied` states the filters and
//! ordering actually used, which is the compensating control for the lenient key
//! policy; every row serializes through the redacted, owner-free body.

use axum::Json;
use axum::Router;
use axum::extract::State;
use axum::routing::get;
use netroll_adapters::pg::discovery::DiscoveryUpcomingRow;
use netroll_domain::net::discovery::{
    DiscoveryFilters, DiscoveryQuery, RawDiscoveryQuery, parse_discovery_query,
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use netroll_domain::net::wire::NetConnectionWire;

use super::net_definitions::connection_bodies;
use super::problem::ApiError;
use super::{AppQuery, AppState, rfc3339};

/// Result-set cap on one `upcoming` answer. A sane default ceiling — real
/// cursor/offset pagination of the upcoming list is deferred;
/// tune here or lift into `config.rs` if it needs to be instance-tunable.
///
/// `pub` so the integration test seeds against the constant rather than a copy
/// of its value. The cut is STATED on the wire (`applied.truncated`), never silent.
pub const DISCOVERY_LIMIT: i64 = 100;

/// Ceiling on one `activeNow` answer.
///
/// NOT a page size. This collection takes no query and has no cursor, so a card
/// past this bound is PERMANENTLY unreachable and no filter reveals it. The
/// ceiling exists to stop a public read being UNBOUNDED, not to tune cost — the
/// per-IP read governor does that — so it is set high enough that reaching it
/// means something is wrong on the instance. It shares a value with
/// `netroll_domain::admin::MAX_PAGE_LIMIT` by COINCIDENCE; do not wire them together.
pub const ACTIVE_NOW_LIMIT: i64 = 200;

/// The PUBLIC discovery route, merged OUTSIDE `require_session`.
pub fn discovery_routes() -> Router<AppState> {
    Router::new().route("/api/discovery", get(get_discovery))
}

/// Discovery query parameters (camelCase / kebab wire). `q` is the
/// free-text title filter; `band`/`mode`/`kind` are matched across a net's
/// connections; `category`/`type` are the net taxonomy
/// filters; `sort` selects the ordering. All optional — an empty query lists
/// all upcoming Listed occurrences soonest-first.
#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
struct DiscoveryParams {
    #[serde(rename = "q")]
    name: Option<String>,
    band: Option<String>,
    mode: Option<String>,
    kind: Option<String>,
    country: Option<String>,
    state: Option<String>,
    grid: Option<String>,
    category: Option<String>,
    #[serde(rename = "type")]
    net_type: Option<String>,
    sort: Option<String>,
}

impl crate::http::QueryKeyPolicy for DiscoveryParams {
    const REASON: &'static str = "the forward rule is that a filtered read whose URL is publicly shareable cannot be \
         strict: third parties append their own keys to a shared link (fbclid, utm_*, ad-network \
         click ids), so strictness would make a correct request depend on channels outside this \
         project's control. Discovery is the only filtered read that is such a URL — every strict \
         read sits behind a session and is never linked from anywhere. Leniency's cost, a filter \
         key silently dropped, is paid back by the `applied` echo below, which states the filters \
         and sort the server actually used";
}

impl crate::http::LenientQuery for DiscoveryParams {}

impl DiscoveryParams {
    fn into_raw(self) -> RawDiscoveryQuery {
        RawDiscoveryQuery {
            name: self.name,
            band: self.band,
            mode: self.mode,
            kind: self.kind,
            country: self.country,
            state: self.state,
            grid: self.grid,
            net_category: self.category,
            net_type: self.net_type,
            sort: self.sort,
        }
    }
}

/// The REDACTED public projection of one discoverable net, live or upcoming.
///
/// OMITS `ownerAccountIds` and `visibility` — the no-leak property of a public
/// surface — and CARRIES `linkToken`, so the card's title can open the net.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct DiscoveryNetBody {
    /// The net's definition id.
    id: Uuid,
    definition_version: i32,
    /// The specific upcoming occurrence's id.
    occurrence_id: Uuid,
    /// The occurrence's planned start as RFC 3339 UTC; local render is the
    /// client's job.
    scheduled_start_at: String,
    title: String,
    description: Option<String>,
    country: Option<String>,
    state: Option<String>,
    grid: Option<String>,
    net_category: &'static str,
    net_type: &'static str,
    expected_duration_minutes: Option<i32>,
    /// The net's permalink token — the client builds `/nets/t/{linkToken}` from
    /// it. Present for every net on this read BECAUSE every net on
    /// this read is Listed: the token of a net already published here is not a
    /// capability anyone lacks. See this module's doc for what stays redacted.
    link_token: String,
    /// Every way this net can be reached, in the owner's order —
    /// and the only place a connection fact appears on the
    /// card. The flat `band`/`mode`/`plannedFrequencyHz` mirror that used to
    /// sit beside it went stale the moment a net dropped its last RF way.
    connections: Vec<NetConnectionWire>,
    /// The connection that satisfied the band, mode or connection-kind filter,
    /// or `null` when none of the three was applied.
    ///
    /// Serialized even when null: `null` is the positive statement "no filter, so
    /// nothing matched", and a client must not have to tell an omitted key from an
    /// absent match. It crosses as an IDENTITY and is never rendered — the card
    /// marks the connection it names and shows that connection's own label.
    matched_connection_id: Option<Uuid>,
}

fn discovery_net_body(row: DiscoveryUpcomingRow) -> DiscoveryNetBody {
    DiscoveryNetBody {
        id: row.definition_id,
        definition_version: row.definition_version,
        occurrence_id: row.occurrence_id,
        scheduled_start_at: rfc3339(row.scheduled_start_at_millis),
        title: row.title,
        description: row.description,
        country: row.country,
        state: row.state,
        grid: row.grid,
        net_category: row.net_category.as_str(),
        net_type: row.net_type.as_str(),
        expected_duration_minutes: row.expected_duration_minutes,
        link_token: row.link_token,
        connections: row
            .connections
            .as_ref()
            .map(connection_bodies)
            .unwrap_or_default(),
        matched_connection_id: row.matched_connection_id,
    }
}

/// The filters and the ordering the server ACTUALLY applied to `upcoming`
///
/// Keyed by the QUERY-PARAM names, not the internal field names: `q` and `type`
/// are renamed on the way in ([`DiscoveryParams`]), and an echo keyed
/// `name`/`netType` could not be joined back to the request a client sent,
/// which is the echo's whole purpose.
///
/// **A dimension that was not filtered is ABSENT, never null**, so the key set
/// alone reads as "what was applied".
///
/// Read the echo as a POSITIVE statement and nothing more: a key present was
/// applied, a key absent was not applied, for reasons this object cannot
/// distinguish. A KNOWN key sent blank (`?band=`, `?q=%20%20`) folds to no filter
/// and is correctly absent, so `?bnad=40m` and no `bnad` at all produce
/// byte-identical echoes; the typo question is answered by the page against its
/// own closed vocabulary. Values are post-trim, post-blank-fold and
/// post-validation, taken from the validated query rather than the raw params.
///
/// `sort` is ALWAYS present, and that is load-bearing rather than incidental.
///
/// `band` and `mode` are `RetiredSort` tokens: they parse to the default `Time`
/// ordering and travel out as `sort_unavailable`, stated below as
/// `sortUnavailable`. Deleting the variants instead would have made `?sort=band`
/// a **400**, never a fallback, because `sort` deserializes into the enum and the
/// request would not reach this struct at all. Every OTHER out-of-vocabulary
/// token is still a 400: falling back for a typo would answer a misspelling with
/// a silently wrong ordering, and the always-stated sort is what makes the
/// legitimate fallback seen rather than silent.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct AppliedQuery {
    #[serde(skip_serializing_if = "Option::is_none")]
    q: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    band: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    mode: Option<&'static str>,
    /// The connection kind applied. Declared after `mode`
    /// because serde emits keys in declaration order and the page renders the
    /// echo in its `FILTER_KEYS` order, where `kind` follows `mode`.
    #[serde(skip_serializing_if = "Option::is_none")]
    kind: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    country: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    state: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    grid: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    category: Option<&'static str>,
    #[serde(rename = "type", skip_serializing_if = "Option::is_none")]
    net_type: Option<&'static str>,
    sort: &'static str,
    /// The sort this request ASKED FOR that the endpoint no longer offers, when
    /// there was one. Present ONLY when a fallback actually
    /// happened — a statement that is always there states nothing.
    ///
    /// It rides this object rather than sitting beside it on `DiscoveryResponse`,
    /// because two echoes on one response is how they come to disagree.
    #[serde(skip_serializing_if = "Option::is_none")]
    sort_unavailable: Option<&'static str>,
    /// The collections this response CUT, by their wire names, in envelope order.
    /// OMITTED when nothing was cut: an optional statement that is always present
    /// states nothing. A collection matching EXACTLY its bound is complete and is
    /// not named — the flag comes from an over-fetch, never from a length test.
    ///
    /// These are HAND-TYPED literals with no compile-time edge to the
    /// `rename_all = "camelCase"` that derives the envelope keys, and the client
    /// carries a third copy, so a `#[serde(rename = …)]` on the envelope would move
    /// the key while this array kept the old spelling with nothing red. The
    /// integration tests asserting it names a cut collection are the only fence:
    /// the client cannot throw on absence, because absence is meaningful.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    truncated: Vec<&'static str>,
}

/// Projects the VALIDATED query the adapter was handed — never the raw params —
/// onto the wire key names.
///
/// BOTH structs are destructured exhaustively, and that is the point. This
/// function is the compensating control for a LENIENT key policy: a dimension the
/// endpoint filters on but does not state here is one the echo silently stops
/// covering. Reading `filters.<field>` one at a time would let a ninth filter
/// compile clean and be absent; a pattern naming every field cannot.
fn applied_query(query: DiscoveryQuery, truncated: Vec<&'static str>) -> AppliedQuery {
    let DiscoveryQuery {
        filters,
        sort,
        sort_unavailable,
    } = query;
    let DiscoveryFilters {
        name,
        band,
        mode,
        kind,
        country,
        state,
        grid,
        net_category,
        net_type,
    } = filters;
    AppliedQuery {
        q: name,
        band: band.map(|b| b.as_str()),
        mode: mode.map(|m| m.as_str()),
        kind: kind.map(|k| k.as_str()),
        country,
        state,
        grid,
        category: net_category.map(|c| c.as_str()),
        net_type: net_type.map(|t| t.as_str()),
        sort: sort.as_query_token(),
        sort_unavailable: sort_unavailable.map(|retired| retired.as_query_token()),
        truncated,
    }
}

/// The discovery response envelope: discrete `activeNow` (live
/// sessions) and `upcoming` (scheduled occurrences) collections — genuinely
/// independent sources, not one derived from the other — plus the `applied`
/// echo describing what shaped them.
///
/// The echo's FILTERS and ORDERING describe `upcoming` only: `activeNow` takes no
/// query, so every live Listed session is a candidate whatever the filters say.
/// The TRUNCATION statement names the collections it applies to and can name
/// either, since both are bounded. It rides `applied` rather than a second
/// envelope, because two echoes on one response is how they come to disagree.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct DiscoveryResponse {
    active_now: Vec<DiscoveryNetBody>,
    upcoming: Vec<DiscoveryNetBody>,
    applied: AppliedQuery,
}

/// `GET /api/discovery` — the public landing read.
/// NO auth/consent extractor: it is world-reachable.
///
/// A deployed instance DOES govern this read per caller: `main` builds
/// `super::api_router_ip_limited`, which layers the per-IP read governor over
/// `public_read_routes()`. That is what [`ACTIVE_NOW_LIMIT`] leans on when it
/// calls its ceiling a backstop rather than a cost control.
///
/// Two out-of-vocabulary outcomes, and they are NOT the same one:
///
/// * an out-of-vocabulary FILTER token, or a sort token that was never a sort
///   key (`?sort=zzz`, `?sort=Time`) → `400 /errors/discovery-query-invalid`
///   with a field-level detail. Never a silently-unfiltered result, and never
///   a silently-reordered one.
/// * a RETIRED sort token — the closed `RetiredSort` set, `band` and `mode`
///   and nothing else → `200`, ordered by the default `time`, with
///   `applied.sortUnavailable` naming the token that could not be honoured.
///
/// A retired token is NOT a `400`: `?sort=band` is out of the sortable
/// vocabulary and answers `200` with a stated fallback, so an already-shared
/// public URL degrades visibly rather than breaking. The [`AppliedQuery`] doc
/// above states the same distinction.
async fn get_discovery(
    State(state): State<AppState>,
    AppQuery(params): AppQuery<DiscoveryParams>,
) -> Result<Json<DiscoveryResponse>, ApiError> {
    let query = parse_discovery_query(params.into_raw())
        .map_err(|e| ApiError::DiscoveryQueryInvalid(e.to_string()))?;
    let now = state.clock.now_epoch_millis();
    let active = state.discovery.list_active_now(ACTIVE_NOW_LIMIT).await?;
    let upcoming = state
        .discovery
        .list_discovery_upcoming(&query, now, DISCOVERY_LIMIT)
        .await?;
    // Envelope order, `activeNow` before `upcoming`: the wire names of the
    // collections that were cut, and nothing when neither was.
    let truncated: Vec<&'static str> = [
        (active.truncated, "activeNow"),
        (upcoming.truncated, "upcoming"),
    ]
    .into_iter()
    .filter_map(|(cut, name)| cut.then_some(name))
    .collect();
    let active_now = active.rows.into_iter().map(discovery_net_body).collect();
    let upcoming = upcoming.rows.into_iter().map(discovery_net_body).collect();
    Ok(Json(DiscoveryResponse {
        active_now,
        upcoming,
        applied: applied_query(query, truncated),
    }))
}
