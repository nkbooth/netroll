// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
//! Favorites and "My Nets": favorite, unfavorite, page, batch-membership.
//!
//! The acting account always comes from the session, so favorites are isolated
//! by construction. Limits are keyed per ACCOUNT, never per IP, because a shared
//! NAT would collapse distinct users. The body omits owners structurally.

use axum::Json;
use axum::Router;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, put};
use netroll_adapters::pg::favorites::FavoriteRow;
use netroll_domain::admin::{MAX_PAGE_LIMIT, clamp_limit, encode_cursor};
use netroll_domain::net::wire::NetConnectionWire;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::middleware::consent::ConsentedAccount;

use super::admin::PageQuery;
use super::net_definitions::connection_bodies;
use super::problem::ApiError;
use super::{AppState, AppStrictQuery, rfc3339};

/// The session-gated favorite routes, merged into the protected tree (kebab
/// plural; the HTTP method carries the verb, so `PUT`/`DELETE` share the
/// resource path and `PUT` makes idempotency natural).
pub fn favorite_routes() -> Router<AppState> {
    Router::new()
        .route("/api/favorites", get(list_favorites))
        .route("/api/favorites/membership", get(membership))
        .route(
            "/api/favorites/{netDefinitionId}",
            put(favorite).delete(unfavorite),
        )
}

/// One favorited net on the wire: the definition's display fields, its
/// connection set, and `favoritedAt`. Omits `ownerAccountIds` and `visibility`;
/// includes `linkToken` (return-link) and `archivedAt` (indicator). Optionals
/// serialize as `null`.
///
/// `pub(crate)` so the personal-data export reuses the SAME redacted
/// per-net favorite shape rather than defining a divergent one — which is also
/// why `connections` reaches the export: whatever a favorite says about a net,
/// an account's copy of its own data says the same.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct FavoriteNetBody {
    id: Uuid,
    title: String,
    description: Option<String>,
    /// Every way to reach this net, in the owner's order. It
    /// replaces the flat `band`/`mode` pair, which went stale the moment a net
    /// dropped its last RF way; the card renders the same connection summary
    /// the owned-nets card and the discovery card do.
    connections: Vec<NetConnectionWire>,
    country: Option<String>,
    state: Option<String>,
    grid: Option<String>,
    net_category: &'static str,
    net_type: &'static str,
    expected_duration_minutes: Option<i32>,
    /// The permalink token — the favoriter builds `/nets/t/{linkToken}` from it.
    link_token: String,
    /// RFC 3339 archival instant, or `null` for an active net (the indicator).
    archived_at: Option<String>,
    /// When the account favorited this net, RFC 3339 UTC.
    favorited_at: String,
}

pub(crate) fn favorite_net_body(row: FavoriteRow) -> FavoriteNetBody {
    FavoriteNetBody {
        id: row.id,
        title: row.title,
        description: row.description,
        connections: connection_bodies(&row.connections),
        country: row.country,
        state: row.state,
        grid: row.grid,
        net_category: row.net_category.as_str(),
        net_type: row.net_type.as_str(),
        expected_duration_minutes: row.expected_duration_minutes,
        link_token: row.link_token,
        archived_at: row.archived_at_millis.map(rfc3339),
        favorited_at: rfc3339(row.favorited_at_millis),
    }
}

/// One page of the account's favorites (camelCase wire) — the house page
/// envelope every paged read publishes. The older `{ favorites }`
/// wrapper existed "so the shape can grow"; growing into this shape is that growth.
///
/// `nextCursor` is explicitly `null` on the last page rather than omitted — the
/// one deliberate exception to the omit-optional-fields rule, because clients
/// branch on it to decide whether to offer "load more".
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct FavoritesPageBody {
    items: Vec<FavoriteNetBody>,
    next_cursor: Option<String>,
}

/// `PUT /api/favorites/{netDefinitionId}` — favorite a net. Idempotent.
/// Gate order: per-account rate limit → net existence (404) → write.
/// There is NO visibility gate: an Unlisted net's id is only learnable by
/// holding its token, so id-knowledge is itself the capability (Dev Notes).
async fn favorite(
    State(state): State<AppState>,
    ConsentedAccount(current): ConsentedAccount,
    Path(net_definition_id): Path<Uuid>,
) -> Result<Response, ApiError> {
    if let Err(retry_after_secs) = state
        .favorite_limiter
        .check(&current.account_id.to_string())
    {
        return Err(ApiError::RateLimited { retry_after_secs });
    }

    // Existence check before the write: a favorite for a net that does
    // not exist is a 404, never an orphan row.
    if state
        .net_definitions
        .find_by_id(net_definition_id)
        .await?
        .is_none()
    {
        return Err(ApiError::NetDefinitionNotFound);
    }

    state
        .favorites
        .add(
            current.account_id,
            net_definition_id,
            state.clock.now_epoch_millis(),
        )
        .await?;
    // Ids only — no PII.
    tracing::info!(
        account_id = %current.account_id,
        net_definition_id = %net_definition_id,
        "favorited"
    );
    Ok(StatusCode::NO_CONTENT.into_response())
}

/// `DELETE /api/favorites/{netDefinitionId}` — unfavorite a net.
/// Idempotent: removing an absent favorite is a success no-op, so no existence
/// check is needed. Rate-limited per account on the write path.
async fn unfavorite(
    State(state): State<AppState>,
    ConsentedAccount(current): ConsentedAccount,
    Path(net_definition_id): Path<Uuid>,
) -> Result<Response, ApiError> {
    if let Err(retry_after_secs) = state
        .favorite_limiter
        .check(&current.account_id.to_string())
    {
        return Err(ApiError::RateLimited { retry_after_secs });
    }

    state
        .favorites
        .remove(current.account_id, net_definition_id)
        .await?;
    tracing::info!(
        account_id = %current.account_id,
        net_definition_id = %net_definition_id,
        "unfavorited"
    );
    Ok(StatusCode::NO_CONTENT.into_response())
}

