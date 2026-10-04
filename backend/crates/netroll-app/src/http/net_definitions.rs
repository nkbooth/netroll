// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
//! Net-definition CRUD, gated on consent plus — for CREATE only — a verified
//! email and a callsign. Edit and delete authority is ownership alone, so a
//! cleared callsign never revokes a net you own. Object-level authz reads the
//! owner set from storage: a non-owner gets 403, a missing net 404, and no
//! client-supplied id or role is ever trusted.

use std::collections::BTreeMap;

use axum::Json;
use axum::Router;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post, put};
use netroll_adapters::pg::net_definitions::{AddOwnerOutcome, RemoveOwnerOutcome};
use netroll_domain::admin::{clamp_limit, encode_cursor};
use netroll_domain::audit::AuditAction;
use netroll_domain::authz::can_manage_definition;
use netroll_domain::bot_mitigation::BotVerdict;
use netroll_domain::callsign::parse_callsign;
use netroll_domain::caps::within_cap;
use netroll_domain::net::NetDefinition;
use netroll_domain::net::connection::{
    MAX_CONNECTIONS, NetConnectionError, NetConnectionSet, RawNetConnection, parse_connection_set,
};
use netroll_domain::net::delivery::{RawDeliveryConfig, parse_delivery_config};
use netroll_domain::net::schedule::{
    Frequency, RawSchedule, Schedule, parse_schedule, weekday_token,
};
use netroll_domain::net::validation::{
    NetDefinitionFields, RawNetDefinition, changed_fields, parse_net_definition_fields,
};
use netroll_domain::net::wire::{NetConnectionWire, wire_connections};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::middleware::consent::ConsentedAccount;
use crate::middleware::session::CurrentAccount;

use super::admin::PageQuery;
use super::problem::ApiError;
use super::{AppJson, AppState, AppStrictQuery, rfc3339, tokens};

/// The session-gated net-definition routes, merged into the protected tree
/// (kebab plural; verbs by HTTP method).
pub fn net_definition_routes() -> Router<AppState> {
    Router::new()
        .route(
            "/api/net-definitions",
            post(create_net_definition).get(list_owned_definitions),
        )
        .route(
            "/api/net-definitions/{id}",
            get(get_net_definition)
                .put(update_net_definition)
                .delete(delete_net_definition),
        )
        // Owner management — same session + consent gate; the
        // acting account must own the net (checked in-handler via load_owned).
        .route("/api/net-definitions/{id}/owners", post(add_owner_handler))
        .route(
            "/api/net-definitions/{id}/owners/{account_id}",
            delete(remove_owner_handler),
        )
        // Schedule sub-resource — gated on consent + ownership only
        // (via load_owned), NOT the acting account's callsign/verified: setting
        // a schedule is a management action, and ownership is the authority
        // (the owner-management posture). Setting a schedule does NOT bump
        // definition_version (it is not a field edit).
        .route(
            "/api/net-definitions/{id}/schedule",
            get(get_schedule_handler)
                .put(set_schedule_handler)
                .delete(clear_schedule_handler),
        )
        .route(
            "/api/net-definitions/{id}/occurrences",
            get(list_occurrences_handler),
        )
        // Connections sub-resource — same consent +
        // ownership gate as schedule (via `load_owned`). A SUB-RESOURCE rather
        // than a field on the scalar PUT, so a lost race raises a conflict on
        // the connection list ALONE and never on fields nobody was contending.
        // Unlike its two neighbours it DOES bump `definition_version`: a
        // session's provenance stamp is `(id, definition_version)`.
        .route(
            "/api/net-definitions/{id}/connections",
            put(set_connections_handler),
        )
        // Delivery-config sub-resource — same consent + ownership
        // gate as schedule (via load_owned), NOT callsign/verified. Setting
        // delivery config does NOT bump definition_version (not a field edit).
        .route(
            "/api/net-definitions/{id}/delivery-config",
            get(get_delivery_config_handler)
                .put(set_delivery_config_handler)
                .delete(clear_delivery_config_handler),
        )
}

/// The PUBLIC (unauthenticated) read-by-token route, plus the two "no token
/// supplied" path shapes.
///
/// A request with NO token segment — the bare `by-token` or its trailing-slash
/// form — does not match the `{token}` capture, and the bare path would
/// otherwise collide with the session-gated `/api/net-definitions/{id}` route
/// as `id="by-token"`. Both are registered here and refused with the same
/// uniform 404.
pub fn public_net_routes() -> Router<AppState> {
    Router::new()
        .route(
            "/api/net-definitions/by-token/{token}",
            get(get_net_by_token),
        )
        .route("/api/net-definitions/by-token", get(missing_link_token))
        .route("/api/net-definitions/by-token/", get(missing_link_token))
}

/// Handles the "no token supplied" path shapes for the public read — the same
/// uniform 404 as any other non-resolving token, never a distinct signal.
async fn missing_link_token() -> Result<Json<PublicNetBody>, ApiError> {
    Err(ApiError::NetDefinitionNotFound)
}

/// Inbound net-definition body: the SCALAR fields, every one as its raw wire
/// shape (enums as kebab tokens, everything else as strings).
/// `definitionVersion` is deliberately NOT a field — the version is
/// server-owned and any client value is ignored. Nor is any connection fact:
/// frequency, band, mode, offset, tone, node and reflector each belong to a
/// connection, written on create through [`CreateNetDefinitionRequest`] and
/// afterwards through the `/connections` sub-resource.
#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
struct NetDefinitionRequest {
    title: Option<String>,
    description: Option<String>,
    country: Option<String>,
    state: Option<String>,
    grid: Option<String>,
    net_category: Option<String>,
    net_type: Option<String>,
    expected_duration: Option<String>,
    visibility: Option<String>,
    /// Bot-mitigation form token — fetched from `GET
    /// /api/form-tokens` when the form rendered. Absent when mitigation is off.
    form_token: Option<String>,
    /// Honeypot field — a hidden input real users leave empty. Any
    /// non-empty value silently drops the create. Named `hp_field` rather than
    /// something like `website`/`url`, which some password managers/privacy
    /// extensions autofill into hidden inputs regardless of visibility because
    /// the name matches a known profile field.
    hp_field: Option<String>,
    /// Every key the body carried that no field above reads. Screened by
    /// [`Self::refuse_connection_keys`]: a key naming a CONNECTION fact is
    /// refused, because the scalar route used to drop it with a `200` and the
    /// caller believed the edit had landed. Any other unread key stays ignored.
    /// `deny_unknown_fields` could not draw that line, and serde does not support
    /// it under a `flatten` anyway.
    #[serde(flatten)]
    unrecognised: BTreeMap<String, serde_json::Value>,
}

/// The keys a scalar body may not carry: the connection list itself, every flat
/// connection key the older request accepted, and the two exact-Hz spellings a
/// connection entry speaks. The Hz pair is listed because it is the spelling the
/// API now teaches, so it is the one a client misplacing a connection fact most
/// plausibly sends; unlisted, it would be the one that still dropped with a `200`.
const CONNECTION_KEYS: [&str; 12] = [
    "connections",
    "plannedFrequency",
    "plannedFrequencyHz",
    "band",
    "mode",
    "repeaterOffset",
    "repeaterOffsetHz",
    "toneMode",
    "toneValue",
    "echolinkNode",
    "reflector",
    "allstarNode",
];

impl NetDefinitionRequest {
    /// The definition's own slug for a body that carries a connection fact on
    /// the scalar route. One `400` naming every offending key, so a client
    /// still sending the older flat shape learns where the facts went
    /// rather than watching them vanish.
    fn refuse_connection_keys(&self) -> Result<(), ApiError> {
        let keys: Vec<&str> = self
            .unrecognised
            .keys()
            .map(String::as_str)
            .filter(|key| CONNECTION_KEYS.contains(key))
            .collect();
        if keys.is_empty() {
            return Ok(());
        }
        Err(ApiError::NetDefinitionInvalid(format!(
            "{}: not a field of the net definition — a net's ways in (frequency, band, mode, \
             offset, tone, node, reflector) are written through `connections` on create and \
             through PUT /api/net-definitions/{{id}}/connections afterwards",
            keys.join(", ")
        )))
    }

    fn into_raw(self) -> RawNetDefinition {
        RawNetDefinition {
            title: self.title,
            description: self.description,
            country: self.country,
            state: self.state,
            grid: self.grid,
            net_category: self.net_category,
            net_type: self.net_type,
            expected_duration: self.expected_duration,
            visibility: self.visibility,
        }
    }
}

/// The create body: the scalar fields plus the connection list the net is born
/// with. `connections` speaks the SAME vocabulary as
/// `PUT …/{id}/connections` and is parsed by the same `parse_connection_set`,
/// so create and edit cannot drift; a body carrying none — including one still
/// sending the retired flat `plannedFrequency`/`band`/`mode` keys — is refused
/// with a stated reason (`NetConnectionSet::new` refuses an empty set), never
/// accepted into a net with no way to reach it.
#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
struct CreateNetDefinitionRequest {
    #[serde(flatten)]
    definition: NetDefinitionRequest,
    connections: Vec<ConnectionRequest>,
}

/// Serializes a whole set in the owner's order. `pub(crate)` so the discovery
/// card reuses this ONE serializer rather than growing a parallel one.
///
/// The SHAPE lives in `netroll-domain` ([`NetConnectionWire`]). The session
/// snapshot lives in `netroll-adapters`, which `netroll-app` depends on and not
/// the reverse, so a shape declared here could never be read back by the
/// snapshot — which is what leaves exactly ONE shape across the crate boundary.
pub(crate) fn connection_bodies(connections: &NetConnectionSet) -> Vec<NetConnectionWire> {
    wire_connections(connections)
}

/// Outbound net-definition view: Hz frequencies as integers, enums as their
/// kebab tokens, timestamps as RFC 3339. Optionals serialize as `null` (the
/// `AccountBody` convention).
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct NetDefinitionBody {
    id: Uuid,
    definition_version: i32,
    title: String,
    description: Option<String>,
    country: Option<String>,
    state: Option<String>,
    grid: Option<String>,
    /// Every way to reach this net, in the owner's order — and
    /// the only place a connection fact appears on the body.
    connections: Vec<NetConnectionWire>,
    net_category: &'static str,
    net_type: &'static str,
    expected_duration_minutes: Option<i32>,
    visibility: &'static str,
    /// Plaintext permalink token — the owner needs it to build the share URL.
    link_token: String,
    owner_account_ids: Vec<Uuid>,
    /// Owners paired with their callsign for display — the same
    /// ordered set as `owner_account_ids`, enriched for the owner panel.
    owners: Vec<OwnerBody>,
    created_at: String,
    updated_at: String,
    /// RFC 3339 archival instant, or `null` for an active net.
    archived_at: Option<String>,
}

/// One owner in the owner-facing body: the account id plus its callsign (may
/// be `null` if the account has not reserved one).
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct OwnerBody {
    account_id: Uuid,
    callsign: Option<String>,
}

/// Builds the owner-facing body, fetching the owner set with callsigns for
/// display. One extra query — the owner panel needs the callsigns.
async fn net_definition_body(
    state: &AppState,
    def: &NetDefinition,
) -> Result<NetDefinitionBody, ApiError> {
    let owners = state
        .net_definitions
        .owners_with_callsign(def.id)
        .await?
        .into_iter()
        .map(|(account_id, callsign)| OwnerBody {
            account_id,
            callsign,
        })
        .collect();
    Ok(build_net_definition_body(def, owners))
}

/// Pure body construction from a definition and its resolved owner set — shared
/// by the DB-backed [`net_definition_body`] and the bot-mitigation
/// success-shaped path, which fabricates a body without touching storage.
fn build_net_definition_body(def: &NetDefinition, owners: Vec<OwnerBody>) -> NetDefinitionBody {
    NetDefinitionBody {
        id: def.id,
        definition_version: def.definition_version,
        title: def.title.clone(),
        description: def.description.clone(),
        country: def.country.clone(),
        state: def.state.clone(),
        grid: def.grid.clone(),
        connections: connection_bodies(&def.connections),
        net_category: def.net_category.as_str(),
        net_type: def.net_type.as_str(),
        expected_duration_minutes: def.expected_duration_minutes,
        visibility: def.visibility.as_str(),
        link_token: def.link_token.clone(),
        owner_account_ids: def.owner_account_ids.clone(),
        owners,
        created_at: rfc3339(def.created_at_millis),
        updated_at: rfc3339(def.updated_at_millis),
        archived_at: def.archived_at_millis.map(rfc3339),
    }
}

/// One net on the "My Nets" Owned-tab wire: the full owner-facing definition
/// body plus the two fields that body alone can't answer — whether it's
/// live right now, and when it next runs. `#[serde(flatten)]` inlines
/// `definition`'s fields alongside these two, matching the frontend's
/// `OwnedNet extends NetDefinition` shape (a flat extension, not nested).
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct OwnedNetBody {
    #[serde(flatten)]
    definition: NetDefinitionBody,
    /// The currently-live net-session id, or `null` when not live.
    live_session_id: Option<Uuid>,
    /// RFC 3339 UTC start of the soonest upcoming occurrence, or `null` when
    /// unscheduled.
    next_occurrence_at: Option<String>,
}

/// One page of the caller's owned nets — the house page envelope every paged
/// read publishes. `nextCursor` is explicitly `null` on the last page rather
/// than omitted, because clients branch on it to offer "load more".
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct OwnedNetsPageBody {
    items: Vec<OwnedNetBody>,
    next_cursor: Option<String>,
}