/// `GET /api/favorites?limit=&cursor=` — one page of the account's "My Nets"
/// Newest-favorited first, keyset-paginated. Returns
/// ONLY the calling account's favorites, each redacted to [`FavoriteNetBody`].
/// Rate-limited per ACCOUNT via
/// `state.favorites_read_limiter` — NOT the IP tower layer, since this is an
/// authenticated, account-isolated read (IP-keying would collapse NAT'd users).
/// Its bucket is separate from the favorite-write limiter's.
///
/// STRICT (forward rule): a dropped cursor would
/// silently restart the page at 1, so an unrecognised parameter, a malformed
/// `limit`, or a MALFORMED cursor (not `millis:uuid`) is `400 /errors/validation`.
/// A well-formed cursor is honoured as a keyset position whatever read issued
/// it — `PageQuery::cursor()` parses, it does not verify, the same as every
/// other `PageQuery` consumer; only the audit log binds a cursor to the read
/// that issued it. Unauthenticated is `401` before any of that
/// (`require_session` is a route layer).
async fn list_favorites(
    State(state): State<AppState>,
    ConsentedAccount(current): ConsentedAccount,
    AppStrictQuery(params): AppStrictQuery<PageQuery>,
) -> Result<Json<FavoritesPageBody>, ApiError> {
    // The strict extractor has ALREADY run by the time this body executes —
    // axum resolves every handler argument first — so a malformed query
    // (`?limitt=5`, `?limit=abc`) is a 400 that never reaches this check and
    // costs no limiter budget. What the limiter gates is everything below it:
    // the cursor parse and the query.
    if let Err(retry_after_secs) = state
        .favorites_read_limiter
        .check(&current.account_id.to_string())
    {
        return Err(ApiError::RateLimited { retry_after_secs });
    }

    let page = state
        .favorites
        .list_for_account_page(
            current.account_id,
            clamp_limit(params.limit),
            params.cursor()?,
        )
        .await?;
    Ok(Json(FavoritesPageBody {
        items: page.rows.into_iter().map(favorite_net_body).collect(),
        next_cursor: page.next.map(encode_cursor),
    }))
}

/// `?ids=<uuid>,<uuid>,…` — the nets a membership read asks about.
///
/// STRICT, and more sharply than the paged reads: a dropped `ids` key would not
/// restart a page, it would answer "none favorited" for every net on the
/// caller's screen, with a 200. `ids` is therefore required (a missing key is a
/// deserialize rejection) and unknown keys are refused.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct MembershipQuery {
    ids: String,
}

impl crate::http::QueryKeyPolicy for MembershipQuery {
    const REASON: &'static str = "the batch favorites-membership read: a dropped `ids` key would answer \
         \"none favorited\" for every net asked about, with a 200, so a silently-dropped key \
         changes the meaning of the answer";
}

impl crate::http::StrictQuery for MembershipQuery {}

impl MembershipQuery {
    /// The asked ids, or [`ApiError::Validation`] when any segment is not a
    /// UUID or there are more than [`MAX_PAGE_LIMIT`] of them.
    ///
    /// Over the cap is REFUSED, never truncated: answering the first 200 and
    /// dropping the rest would report "not favorited" for every id past the
    /// cap — the same silently-changed answer strictness exists to prevent. A
    /// caller that overruns the cap has a bug and is told so.
    fn ids(&self) -> Result<Vec<Uuid>, ApiError> {
        let ids: Vec<Uuid> = self
            .ids
            .split(',')
            .map(str::parse)
            .collect::<Result<_, _>>()
            .map_err(|_| ApiError::validation("ids must be a comma-separated list of net ids"))?;
        if ids.len() > MAX_PAGE_LIMIT {
            return Err(ApiError::validation(
                "ids names more nets than one membership read answers",
            ));
        }
        Ok(ids)
    }
}

/// The membership answer (camelCase wire): the subset of the asked ids
/// the account has favorited, and nothing else — an id not in the list is not
/// favorited (or names no net), and no other account's favorites are visible.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct MembershipBody {
    favorited: Vec<Uuid>,
}

/// `GET /api/favorites/membership?ids=…` — which of up to [`MAX_PAGE_LIMIT`]
/// nets the calling account has favorited. This is
/// the O(1) form of the question the star on a discovery card or a public net
/// page asks; it replaces walking every page of the account's favorites, which
/// spent the read budget in proportion to how many nets an account had
/// favorited. The public net and discovery bodies stay account-less — the
/// account's answer lives here, behind its session.
///
/// Rate-limited per account through the SAME `favorites_read_limiter` bucket
/// as [`list_favorites`], so the two reads share one budget. As there, the
/// strict extractor has already refused a malformed query before this body
/// runs; the limiter gates the id parse and the query. Unauthenticated is `401`
/// before any of that.
async fn membership(
    State(state): State<AppState>,
    ConsentedAccount(current): ConsentedAccount,
    AppStrictQuery(params): AppStrictQuery<MembershipQuery>,
) -> Result<Json<MembershipBody>, ApiError> {
    if let Err(retry_after_secs) = state
        .favorites_read_limiter
        .check(&current.account_id.to_string())
    {
        return Err(ApiError::RateLimited { retry_after_secs });
    }

    let favorited = state
        .favorites
        .membership(current.account_id, &params.ids()?)
        .await?;
    Ok(Json(MembershipBody { favorited }))
}