/// `GET /api/net-definitions?limit=&cursor=` — one page of the caller's own
/// ACTIVE (non-archived) net definitions, newest first, for the "My Nets" Owned
/// tab. Consent-gated like the sibling
/// `GET /api/net-definitions/{id}` — no ownership check needed beyond the
/// scoping query itself, since it only ever returns the caller's own nets.
///
/// The per-row work is three queries a net, so the bound lands in SQL before the
/// loop, never as a truncate after it. STRICT: a dropped cursor would silently
/// restart the page at 1, so an unrecognised parameter, a malformed `limit`, or a
/// malformed cursor is `400 /errors/validation`. A well-formed cursor is honoured
/// as a keyset position whatever read issued it — `PageQuery::cursor()` parses,
/// it does not verify.
async fn list_owned_definitions(
    State(state): State<AppState>,
    ConsentedAccount(current): ConsentedAccount,
    AppStrictQuery(params): AppStrictQuery<PageQuery>,
) -> Result<Json<OwnedNetsPageBody>, ApiError> {
    let page = state
        .net_definitions
        .list_owned_page(
            current.account_id,
            clamp_limit(params.limit),
            params.cursor()?,
        )
        .await?;
    let now = state.clock.now_epoch_millis();

    let mut nets = Vec::with_capacity(page.rows.len());
    for def in &page.rows {
        let owners = state
            .net_definitions
            .owners_with_callsign(def.id)
            .await?
            .into_iter()
            .map(|(account_id, callsign)| OwnerBody {
                account_id,
                callsign,
            })
            .collect();
        let live_session_id = state
            .net_sessions
            .find_live_session_id_by_definition(def.id)
            .await?;
        let next_occurrence_at = state
            .schedules
            .upcoming_for_definition(def.id, now)
            .await?
            .into_iter()
            .next()
            .map(|o| rfc3339(o.scheduled_start_at_millis));
        nets.push(OwnedNetBody {
            definition: build_net_definition_body(def, owners),
            live_session_id,
            next_occurrence_at,
        });
    }
    Ok(Json(OwnedNetsPageBody {
        items: nets,
        next_cursor: page.next.map(encode_cursor),
    }))
}

/// The PUBLIC, minimal projection served by the read-by-token endpoint.
/// Deliberately OMITS `ownerAccountIds` (the surface is
/// unauthenticated and world-reachable — it must not leak owner identities)
/// and `linkToken` (the caller already holds the token).
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct PublicNetBody {
    id: Uuid,
    title: String,
    description: Option<String>,
    country: Option<String>,
    state: Option<String>,
    grid: Option<String>,
    /// Every way to reach this net, in the owner's order.
    connections: Vec<NetConnectionWire>,
    net_category: &'static str,
    net_type: &'static str,
    expected_duration_minutes: Option<i32>,
    visibility: &'static str,
    created_at: String,
    updated_at: String,
    /// RFC 3339 archival instant, or `null` for an active net.
    /// The permalink stays resolvable when archived — the client renders "this
    /// net was archived" rather than presenting it as live, and the token is
    /// never 404'd (a shared link must keep working).
    archived_at: Option<String>,
}

fn public_net_body(def: &NetDefinition) -> PublicNetBody {
    PublicNetBody {
        id: def.id,
        title: def.title.clone(),
        description: def.description.clone(),
        country: def.country.clone(),
        state: def.state.clone(),
        grid: def.grid.clone(),
        connections: connection_bodies(&def.connections),
        net_category: def.net_category.as_str(),
        net_type: def.net_type.as_str(),
        expected_duration_minutes: def.expected_duration_minutes,
        visibility: def.visibility.as_str(),
        created_at: rfc3339(def.created_at_millis),
        updated_at: rfc3339(def.updated_at_millis),
        archived_at: def.archived_at_millis.map(rfc3339),
    }
}

/// `POST /api/net-definitions` — creates a definition owned solely by the
/// creating account, born with the connection list the body names.
/// Gate order: consent (`ConsentedAccount`) → verified →
/// callsign → validate (scalar fields, then connections) → insert. Validation
/// fails before any row is written — no partial definition.
async fn create_net_definition(
    State(state): State<AppState>,
    ConsentedAccount(current): ConsentedAccount,
    AppJson(body): AppJson<CreateNetDefinitionRequest>,
) -> Result<Response, ApiError> {
    // Rate-limit at the TOP, before any DB work: a scripted
    // create flood is capped at 429 + `Retry-After`, keyed on the account.
    if let Err(retry_after_secs) = state
        .net_creation_limiter
        .check(&current.account_id.to_string())
    {
        return Err(ApiError::RateLimited { retry_after_secs });
    }

    let account = state
        .accounts
        .find_by_id(current.account_id)
        .await?
        .ok_or(ApiError::Unauthenticated)?;

    // Defensive (mirroring `set_callsign`): unreachable via today's
    // always-verified magic-link flow, but a fast-follow auth method
    // may not verify — refuse rather than assume.
    if account.email_verified_at_millis.is_none() {
        return Err(ApiError::EmailUnverified);
    }
    // Net creation is the first callsign-gated action.
    if account.callsign.is_none() {
        return Err(ApiError::CallsignRequired);
    }

    // Capture the bot-mitigation fields before the body is consumed into raw.
    let form_token = body.definition.form_token.clone();
    let honeypot = body.definition.hp_field.clone();
    let CreateNetDefinitionRequest {
        definition,
        connections,
    } = body;
    definition.refuse_connection_keys()?;
    let fields = parse_net_definition_fields(definition.into_raw())
        .map_err(|e| ApiError::NetDefinitionInvalid(e.to_string()))?;
    // The list a net is born with, through the same parser and the same
    // refusal shape the `/connections` sub-resource uses. There is no existing
    // set to resolve ids against, so every id is minted here.
    let raw = identify_connections(connections, None)?;
    let connections = parse_connection_set(raw, None).map_err(connections_refusal)?;

    let now = state.clock.now_epoch_millis();

    // A honeypot/timing failure returns the SAME success-shaped 201 a real
    // create yields, but writes NO row. Run AFTER validation, so a malformed
    // submission 400s identically for bot and human, and BEFORE any write.
    if state
        .bot_mitigation
        .verify(form_token.as_deref(), honeypot.as_deref(), now)
        == BotVerdict::Bot
    {
        tracing::info!(account_id = %account.id, "net creation dropped by bot mitigation");
        let link_token = tokens::generate_link_token();
        let fake = fabricated_definition(&fields, &connections, account.id, &link_token, now);
        let owners = vec![OwnerBody {
            account_id: account.id,
            callsign: account.callsign.clone(),
        }];
        return Ok((
            StatusCode::CREATED,
            Json(build_net_definition_body(&fake, owners)),
        )
            .into_response());
    }

    // Per-account max-nets cap, counted just before the insert. A benign
    // concurrent-create over-count is accepted: a determined abuser gaining
    // cap+1 nets is not catastrophic, and no single row exists to lock the
    // account's net count on. The OWNER cap, by contrast, is atomic inside
    // `add_owner`.
    let owned = state
        .net_definitions
        .count_active_owned_nets(account.id)
        .await?;
    if !within_cap(owned as usize, state.max_nets_per_user) {
        return Err(ApiError::MaxNetsPerUserReached);
    }

    // Mint the plaintext permalink token in the app layer (entropy is an
    // app-layer effect); the adapter stays a pure writer. Never logged
    // (secret-adjacent — the "never log tokens" rule).
    let link_token = tokens::generate_link_token();
    let def = state
        .net_definitions
        .create(&fields, &connections, account.id, &link_token, now)
        .await?;
    // Ids only — the definition's geography (grid/state) is PII-ish and its
    // field values are never logged.
    tracing::info!(
        account_id = %account.id,
        net_definition_id = %def.id,
        "net definition created"
    );
    // A lifecycle event: gives a net a history an admin can filter to. Ids
    // only — the title is user-authored text and never enters `audit_log`.
    super::audit::append_audit(
        &state,
        account.id,
        AuditAction::NetCreated.as_str(),
        super::audit::AuditSubject::definition(def.id),
        None,
        now,
    )
    .await;
    let body = net_definition_body(&state, &def).await?;
    Ok((StatusCode::CREATED, Json(body)).into_response())
}

/// Builds an in-memory definition for the bot-mitigation success-shaped response
/// — a well-formed 201 body that is NEVER persisted. Uses the
/// already-validated fields and connection set, a fresh unused link token, and
/// the acting account as sole owner, so the shape is indistinguishable from a
/// real create without leaking that a honeypot/timing check fired.
fn fabricated_definition(
    fields: &NetDefinitionFields,
    connections: &NetConnectionSet,
    owner_account_id: Uuid,
    link_token: &str,
    now: u64,
) -> NetDefinition {
    NetDefinition {
        id: Uuid::now_v7(),
        definition_version: 1,
        title: fields.title.clone(),
        description: fields.description.clone(),
        country: fields.country.clone(),
        state: fields.state.clone(),
        grid: fields.grid.as_ref().map(|g| g.as_str().to_owned()),
        connections: connections.clone(),
        net_category: fields.net_category,
        net_type: fields.net_type,
        expected_duration_minutes: fields.expected_duration_minutes,
        visibility: fields.visibility,
        link_token: link_token.to_owned(),
        owner_account_ids: vec![owner_account_id],
        created_at_millis: now,
        updated_at_millis: now,
        archived_at_millis: None,
    }
}

/// `GET /api/net-definitions/by-token/{token}` — the PUBLIC read. NO
/// auth/consent extractor: the unguessable token is the only
/// capability. Every non-resolving token — malformed, unknown, or absent —
/// yields the SAME uniform `404 /errors/net-definition-not-found`, identical
/// to a net that does not exist, so an Unlisted net's existence is
/// unobservable without its token. It NEVER returns 401/403 (a wrong token is
/// "not found", not "forbidden") and its body omits owner identities.
async fn get_net_by_token(
    State(state): State<AppState>,
    Path(token): Path<String>,
) -> Result<Json<PublicNetBody>, ApiError> {
    let def = state
        .net_definitions
        .find_by_link_token(&token)
        .await?
        .ok_or(ApiError::NetDefinitionNotFound)?;
    Ok(Json(public_net_body(&def)))
}

/// Loads a definition and enforces ownership, returning either the definition
/// or the correct refusal (404 missing, 403 not-owner).
async fn load_owned(
    state: &AppState,
    id: Uuid,
    current: CurrentAccount,
) -> Result<NetDefinition, ApiError> {
    let def = state
        .net_definitions
        .find_by_id(id)
        .await?
        .ok_or(ApiError::NetDefinitionNotFound)?;
    // The owner set is the DB's, never a client-supplied id/role.
    if !can_manage_definition(&def.owner_account_ids, current.account_id) {
        return Err(ApiError::Forbidden);
    }
    Ok(def)
}

/// `GET /api/net-definitions/{id}` — owner-scoped read (the edit form's
/// loader).
async fn get_net_definition(
    State(state): State<AppState>,
    ConsentedAccount(current): ConsentedAccount,
    Path(id): Path<Uuid>,
) -> Result<Json<NetDefinitionBody>, ApiError> {
    let def = load_owned(&state, id, current).await?;
    Ok(Json(net_definition_body(&state, &def).await?))
}

/// `PUT /api/net-definitions/{id}` — owner edit of the SCALAR fields.
/// The server increments `definition_version`; last-write-wins, no CAS.
/// Callsign/verified are NOT re-checked — ownership is the authority on edit.
/// The connection list is not this request's to change: it cannot carry one,
/// and `PUT …/{id}/connections` is where the list is replaced, under its own
/// compare-and-swap.
async fn update_net_definition(
    State(state): State<AppState>,
    ConsentedAccount(current): ConsentedAccount,
    Path(id): Path<Uuid>,
    AppJson(body): AppJson<NetDefinitionRequest>,
) -> Result<Json<NetDefinitionBody>, ApiError> {
    // Bound (not discarded) because the audit append below needs the PRE-edit
    // state to tell a real change from a no-op re-save.
    let existing = load_owned(&state, id, current).await?;

    body.refuse_connection_keys()?;
    let fields = parse_net_definition_fields(body.into_raw())
        .map_err(|e| ApiError::NetDefinitionInvalid(e.to_string()))?;
    let now = state.clock.now_epoch_millis();
    let changed = changed_fields(&existing, &fields);
    let updated = match state.net_definitions.update(id, &fields, now).await {
        Ok(def) => def,
        // Raced to deleted between the ownership load and the write.
        Err(sqlx::Error::RowNotFound) => return Err(ApiError::NetDefinitionNotFound),
        Err(err) => return Err(err.into()),
    };
    tracing::info!(
        account_id = %current.account_id,
        net_definition_id = %id,
        "net definition updated"
    );
    // Only a real edit is an event: this is a full-replace PUT, so an unchanged
    // re-save would otherwise bury genuine edits in noise (the same
    // discrimination `DisableOutcome`/`newly_revoked` make). Metadata carries
    // field NAMES only — a value would put user-authored text in `audit_log`.
    if !changed.is_empty() {
        super::audit::append_audit(
            &state,
            current.account_id,
            AuditAction::NetUpdated.as_str(),
            super::audit::AuditSubject::definition(id),
            Some(serde_json::json!({ "changedFields": changed })),
            now,
        )
        .await;
    }
    Ok(Json(net_definition_body(&state, &updated).await?))
}

/// Inbound connection-list write: the full ordered list plus
/// the version the client believes it is editing.
#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
struct ConnectionsRequest {
    expected_definition_version: Option<i32>,
    connections: Vec<ConnectionRequest>,
}

/// One inbound connection, in the READ's vocabulary: the same keys and types
/// [`NetConnectionWire`] serves, so a list read from `GET` is accepted by the
/// write unmodified. Enums arrive as kebab tokens and text as
/// text; the two frequency facts arrive as exact-Hz integers
/// (`plannedFrequencyHz`, `repeaterOffsetHz`), not decimal-MHz strings.
///
/// One vocabulary for one concept. The write once took MHz strings, which left
/// it the only body in the API speaking MHz against the only read speaking Hz.
/// The retired spellings are REFUSED, not accepted beside the new ones, because
/// accepting both would mint a which-wins question.
///
/// `id` is echoed back from a previous read so a connection keeps its identity
/// across an edit; a new connection omits it. `position` is accepted so an
/// unmodified echo round-trips, and when present must equal the entry's place.
#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
struct ConnectionRequest {
    id: Option<String>,
    kind: Option<String>,
    planned_frequency_hz: Option<i64>,
    band: Option<String>,
    mode: Option<String>,
    repeater_offset_hz: Option<i64>,
    tone_mode: Option<String>,
    tone_value: Option<String>,
    node: Option<String>,
    reflector: Option<String>,
    network: Option<String>,
    talkgroup: Option<String>,
    label: Option<String>,
    detail: Option<String>,
    position: Option<i32>,
    /// Every key the entry carried that no field above reads. Screened by
    /// [`Self::refuse_retired_keys`] for the two spellings this write retired: a
    /// retired key silently dropped is a `200` the owner believes landed.
    /// `deny_unknown_fields` is refused for this job because a serde rejection
    /// surfaces as the generic `/errors/validation` problem, with no
    /// `connectionIndex` and no word on where the value now goes.
    #[serde(flatten)]
    unrecognised: BTreeMap<String, serde_json::Value>,
}

/// The connection-entry spellings the write no longer takes, each beside the
/// key that replaced it. A DIFFERENT list from [`CONNECTION_KEYS`]: that one
/// names keys a SCALAR body may not carry at all (where `plannedFrequency` is
/// the retired flat spelling); this one names spellings a connection ENTRY may
/// not carry.
const RETIRED_CONNECTION_KEYS: [(&str, &str); 2] = [
    ("plannedFrequency", "plannedFrequencyHz"),
    ("repeaterOffset", "repeaterOffsetHz"),
];

impl ConnectionRequest {
    /// Refuses an entry carrying a retired decimal-MHz spelling, naming the entry
    /// and where each value now goes. One `400` names EVERY retired key on the
    /// entry, so a repeater sending both old spellings learns both in one round
    /// trip. Collection stops at this entry: `connectionIndex` is singular by
    /// contract, so a second entry's faults must not fold under the wrong index.
    fn refuse_retired_keys(&self, index: usize) -> Result<(), ApiError> {
        let moved: Vec<String> = RETIRED_CONNECTION_KEYS
            .iter()
            .filter(|(retired, _)| self.unrecognised.contains_key(*retired))
            .map(|(retired, replacement)| format!("`{retired}` (now `{replacement}`)"))
            .collect();
        if moved.is_empty() {
            return Ok(());
        }
        Err(ApiError::NetConnectionsInvalid(
            Some(index),
            format!(
                "connection {index}: {} — no longer a field of a connection; send the exact Hz \
                 integer under the key now named",
                moved.join(", ")
            ),
        ))
    }

    /// Refuses a `position` that disagrees with the entry's place in the
    /// array. Absent is fine (the array order is the owner's order, as it
    /// always was); present and equal is fine (an unmodified echo). Present and
    /// different is a client that believes it reordered by editing values, and
    /// a `200` that moved nothing is the silently-dropped-meaning class this
    /// API refuses.
    fn refuse_misplaced_position(&self, index: usize) -> Result<(), ApiError> {
        match self.position {
            None => Ok(()),
            Some(position) if usize::try_from(position) == Ok(index) => Ok(()),
            Some(position) => Err(ApiError::NetConnectionsInvalid(
                Some(index),
                format!(
                    "connection {index}: `position` {position} does not match the entry's place \
                     in the list — the array order is the owner's order"
                ),
            )),
        }
    }

    /// Resolves this entry's identity against the set the definition holds
    /// today.
    ///
    /// An id is only ever the client's echo of a value the server minted, so
    /// an absent, unparsable or UNKNOWN one is treated as a new connection
    /// rather than refused: the only thing at stake is whether a `via`
    /// reference survives the edit, and an id the definition does not hold has
    /// no `via` pointing at it. Resolving it against `existing` rather than
    /// trusting it is what stops a client naming another account's connection
    /// row — which would be a primary-key violation surfacing as a `500`, and
    /// a probe that answers "does this id exist" by the status code.
    fn into_identified(self, existing: Option<&NetConnectionSet>) -> (Uuid, RawNetConnection) {
        let id = self
            .id
            .as_deref()
            .and_then(|v| Uuid::parse_str(v).ok())
            .filter(|id| existing.is_some_and(|set| set.find(*id).is_some()))
            .unwrap_or_else(Uuid::now_v7);
        (
            id,
            RawNetConnection {
                kind: self.kind,
                planned_frequency_hz: self.planned_frequency_hz,
                band: self.band,
                mode: self.mode,
                repeater_offset_hz: self.repeater_offset_hz,
                tone_mode: self.tone_mode,
                tone_value: self.tone_value,
                node: self.node,
                reflector: self.reflector,
                network: self.network,
                talkgroup: self.talkgroup,
                label: self.label,
                detail: self.detail,
            },
        )
    }
}

/// Resolves a submitted connection list into the domain's raw entries, refusing
/// first the two faults the domain cannot see because they are about the WIRE
/// rather than the connection: a retired spelling, and a `position` that
/// disagrees with the entry's place. One function for both the
/// create and the replace path, so the two routes cannot drift on what a
/// connection entry may carry.
///
/// The set-level cap is answered BEFORE any per-entry screen. This mapping
/// was once infallible and `parse_connection_set`'s cap was the first
/// refusal an over-long list met; a fallible per-entry pass in front of it
/// would tell a 500-entry client about entry 0, then entry 1, and never about
/// the size that actually decides the request. The refusal is built from the
/// domain's own `TooMany` so the over-cap problem body is the one it always was.
fn identify_connections(
    entries: Vec<ConnectionRequest>,
    existing: Option<&NetConnectionSet>,
) -> Result<Vec<(Uuid, RawNetConnection)>, ApiError> {
    if entries.len() > MAX_CONNECTIONS {
        return Err(connections_refusal(NetConnectionError::TooMany {
            max: MAX_CONNECTIONS,
        }));
    }
    entries
        .into_iter()
        .enumerate()
        .map(|(index, entry)| {
            entry.refuse_retired_keys(index)?;
            entry.refuse_misplaced_position(index)?;
            Ok(entry.into_identified(existing))
        })
        .collect()
}

/// Turns a connection-set refusal into its problem, keeping the failing
/// entry's index when the domain named one.
///
/// The index is read off [`NetConnectionError::Entry`] rather than parsed back
/// out of the rendered `detail`: an editor that located the offending row by
/// matching prose would move every message back to the form head the next time
/// a domain error is copy-edited.
fn connections_refusal(error: NetConnectionError) -> ApiError {
    let index = match &error {
        NetConnectionError::Entry { index, .. } => Some(*index),
        _ => None,
    };
    ApiError::NetConnectionsInvalid(index, error.to_string())
}

/// `PUT /api/net-definitions/{id}/connections` — replaces the ordered
/// connection list. Gate is consent + ownership (via
/// `load_owned`), like the sibling sub-resources.
///
/// A full replace rather than a patch: reordering cannot be expressed as a
/// partial update without inventing move semantics.
///
/// `expectedDefinitionVersion` is REQUIRED and CAS-checked, so a second owner
/// editing the same list loses with a `409` instead of silently overwriting.
/// The scalar `PUT /api/net-definitions/{id}` keeps its documented
/// last-write-wins, so no conflict is ever raised on a field nobody contended.
async fn set_connections_handler(
    State(state): State<AppState>,
    ConsentedAccount(current): ConsentedAccount,
    Path(id): Path<Uuid>,
    AppJson(body): AppJson<ConnectionsRequest>,
) -> Result<Json<NetDefinitionBody>, ApiError> {
    let owned = load_owned(&state, id, current).await?;

    let expected_version = body.expected_definition_version.ok_or_else(|| {
        ApiError::NetDefinitionInvalid("expected definition version: is required".to_owned())
    })?;
    let existing = Some(&owned.connections);
    let raw = identify_connections(body.connections, existing)?;
    let connections = parse_connection_set(raw, existing).map_err(connections_refusal)?;

    let now = state.clock.now_epoch_millis();
    let updated = state
        .net_definitions
        .update_connections(id, expected_version, &connections, now)
        .await?
        .ok_or(ApiError::StaleVersion)?;

    tracing::info!(
        account_id = %current.account_id,
        net_definition_id = %id,
        "net connections replaced"
    );
    // Field NAMES only — a connection's label or detail is owner-authored free
    // text and must not land in `audit_log`.
    super::audit::append_audit(
        &state,
        current.account_id,
        AuditAction::NetUpdated.as_str(),
        super::audit::AuditSubject::definition(id),
        Some(serde_json::json!({ "changedFields": ["connections"] })),
        now,
    )
    .await;

    Ok(Json(net_definition_body(&state, &updated).await?))
}

/// Add-a-co-owner body: the target's callsign (the unique, ham-facing
/// identifier — email is PII the adder rarely knows).
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct AddOwnerRequest {
    callsign: String,
}

/// `POST /api/net-definitions/{id}/owners` — adds a co-owner by callsign
/// Gate is consent + ownership only (via `load_owned`) —
/// NOT the acting account's callsign/verified: ownership is the authority,
/// exactly like edit/delete. The target holding a callsign implies a verified
/// email, so "a callsign is required to own a net" is satisfied by
/// construction — no extra check on the target. Idempotent; returns 200 with
/// the refreshed owner-facing body.
async fn add_owner_handler(
    State(state): State<AppState>,
    ConsentedAccount(current): ConsentedAccount,
    Path(id): Path<Uuid>,
    AppJson(body): AppJson<AddOwnerRequest>,
) -> Result<Json<NetDefinitionBody>, ApiError> {
    load_owned(&state, id, current).await?;

    let callsign =
        parse_callsign(&body.callsign).map_err(|e| ApiError::CallsignInvalid(e.to_string()))?;
    let target = state
        .accounts
        .find_by_callsign(callsign.as_str())
        .await?
        .ok_or(ApiError::OwnerNotFound)?;
    // A soft-delete never clears the callsign, so a callsign can still
    // resolve to an account mid-grace-window or (transiently) past it — but
    // adding a departing account as a co-owner is nonsensical: its sessions
    // are already revoked, and it self-heals into a zero-owner archive the
    // moment it finalizes anyway. Treat it the same as "no account holds this
    // callsign" — 404, never a silent 200 that hands ownership to an account
    // on its way out.
    if target.deleted_at_millis.is_some() {
        return Err(ApiError::OwnerNotFound);
    }

    let now = state.clock.now_epoch_millis();
    // Owner cap enforced atomically inside `add_owner` (FOR
    // UPDATE-locked count): a new owner past the cap is refused with a specific
    // 409 and no row written, while a re-add of an existing owner is never
    // cap-refused (idempotent no-op).
    match state
        .net_definitions
        .add_owner(id, target.id, now, state.max_owners_per_net)
        .await?
    {
        AddOwnerOutcome::AtCap => return Err(ApiError::MaxOwnersPerNetReached),
        AddOwnerOutcome::Added => {}
    }
    // Ids only — never callsigns/emails.
    tracing::info!(
        net_definition_id = %id,
        account_id = %current.account_id,
        owner_account_id = %target.id,
        "net owner added"
    );

    let def = state
        .net_definitions
        .find_by_id(id)
        .await?
        .ok_or(ApiError::NetDefinitionNotFound)?;
    Ok(Json(net_definition_body(&state, &def).await?))
}

/// `DELETE /api/net-definitions/{id}/owners/{account_id}` — removes a
/// co-owner. `load_owned` decides 404/403 (net exists + the
/// ACTING account is an owner). The last-owner decision itself — removing the
/// LAST owner is refused with 409 `/errors/last-owner`, so a net can never be
/// voluntarily orphaned through the API (only account-finalize can orphan,
/// and that archives) — happens atomically inside `remove_owner` (a
/// row-locked transaction), NOT here: a handler-side pre-check against
/// `load_owned`'s snapshot would leave a TOCTOU window where two concurrent
/// removals of different owners of the same 2-owner net could each pass a
/// stale guard and jointly orphan the net. Removing an account not in the
/// owner set is 404 `/errors/owner-not-found`. Returns 204 on success.
async fn remove_owner_handler(
    State(state): State<AppState>,
    ConsentedAccount(current): ConsentedAccount,
    Path((id, account_id)): Path<(Uuid, Uuid)>,
) -> Result<Response, ApiError> {
    load_owned(&state, id, current).await?;
    match state.net_definitions.remove_owner(id, account_id).await? {
        RemoveOwnerOutcome::NotAMember => return Err(ApiError::OwnerNotFound),
        RemoveOwnerOutcome::WouldOrphan => return Err(ApiError::LastOwner),
        RemoveOwnerOutcome::Removed => {}
    }
    // Ids only.
    tracing::info!(
        net_definition_id = %id,
        account_id = %current.account_id,
        owner_account_id = %account_id,
        "net owner removed"
    );
    Ok(StatusCode::NO_CONTENT.into_response())
}

/// `DELETE /api/net-definitions/{id}` — owner-initiated delete, which ARCHIVES
/// the net rather than hard-deleting it: the definition row
/// and its occurrences survive for provenance, the archived net leaves
/// discovery, and its future occurrences leave discovery. Idempotent (a second
/// delete on an already-archived net is still 204). Returns 204.
async fn delete_net_definition(
    State(state): State<AppState>,
    ConsentedAccount(current): ConsentedAccount,
    Path(id): Path<Uuid>,
) -> Result<Response, ApiError> {
    load_owned(&state, id, current).await?;
    let now = state.clock.now_epoch_millis();
    let newly_archived = state.net_definitions.archive(id, now).await?;
    tracing::info!(
        account_id = %current.account_id,
        net_definition_id = %id,
        "net definition archived"
    );
    // Idempotent endpoint: only the transition is an event. Re-archiving an
    // already-archived net did nothing and must not read as a second action.
    if newly_archived {
        super::audit::append_audit(
            &state,
            current.account_id,
            AuditAction::NetArchived.as_str(),
            super::audit::AuditSubject::definition(id),
            None,
            now,
        )
        .await;
    }
    Ok(StatusCode::NO_CONTENT.into_response())
}

/// Inbound schedule body: `kind` (`one-off`/`recurring`),
/// `timezone` (IANA), and the kind-specific fields. Maps to the domain
/// [`RawSchedule`]; `parse_schedule` is the single validation authority.
#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
struct ScheduleRequest {
    kind: Option<String>,
    timezone: Option<String>,
    one_off_start_at: Option<String>,
    frequency: Option<String>,
    time_of_day: Option<String>,
    weekday: Option<String>,
    day_of_month: Option<String>,
}

impl ScheduleRequest {
    fn into_raw(self) -> RawSchedule {
        RawSchedule {
            kind: self.kind,
            timezone: self.timezone,
            one_off_start_at: self.one_off_start_at,
            frequency: self.frequency,
            time_of_day: self.time_of_day,
            weekday: self.weekday,
            day_of_month: self.day_of_month,
        }
    }
}

/// Outbound schedule body: the field-for-field serialize
/// mirror of [`ScheduleRequest`], so the rule a client reads round-trips back
/// into the write shape unchanged and re-submitting it is a no-op. Fields the
/// kind does not use are OMITTED rather than sent as `null` (the architecture
/// format rule).
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ScheduleBody {
    kind: &'static str,
    timezone: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    one_off_start_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    frequency: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    time_of_day: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    weekday: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    day_of_month: Option<String>,
}

impl ScheduleBody {
    /// Projects a stored [`Schedule`] onto the wire, spelling enums with the
    /// domain's own tokenizers so the read and write vocabularies cannot drift.
    fn from_schedule(schedule: &Schedule) -> Self {
        match schedule {
            Schedule::OneOff {
                start_at_millis,
                timezone,
            } => Self {
                kind: "one-off",
                timezone: timezone.name().to_owned(),
                one_off_start_at: Some(rfc3339(*start_at_millis)),
                frequency: None,
                time_of_day: None,
                weekday: None,
                day_of_month: None,
            },
            // Weekday and day-of-month are gated on the FREQUENCY, not merely
            // on being present. `parse_schedule` keeps each `None` outside its
            // own frequency, so a consistent row makes the gate a no-op — but a
            // row written by something that bypassed the domain could hold a
            // stray weekday on a daily rule, and serializing it would emit a
            // body contradicting this struct's own "omit what the kind does not
            // use" contract. `parse_schedule` would then discard that field on
            // resubmit, breaking the round-trip for exactly the row whose
            // schedule is already suspect.
            Schedule::Recurring(r) => Self {
                kind: "recurring",
                timezone: r.timezone.name().to_owned(),
                one_off_start_at: None,
                frequency: Some(r.frequency.as_str()),
                time_of_day: Some(format!("{:02}:{:02}", r.hour, r.minute)),
                weekday: match r.frequency {
                    Frequency::Weekly => r.weekday.map(weekday_token),
                    Frequency::Daily | Frequency::Monthly => None,
                },
                day_of_month: match r.frequency {
                    Frequency::Monthly => r.day_of_month.map(|d| d.to_string()),
                    Frequency::Daily | Frequency::Weekly => None,
                },
            },
        }
    }
}

/// One planned occurrence on the wire: server id + the absolute start as an
/// RFC 3339 UTC string. Local rendering is the client's job.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct OccurrenceBody {
    id: Uuid,
    scheduled_start_at: String,
}

/// The occurrence-list response for the owner endpoint.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct OccurrencesBody {
    occurrences: Vec<OccurrenceBody>,
}

/// `PUT /api/net-definitions/{id}/schedule` — sets or replaces the net's
/// schedule. Gate is consent + ownership (via
/// `load_owned`). Validation fails at the boundary before any schedule or
/// occurrence row is written. Returns 200 with the
/// materialized upcoming occurrences.
async fn set_schedule_handler(
    State(state): State<AppState>,
    ConsentedAccount(current): ConsentedAccount,
    Path(id): Path<Uuid>,
    AppJson(body): AppJson<ScheduleRequest>,
) -> Result<Json<OccurrencesBody>, ApiError> {
    load_owned(&state, id, current).await?;
    let schedule =
        parse_schedule(body.into_raw()).map_err(|e| ApiError::ScheduleInvalid(e.to_string()))?;
    let now = state.clock.now_epoch_millis();
    state.schedules.set_schedule(id, &schedule, now).await?;
    tracing::info!(
        account_id = %current.account_id,
        net_definition_id = %id,
        "net schedule set"
    );
    occurrences_response(&state, id, now).await
}

/// `DELETE /api/net-definitions/{id}/schedule` — clears the schedule and its
/// future occurrences; past occurrences are left as
/// provenance. Idempotent; returns 204.
async fn clear_schedule_handler(
    State(state): State<AppState>,
    ConsentedAccount(current): ConsentedAccount,
    Path(id): Path<Uuid>,
) -> Result<Response, ApiError> {
    load_owned(&state, id, current).await?;
    let now = state.clock.now_epoch_millis();
    state.schedules.clear_schedule(id, now).await?;
    tracing::info!(
        account_id = %current.account_id,
        net_definition_id = %id,
        "net schedule cleared"
    );
    Ok(StatusCode::NO_CONTENT.into_response())
}

/// `GET /api/net-definitions/{id}/schedule` — the stored rule, for the edit
/// form to hydrate its schedule panel from. Same consent +
/// ownership gate as the write arms (`load_owned` first, before any read), and
/// the body carries rule fields ONLY — no link token, no owner identity.
/// `204` when the net has no schedule: that is the signal the panel uses to
/// tell "unscheduled" from "still loading", and a schedule has no meaningful
/// empty shape (`kind` is required).
async fn get_schedule_handler(
    State(state): State<AppState>,
    ConsentedAccount(current): ConsentedAccount,
    Path(id): Path<Uuid>,
) -> Result<Response, ApiError> {
    load_owned(&state, id, current).await?;
    match state.schedules.get_schedule(id).await? {
        None => Ok(StatusCode::NO_CONTENT.into_response()),
        Some(schedule) => Ok(Json(ScheduleBody::from_schedule(&schedule)).into_response()),
    }
}

/// `GET /api/net-definitions/{id}/occurrences` — the owner's upcoming
/// occurrences, ascending, RFC 3339 UTC.
async fn list_occurrences_handler(
    State(state): State<AppState>,
    ConsentedAccount(current): ConsentedAccount,
    Path(id): Path<Uuid>,
) -> Result<Json<OccurrencesBody>, ApiError> {
    load_owned(&state, id, current).await?;
    let now = state.clock.now_epoch_millis();
    occurrences_response(&state, id, now).await
}

/// Builds the occurrence-list body for a definition (shared by set + list).
async fn occurrences_response(
    state: &AppState,
    id: Uuid,
    now: u64,
) -> Result<Json<OccurrencesBody>, ApiError> {
    let occurrences = state
        .schedules
        .upcoming_for_definition(id, now)
        .await?
        .into_iter()
        .map(|o| OccurrenceBody {
            id: o.id,
            scheduled_start_at: rfc3339(o.scheduled_start_at_millis),
        })
        .collect();
    Ok(Json(OccurrencesBody { occurrences }))
}

/// Inbound delivery-config body: the raw email
/// list, the raw optional generic webhook URL, and the raw optional Discord
/// webhook URL. Maps to the domain [`RawDeliveryConfig`];
/// `parse_delivery_config` is the single validation authority.
///
/// `#[serde(default)]` means a PUT is a REPLACE for every field: a client that
/// omits `discordWebhookUrl` CLEARS a configured Discord destination, exactly as
/// omitting `emails` or `webhookUrl` clears those. That is deliberate
/// consistency rather than an oversight, and it is pinned by a test
/// (`api_delivery_config.rs`) so it cannot be discovered by an owner instead.
#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
struct DeliveryConfigRequest {
    emails: Vec<String>,
    webhook_url: Option<String>,
    discord_webhook_url: Option<String>,
}

impl DeliveryConfigRequest {
    fn into_raw(self) -> RawDeliveryConfig {
        RawDeliveryConfig {
            emails: self.emails,
            webhook_url: self.webhook_url,
            discord_webhook_url: self.discord_webhook_url,
        }
    }
}

/// Outbound delivery-config view. `webhookSecret` is present ONLY
/// in the PUT response that MINTS a secret (revealed exactly once) — it
/// is `#[serde(skip_serializing_if)]`'d otherwise, so no GET and no non-minting
/// PUT ever carries the plaintext. `webhookSecretSet` is the durable indicator
/// a secret exists.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct DeliveryConfigBody {
    emails: Vec<String>,
    webhook_url: Option<String>,
    /// The configured Discord webhook URL, returned in the clear to the net's
    /// owner — the SAME visibility `webhook_url` has. Not the reveal-once
    /// posture `webhook_secret` uses: the owner
    /// supplied this value, only the owner can reach this endpoint
    /// (`load_owned`), and withholding it would leave the UI unable to show
    /// WHICH channel is configured or let the owner fix a typo.
    discord_webhook_url: Option<String>,
    /// Whether a webhook URL is configured.
    webhook_configured: bool,
    /// Whether a webhook HMAC secret is stored — never the secret value.
    webhook_secret_set: bool,
    /// The freshly minted secret, revealed EXACTLY once at mint. Absent
    /// on GET and on any PUT that did not mint.
    #[serde(skip_serializing_if = "Option::is_none")]
    webhook_secret: Option<String>,
}

/// Builds the read-shape delivery body (no secret plaintext) from stored state.
fn delivery_config_body(
    config: Option<netroll_adapters::pg::delivery_configs::DeliveryConfig>,
    minted_secret: Option<String>,
) -> DeliveryConfigBody {
    match config {
        Some(c) => DeliveryConfigBody {
            webhook_configured: c.webhook_url.is_some(),
            webhook_secret_set: c.webhook_secret_set,
            emails: c.emails,
            webhook_url: c.webhook_url,
            discord_webhook_url: c.discord_webhook_url,
            webhook_secret: minted_secret,
        },
        // No row yet — the empty/"delivery off" shape (a valid state, not 404).
        None => DeliveryConfigBody {
            emails: Vec::new(),
            webhook_url: None,
            discord_webhook_url: None,
            webhook_configured: false,
            webhook_secret_set: false,
            webhook_secret: minted_secret,
        },
    }
}

/// `GET /api/net-definitions/{id}/delivery-config` — reads the net's delivery
/// config. Gate is consent + ownership (via `load_owned`). A
/// net with no config yet returns the empty/off shape (200), not 404. NEVER
/// returns the webhook secret plaintext — only `webhookSecretSet`.
async fn get_delivery_config_handler(
    State(state): State<AppState>,
    ConsentedAccount(current): ConsentedAccount,
    Path(id): Path<Uuid>,
) -> Result<Json<DeliveryConfigBody>, ApiError> {
    load_owned(&state, id, current).await?;
    let config = state.delivery_configs.get(id).await?;
    Ok(Json(delivery_config_body(config, None)))
}

/// `PUT /api/net-definitions/{id}/delivery-config` — sets/replaces the net's
/// delivery targets. Gate is consent + ownership (via
/// `load_owned`), NOT the acting account's callsign/verified (the
/// `set_schedule_handler` posture). Validation fails at the boundary before any
/// row is written.
///
/// The webhook URL is checked here by `parse_delivery_config` →
/// `validate_egress_url`. That is the CONFIG-TIME gate ONLY: it is
/// pure and does NO DNS, so it catches scheme/syntax and bare-IP-literal
/// problems but nothing a hostname resolves to. A host safe at save time can be
/// DNS-rebound before the next delivery, so this is NECESSARY BUT NOT
/// SUFFICIENT — delivery MUST still re-validate live through the
/// `SsrfSafeEgress` resolver-pin at delivery time. Passing here is never
/// a durable guarantee.
///
/// The HMAC secret is minted (OS CSPRNG) ONLY when the config carries a webhook
/// URL AND no secret exists yet; the repo COALESCEs so a URL edit preserves the
/// existing secret. The minted plaintext is returned in this response EXACTLY
/// once and never again.
///
/// A fresh candidate is generated here on EVERY webhook-bearing save, not just
/// when a pre-flight read reports no secret yet — a pre-flight
/// read-then-decide would be a TOCTOU gap: a concurrent PUT that clears the
/// webhook could null the stored secret between this handler's read and its
/// write, leaving `webhook_url` set with no secret to sign with. Generating a
/// candidate unconditionally and letting the repo's single-statement
/// `COALESCE` (atomic under Postgres's `ON CONFLICT DO UPDATE` re-evaluation)
/// be the SOLE arbiter of whether it is actually used closes that gap: the
/// candidate is only ever persisted when the row's secret is genuinely absent
/// at commit time, and the reveal-once check (`candidate == stored`) stays
/// correct in every ordering.
async fn set_delivery_config_handler(
    State(state): State<AppState>,
    ConsentedAccount(current): ConsentedAccount,
    Path(id): Path<Uuid>,
    AppJson(body): AppJson<DeliveryConfigRequest>,
) -> Result<Json<DeliveryConfigBody>, ApiError> {
    load_owned(&state, id, current).await?;
    let fields = parse_delivery_config(body.into_raw())
        .map_err(|e| ApiError::DeliveryConfigInvalid(e.to_string()))?;

    // A fresh candidate is minted whenever a GENERIC webhook is configured,
    // regardless of whether one is believed to already exist — the repo's atomic
    // COALESCE (not this handler) decides whether it is actually stored. See
    // the doc comment above for why a pre-check-then-decide would race.
    //
    // A DISCORD destination is deliberately NOT part of this condition:
    // Discord verifies nothing, its webhook URL carries its own
    // bearer token, and minting an HMAC secret for it would imply a signature
    // contract that does not exist.
    let minted = fields
        .webhook_url
        .is_some()
        .then(tokens::generate_webhook_secret);

    let now = state.clock.now_epoch_millis();
    let outcome = state
        .delivery_configs
        .set(id, &fields, minted.as_deref(), now)
        .await?;
    // Ids only — never the webhook URL, emails, or secret.
    tracing::info!(
        account_id = %current.account_id,
        net_definition_id = %id,
        "net delivery config set"
    );

    let config = state.delivery_configs.get(id).await?;
    Ok(Json(delivery_config_body(config, outcome.minted_secret)))
}

/// `DELETE /api/net-definitions/{id}/delivery-config` — clears the net's
/// delivery config, the "delivery off" state. Gate is consent
/// + ownership. Idempotent; returns 204 (the `clear_schedule_handler` shape).
async fn clear_delivery_config_handler(
    State(state): State<AppState>,
    ConsentedAccount(current): ConsentedAccount,
    Path(id): Path<Uuid>,
) -> Result<Response, ApiError> {
    load_owned(&state, id, current).await?;
    state.delivery_configs.clear(id).await?;
    tracing::info!(
        account_id = %current.account_id,
        net_definition_id = %id,
        "net delivery config cleared"
    );
    Ok(StatusCode::NO_CONTENT.into_response())
}
