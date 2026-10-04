// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
//! Net-session HTTP surface: lifecycle, control handoff, check-in CRUD, roster
//! ordering, roles, export, and the account-less public read (no authz step
//! at all — the id in the URL is the capability). Every other handler
//! validates at the boundary and authorizes server-side, 404 before 403.
//! Every summary is folded from the event log, never assembled from columns.

use std::collections::BTreeMap;

use axum::Json;
use axum::Router;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::http::header;
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post, put};
use netroll_adapters::pg::net_session_roles::RevokeOutcome;
use netroll_adapters::pg::net_sessions::{
    AddCheckInOutcome, ChangeFrequencyOutcome, ClaimControlOutcome, CloseOutcome,
    DefinitionSnapshot, EditCheckInOutcome, HandoffOutcome, ModerateOutcome, NetSessionRow,
    NoteOutcome, OrderModeOutcome, ReorderOutcome, StartOutcome, WorkedStationOutcome,
};
use netroll_domain::audit::AuditAction;
use netroll_domain::authz::{
    Capability, Role, can_manage_definition, can_manage_role, owns_check_in, rank, role_has,
};
use netroll_domain::callsign::{Callsign, parse_callsign};
use netroll_domain::check_in::{
    CheckInSource, Location, Name, Note, Precedence, SignalReport, StayingStatus, TrafficCount,
    parse_location, parse_name, parse_note, parse_signal_report, parse_traffic_count,
};
use netroll_domain::event::{SessionEvent, SessionEventBody};
use netroll_domain::export;
use netroll_domain::fold::{Correction, CorrectionValue};
use netroll_domain::fold::{RosterEntry, RosterOrderMode, SessionLifecycle, SessionState, replay};
use netroll_domain::net::connection::{Via, ViaError, parse_via_text};
use netroll_domain::net::validation::{FrequencyError, parse_frequency_hz};
use netroll_domain::net::wire::{NetConnectionWire, ViaWire, resolve_via, via_label, via_of};
use netroll_domain::profile::{Grid, parse_grid};
use netroll_domain::session_sm;
use netroll_domain::session_sm::{ControlTransitionError, SessionTransitionError};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::middleware::consent::ConsentedAccount;
use crate::middleware::session::CurrentAccount;
use crate::ws::protocol::WireEvent;

use super::problem::ApiError;
use super::undefinable::Undefinable;
use super::{AppJson, AppQuery, AppState, profile_field_message, rfc3339};

/// The session-gated net-session routes, merged into the protected tree.
pub fn net_session_routes() -> Router<AppState> {
    Router::new()
        .route("/api/net-sessions", post(start_session))
        .route("/api/net-sessions/{id}/close", post(close_session))
        .route("/api/net-sessions/{id}/handoff", post(hand_off_control))
        .route("/api/net-sessions/{id}/claim-control", post(claim_control))
        .route("/api/net-sessions/{id}/frequency", post(change_frequency))
        .route("/api/net-sessions/{id}/check-ins", post(add_check_in))
        .route(
            "/api/net-sessions/{id}/check-ins/{check_in_id}",
            put(edit_check_in).delete(remove_check_in),
        )
        .route(
            "/api/net-sessions/{id}/check-ins/{check_in_id}/lock",
            post(acquire_lock).delete(release_lock),
        )
        .route(
            "/api/net-sessions/{id}/check-ins/{check_in_id}/moderate",
            post(moderate_check_in),
        )
        .route("/api/net-sessions/{id}/reorder", post(reorder_roster))
        .route(
            "/api/net-sessions/{id}/worked-station",
            post(set_worked_station),
        )
        .route("/api/net-sessions/{id}/net-note", put(set_net_note))
        .route(
            "/api/net-sessions/{id}/roster-order-mode",
            post(set_roster_order_mode),
        )
        .route(
            "/api/net-sessions/{id}/roles",
            post(grant_role).get(list_roles),
        )
        .route(
            "/api/net-sessions/{id}/roles/{account_id}",
            delete(revoke_role),
        )
        .route(
            "/api/net-sessions/{id}/roster-memory",
            get(get_roster_memory),
        )
        .route(
            "/api/net-sessions/{id}/check-in-autofill",
            get(get_check_in_autofill),
        )
        .route("/api/net-sessions/{id}/export", get(export_session))
        .route("/api/net-sessions/{id}", get(get_session))
        .route("/api/net-sessions/{id}/events", get(session_events_since))
}

/// The `?since=` catch-up cursor: the `seq` the client last folded. Absent → 0
/// → the whole log; `N` → only events with `seq > N`.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct EventsQuery {
    #[serde(default)]
    since: u64,
}

impl crate::http::QueryKeyPolicy for EventsQuery {
    const REASON: &'static str = "?since= is a resume cursor with #[serde(default)]; a dropped key yields since=0, a \
         SUPERSET replayed against a seq-keyed fold, not a wrong answer. Same parameter and same \
         semantics as WsQuery, which is exempted by name";
}

impl crate::http::LenientQuery for EventsQuery {}

/// Start-a-session body: the definition to spawn from.
///
/// There is no `operatingFrequency`: a net has a set of ways to reach it, each
/// with its own frequency or none, so the session freezes the definition's
/// connection set instead and a mid-session move names the connection it moves.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct StartSessionRequest {
    definition_id: Uuid,
}

/// Change-frequency body: WHICH connection moved, and its new frequency as a
/// decimal-MHz string. `connectionId` is required: a net with three ways to
/// reach it has three frequencies, and "the session's frequency" names none.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ChangeFrequencyRequest {
    connection_id: Uuid,
    operating_frequency: String,
}

/// Add-a-check-in body. `clientEventId` is the optimistic quick-add's echo id:
/// the caller mints one per commit and the store reconciles the pending row when
/// the authoritative `checkin.added` echoes it; an omitted id is exempt from the
/// idempotency index.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct AddCheckInRequest {
    callsign: String,
    #[serde(default)]
    client_event_id: Option<Uuid>,
    /// STAFF-ONLY: when present the actor must hold `EditStaffFields`, or the add
    /// is refused 403 with no append. The server bounds it as a string only.
    #[serde(default)]
    signal_report: Option<String>,
    /// Omitted defaults to `in-and-out`. Not staff-gated: a participant may set
    /// their own staying.
    #[serde(default)]
    staying: Option<String>,
    /// Blank/absent → `None`.
    #[serde(default)]
    name: Option<String>,
    /// Free-text; blank/absent → `None`.
    #[serde(default)]
    location: Option<String>,
    /// Independent of `location`: one is a place name, the other a locator. Same
    /// grammar as the profile path; blank/absent → `None`.
    #[serde(default)]
    grid: Option<String>,
    /// WHICH way in this station arrived on: a connection of THIS session's frozen
    /// snapshot, or the operator's own words for a way the owner never listed.
    /// Absent means *nobody recorded it* and is never filled in from the export
    /// connection. An id the snapshot does not hold is refused 404 with no event
    /// appended: writing it would store a `via` no surface could render.
    ///
    /// Accepted on the SELF add path too: how a station got in is a fact about
    /// itself, like `staying`. Changing it afterwards is staff-only.
    #[serde(default)]
    via: Option<ViaWire>,
    /// WHICH STATION passed this check-in's traffic, as a callsign. Absent means
    /// *not relayed*. Distinct from `via`: one records how the traffic travelled,
    /// the other who passed it.
    ///
    /// Refused on the SELF add path with a 403 on PRESENCE, like `signal_report`:
    /// a participant self-checking in reached the net through the app, so there
    /// is no relaying station in that act.
    #[serde(default)]
    relayed_by: Option<String>,
}

/// One derived field correction on the wire, the amber annotation source.
/// `from`/`to` are display strings (`null` when the field was absent on that
/// side). Owner-console only.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct CorrectionBody {
    field: &'static str,
    from: Option<String>,
    to: Option<String>,
    at: String,
}

/// One roster station on the wire (camelCase), projected from the folded log.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct RosterEntryBody {
    check_in_id: Uuid,
    callsign: String,
    added_at: String,
    added_by: Option<Uuid>,
    /// `staff`/`self`. Always projected, so a required wire field.
    source: &'static str,
    /// Staff-console only; the redacted [`PublicRosterEntry`] does not carry it.
    #[serde(skip_serializing_if = "Option::is_none")]
    signal_report: Option<String>,
    /// Always projected (the fold defaults it), so a required wire field.
    staying: &'static str,
    /// Edit-only; omitted when absent.
    #[serde(skip_serializing_if = "Option::is_none")]
    name: Option<String>,
    /// Edit-only; omitted when absent.
    #[serde(skip_serializing_if = "Option::is_none")]
    location: Option<String>,
    /// Staff-console only: a grid is a MORE precise location than the free-text
    /// field the public roster already withholds.
    #[serde(skip_serializing_if = "Option::is_none")]
    grid: Option<String>,
    /// Always projected. Crosses the public wire too: an observer who can see
    /// the ORDER a precedence sort produced needs the reason for it, and the one
    /// case the feature exists for is a station holding emergency traffic.
    precedence: &'static str,
    /// Omitted when absent.
    #[serde(skip_serializing_if = "Option::is_none")]
    traffic: Option<i64>,
    /// The STAFF note. Never crosses the public wire; `public_note` beside it
    /// always does.
    #[serde(skip_serializing_if = "Option::is_none")]
    notes: Option<String>,
    /// Prose written FOR the observer; rides both this owner body and the
    /// redacted [`PublicRosterEntry`], through the same `parse_note` guard.
    #[serde(skip_serializing_if = "Option::is_none")]
    public_note: Option<String>,
    /// Always projected; drives the worked-dim treatment. Staff-console only.
    worked: bool,
    /// The optimistic-concurrency version the modal sends back as `expectedVersion`.
    version: u64,
    /// Omitted when nobody recorded one. STRUCTURED, not a label: the browser
    /// holds the session's connection set and resolves the label itself, which
    /// keeps the wire from carrying a second, staler answer to "what is this
    /// connection called". The webhook is the one surface that carries both,
    /// because it has no browser to resolve in.
    #[serde(skip_serializing_if = "Option::is_none")]
    via: Option<ViaWire>,
    /// A bare callsign, NOT a structured object: a callsign is already the text a
    /// person reads, so a label sibling would imply a resolution step that does
    /// not exist. Owner-console only: it names a third-party station that never
    /// checked in, which puts it with `added_by` and not with `via`.
    #[serde(skip_serializing_if = "Option::is_none")]
    relayed_by: Option<String>,
    /// Owner-console only.
    corrections: Vec<CorrectionBody>,
}

/// The folded session summary — the response body of start (201), close (200),
/// and GET (200). Every projected field comes from folding the event log; the
/// frozen `definition` snapshot comes from the session's own captured copy.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SessionSummaryBody {
    id: Uuid,
    definition_id: Uuid,
    definition_version: i32,
    lifecycle: &'static str,
    /// Every way this session is reachable RIGHT NOW: the frozen snapshot's
    /// connection set with each `frequency.changed` overlaid. Beside the frozen
    /// `definition`, not instead of it: one is what the net is doing now, the
    /// other what it was set up to do. The console renders this one.
    connections: Vec<NetConnectionWire>,
    started_at: Option<String>,
    closed_at: Option<String>,
    duration_seconds: Option<i64>,
    /// The highest `seq` folded into this summary; the WS client passes it back
    /// as `?since=` to resume.
    pub(crate) latest_seq: u64,
    participant_count: usize,
    /// Present-null when absent, NOT omitted: the console reads it to render the
    /// working cursor.
    working_check_in_id: Option<Uuid>,
    /// Operator-only; the public view never carries it.
    net_note: Option<String>,
    /// `manual`/`worked-sink`. Rides the public view too, which renders neither
    /// affordance and follows the shared ORDER automatically.
    roster_order_mode: &'static str,
    /// `active`/`stalled`. Public radio data (a paused net is visibly frozen), so
    /// it rides BOTH this owner summary and the public view.
    control_state: &'static str,
    /// Owner summary only; an operator id is redacted from the public view.
    active_ncs_account_id: Option<Uuid>,
    /// The VIEWER'S OWN resolved role, computed server-side by the SAME
    /// `resolve_role` the capability gate uses. Client UX only: every mutation
    /// stays server-enforced.
    viewer_role: Role,
    roster: Vec<RosterEntryBody>,
    definition: DefinitionSnapshot,
}

/// One roster station on the PUBLIC wire: the REDACTED projection of
/// [`RosterEntryBody`].
///
/// `addedBy` is omitted: an account-less viewer has no need for it, and public
/// surfaces strip every operator-identifying field. `checkInId` IS kept: it
/// identifies the roster ROW, the frontend keys each rendered row on it, and it
/// already crosses the public wire on every live `checkin.added` delta, so a
/// snapshot without it would collide every initial row onto one key.
///
/// A distinct DTO, never an auth-aware toggle on the owner body: a shared struct
/// with conditional serialization would make the next field's default LEAK. The
/// default answer for a NEW per-check-in field here is NO. Which fields are
/// withheld is stated by `build_public_view`'s destructure, and
/// `roster_projection_sites.rs` reds when that set moves.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct PublicRosterEntry {
    check_in_id: Uuid,
    callsign: String,
    added_at: String,
    /// Public PROVENANCE, not PII: a two-value flag driving the Self badge.
    source: &'static str,
    /// An observer needs to know who is staying for comments to follow the net.
    staying: &'static str,
    /// The public stream already carries the ORDER a precedence sort produces;
    /// this is the label that explains it.
    precedence: &'static str,
    /// Omitted when absent, per the shipped omit-optional rule.
    #[serde(skip_serializing_if = "Option::is_none")]
    traffic: Option<i64>,
    /// The STAFF note is NOT here and must never be: the split exists so an
    /// operator keeps a private channel.
    #[serde(skip_serializing_if = "Option::is_none")]
    public_note: Option<String>,
    /// A deliberate YES: an observer watching a cross-mode net wants to know who
    /// is on the radio and who is on the internet. The connection id is a
    /// snapshot-local identifier this view already publishes in `connections`,
    /// neither an account nor an operator identity; the free-text variant is
    /// operator prose in the same category as the public note.
    #[serde(skip_serializing_if = "Option::is_none")]
    via: Option<ViaWire>,
}

/// The REDACTED public session view: the body of the account-less
/// `GET /api/net-sessions/{id}/live` and the public WS `snapshot` frame. The
/// SAME fold as [`SessionSummaryBody`], projected without the ids a public
/// viewer must never see: `definitionId`/`definitionVersion` are omitted because
/// an unlisted net's id is itself the access capability, and roster entries drop
/// `addedBy`. The `definition` snapshot is KEPT: it is already public net data
/// and the viewer needs the title.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct PublicSessionView {
    id: Uuid,
    lifecycle: &'static str,
    /// Public radio data: how to get on a net is what an account-less viewer
    /// came for. See [`SessionSummaryBody::connections`].
    connections: Vec<NetConnectionWire>,
    started_at: Option<String>,
    closed_at: Option<String>,
    duration_seconds: Option<i64>,
    /// The highest `seq` folded into this view; the public WS client resumes
    /// from it.
    pub(crate) latest_seq: u64,
    participant_count: usize,
    /// Public radio data, unlike the net note. Present-null when absent.
    working_check_in_id: Option<Uuid>,
    /// Public radio data (a paused net is visibly frozen); the operator id
    /// behind it is not.
    control_state: &'static str,
    /// Tells the public page's your-turn selector which reading of "next up" is
    /// in force: under worked-sink it is the top of the unworked group, which
    /// may sit ABOVE the cursor. The roster already arrives in this order, so
    /// nothing private crosses, and the public page gains no affordance from it.
    roster_order_mode: &'static str,
    roster: Vec<PublicRosterEntry>,
    definition: DefinitionSnapshot,
}

/// The stable lifecycle token for the wire: the stored `net_sessions.lifecycle`
/// vocabulary.
fn lifecycle_str(lifecycle: SessionLifecycle) -> &'static str {
    match lifecycle {
        SessionLifecycle::Scheduled => "scheduled",
        SessionLifecycle::Live => "live",
        SessionLifecycle::Closed => "closed",
    }
}

/// Assembles the summary from the frozen snapshot row and the folded run-state.
///
/// The fold is the source of truth for every PROJECTED field; the row's frozen
/// columns are consulted only as a defensive fallback for the fold's `Option`s,
/// which are always `Some` for a started session and equal to the columns by
/// construction.
pub(crate) fn build_summary(
    row: &NetSessionRow,
    folded: &SessionState,
    viewer_role: Role,
) -> SessionSummaryBody {
    let duration_seconds = match (folded.started_at, folded.closed_at) {
        // `close_session` clamps closed_at >= started_at, so this never underflows.
        (Some(started), Some(closed)) => Some((closed.saturating_sub(started) / 1000) as i64),
        _ => None,
    };
    // Resolved ONCE for the whole projection: a correction's `from`/`to` become
    // labels against it, and the body's `connections` field is the same list.
    let connections = folded.live_connections(&row.definition_snapshot.connections);
    let roster: Vec<RosterEntryBody> = folded
        .roster
        .iter()
        .map(|entry| {
            // EXHAUSTIVE destructure, no `..` rest pattern: a new `RosterEntry`
            // field must fail to compile HERE until a human decides whether this
            // projection carries it.
            let RosterEntry {
                check_in_id,
                callsign,
                added_at,
                added_by,
                source,
                signal_report,
                staying,
                name,
                location,
                grid,
                precedence,
                traffic,
                notes,
                public_note,
                worked,
                version,
                via,
                relayed_by,
                corrections,
                // A log cursor is not a fact about the station and reaches no wire.
                added_seq: _,
            } = entry;
            RosterEntryBody {
                check_in_id: *check_in_id,
                callsign: callsign.as_str().to_owned(),
                added_at: rfc3339(*added_at),
                added_by: *added_by,
                source: source.as_str(),
                signal_report: signal_report.as_ref().map(|r| r.as_str().to_owned()),
                staying: staying.as_str(),
                name: name.as_ref().map(|n| n.as_str().to_owned()),
                location: location.as_ref().map(|l| l.as_str().to_owned()),
                grid: grid.as_ref().map(|g| g.as_str().to_owned()),
                precedence: precedence.as_str(),
                traffic: traffic.map(|t| t.get() as i64),
                notes: notes.as_ref().map(|n| n.as_str().to_owned()),
                public_note: public_note.as_ref().map(|n| n.as_str().to_owned()),
                worked: *worked,
                version: *version,
                via: via.as_ref().map(via_of),
                relayed_by: relayed_by.as_ref().map(|c| c.as_str().to_owned()),
                corrections: corrections
                    .iter()
                    .map(|correction| correction_body(correction, &connections))
                    .collect(),
            }
        })
        .collect();
    SessionSummaryBody {
        roster_order_mode: folded.roster_order_mode.as_str(),
        id: row.id,
        definition_id: folded.definition_id.unwrap_or(row.definition_id),
        definition_version: folded.definition_version.unwrap_or(row.definition_version),
        lifecycle: lifecycle_str(folded.lifecycle),
        connections,
        started_at: folded.started_at.map(rfc3339),
        closed_at: folded.closed_at.map(rfc3339),
        duration_seconds,
        latest_seq: folded.last_seq,
        participant_count: roster.len(),
        working_check_in_id: folded.working_check_in_id,
        net_note: folded.net_note.as_ref().map(|n| n.as_str().to_owned()),
        control_state: folded.control_state.as_str(),
        active_ncs_account_id: folded.active_ncs_account_id,
        viewer_role,
        roster,
        definition: row.definition_snapshot.clone(),
    }
}

/// Assembles the REDACTED public view. Same fold source as [`build_summary`],
/// projected WITHOUT `definitionId`/`definitionVersion` and with each roster
/// entry stripped of `addedBy`.
pub(crate) fn build_public_view(row: &NetSessionRow, folded: &SessionState) -> PublicSessionView {
    let duration_seconds = match (folded.started_at, folded.closed_at) {
        (Some(started), Some(closed)) => Some((closed.saturating_sub(started) / 1000) as i64),
        _ => None,
    };
    let roster: Vec<PublicRosterEntry> = folded
        .roster
        .iter()
        .map(|entry| {
            // EXHAUSTIVE destructure, no `..` rest pattern: a new `RosterEntry`
            // field must fail to compile HERE until a human decides whether the
            // PUBLIC projection carries it, and the default answer is NO. A
            // "parity" change here is a privacy regression, not a fix.
            //
            // `added_by` is the redaction this DTO exists for. `relayed_by` names
            // a third-party station that never checked in, so it goes with
            // `added_by`. `grid` is a MORE precise location than the free-text
            // `location` already withheld. `added_seq` is a log cursor. The STAFF
            // `notes` stay private; the note split exists so they can.
            // `roster_projection_sites.rs` reds when a field crosses in either
            // direction; `api_self_check_in.rs` asserts the exact public key set.
            let RosterEntry {
                check_in_id,
                callsign,
                added_at,
                source,
                staying,
                precedence,
                traffic,
                public_note,
                via,
                relayed_by: _,
                added_by: _,
                added_seq: _,
                signal_report: _,
                name: _,
                location: _,
                grid: _,
                notes: _,
                worked: _,
                version: _,
                corrections: _,
            } = entry;
            PublicRosterEntry {
                check_in_id: *check_in_id,
                callsign: callsign.as_str().to_owned(),
                added_at: rfc3339(*added_at),
                source: source.as_str(),
                // Unconditional on the net's visibility: an unlisted net's
                // link-token holder sees these too.
                staying: staying.as_str(),
                precedence: precedence.as_str(),
                traffic: traffic.map(|t| t.get() as i64),
                public_note: public_note.as_ref().map(|n| n.as_str().to_owned()),
                via: via.as_ref().map(via_of),
            }
        })
        .collect();
    PublicSessionView {
        id: row.id,
        lifecycle: lifecycle_str(folded.lifecycle),
        connections: folded.live_connections(&row.definition_snapshot.connections),
        started_at: folded.started_at.map(rfc3339),
        closed_at: folded.closed_at.map(rfc3339),
        duration_seconds,
        latest_seq: folded.last_seq,
        participant_count: roster.len(),
        // The worked-station cursor is public radio data; the net note is not.
        working_check_in_id: folded.working_check_in_id,
        // A paused net is visibly frozen; the operator id behind it is not projected.
        control_state: folded.control_state.as_str(),
        roster_order_mode: folded.roster_order_mode.as_str(),
        roster,
        definition: row.definition_snapshot.clone(),
    }
}

/// Folds an already-loaded session row's log into its summary — the shared
/// core [`session_summary`] and `get_session` (which already holds the row
/// from its ownership check) both use, so a reload never re-fetches the row
/// it just fetched.
pub(crate) async fn session_summary_from_row(
    state: &AppState,
    row: &NetSessionRow,
    viewer_role: Role,
) -> Result<SessionSummaryBody, ApiError> {
    let events = state.session_events.events_since(row.id, 0).await?;
    let folded = replay(&events, 0);
    Ok(build_summary(row, &folded, viewer_role))
}

/// Folds an already-loaded session row's log into the REDACTED public view — the
/// shared core the public GET and public WS snapshot both use.
pub(crate) async fn public_view_from_row(
    state: &AppState,
    row: &NetSessionRow,
) -> Result<PublicSessionView, ApiError> {
    let events = state.session_events.events_since(row.id, 0).await?;
    let folded = replay(&events, 0);
    Ok(build_public_view(row, &folded))
}

/// Loads a session and folds its log into the summary. A missing session → 404.
/// `viewer_role` is the acting account's already-resolved role, threaded from
/// the handler's authorization rather than re-read.
async fn session_summary(
    state: &AppState,
    session_id: Uuid,
    viewer_role: Role,
) -> Result<SessionSummaryBody, ApiError> {
    let row = state
        .net_sessions
        .find(session_id)
        .await?
        .ok_or(ApiError::NetSessionNotFound)?;
    session_summary_from_row(state, &row, viewer_role).await
}

/// Resolves the acting account's effective role: `Owner` if it is in the
/// definition owner set, else the granted role, else the `Participant` floor.
///
/// Pure over its inputs: [`authorize_session`] performs the storage reads, so
/// the DECISION is a function of (owner set, grant, acting id) with no
/// client-trusted role.
pub(crate) fn resolve_role(
    owner_account_ids: &[Uuid],
    granted: Option<Role>,
    acting: Uuid,
) -> Role {
    if can_manage_definition(owner_account_ids, acting) {
        Role::Owner
    } else {
        granted.unwrap_or(Role::Participant)
    }
}

/// Loads a session and enforces the object-level capability `cap`, preserving
/// 404-before-403: 404 for an absent session, decided before authority, then 403
/// when the resolved role lacks `cap`, so a non-owner cannot probe existence.
///
/// The role is resolved server-side from the owner set and the session-scoped
/// grant. A grant names exactly one session, so an account granted a role on a
/// different session resolves to `Participant` here: cross-net isolation by
/// construction.
pub(crate) async fn authorize_session(
    state: &AppState,
    session_id: Uuid,
    current: CurrentAccount,
    cap: Capability,
) -> Result<NetSessionRow, ApiError> {
    let (session, _role) = authorize_session_role(state, session_id, current, cap).await?;
    Ok(session)
}

/// Like [`authorize_session`] but ALSO returns the resolved role, for a handler
/// that makes a further field-level capability decision after the endpoint gate
/// without a second storage read.
pub(crate) async fn authorize_session_role(
    state: &AppState,
    session_id: Uuid,
    current: CurrentAccount,
    cap: Capability,
) -> Result<(NetSessionRow, Role), ApiError> {
    let (session, role, _owner_account_ids) = load_session_role(state, session_id, current).await?;
    if !role_has(role, cap) {
        return Err(ApiError::Forbidden);
    }
    Ok((session, role))
}

/// The shared authorization core: both storage reads that can 404 happen before
/// any role decision. Also returns the definition's owner set, which
/// [`grant_role`] needs to refuse a grant for a current owner.
async fn load_session_role(
    state: &AppState,
    session_id: Uuid,
    current: CurrentAccount,
) -> Result<(NetSessionRow, Role, Vec<Uuid>), ApiError> {
    let session = state
        .net_sessions
        .find(session_id)
        .await?
        .ok_or(ApiError::NetSessionNotFound)?;
    // The FK guarantees the backing definition exists; a vanished one reads as
    // "session not found" rather than leaking a distinct signal.
    let def = state
        .net_definitions
        .find_by_id(session.definition_id)
        .await?
        .ok_or(ApiError::NetSessionNotFound)?;
    let granted = state
        .net_session_roles
        .find_role(session_id, current.account_id)
        .await?;
    let role = resolve_role(&def.owner_account_ids, granted, current.account_id);
    Ok((session, role, def.owner_account_ids))
}

/// [`authorize_session`] with `ViewConsole`, kept so existing callers keep an
/// identical posture. New call sites pass their specific capability directly.
pub(crate) async fn load_owned_session(
    state: &AppState,
    session_id: Uuid,
    current: CurrentAccount,
) -> Result<NetSessionRow, ApiError> {
    authorize_session(state, session_id, current, Capability::ViewConsole).await
}

/// `POST /api/net-sessions` — starts a session. Owner-only over the
/// definition's owner set; an archived definition is refused as absent. The
/// pre-flight archive check is a fast path only: [`NetSessionRepo::start`]'s
/// guarded INSERT re-verifies atomically, composes the row and the first
/// `session.started` in one transaction, and refuses a second concurrently-live
/// session for the same definition through the partial unique index. Returns
/// 201 + the folded summary.
async fn start_session(
    State(state): State<AppState>,
    ConsentedAccount(current): ConsentedAccount,
    AppJson(body): AppJson<StartSessionRequest>,
) -> Result<Response, ApiError> {
    let def = state
        .net_definitions
        .find_by_id(body.definition_id)
        .await?
        .ok_or(ApiError::NetDefinitionNotFound)?;
    // An archived net has left discovery and must not spawn new runs; it is
    // refused with the same 404 as a missing net. Fast path only: the repo
    // re-verifies inside the INSERT's own transaction.
    if def.archived_at_millis.is_some() {
        return Err(ApiError::NetDefinitionNotFound);
    }
    if !can_manage_definition(&def.owner_account_ids, current.account_id) {
        return Err(ApiError::Forbidden);
    }

    // A fresh session is Scheduled by construction, so the start transition is
    // always legal and `session_sm::start` is not consulted; the one-live-per-
    // definition invariant is enforced at the DB layer, which reuses
    // `AlreadyLive`'s 409 slug.
    let now = state.clock.now_epoch_millis();
    let row = match state
        .net_sessions
        .start(&def, Some(current.account_id), now)
        .await?
    {
        StartOutcome::Started(row) => row,
        StartOutcome::DefinitionArchived => return Err(ApiError::NetDefinitionNotFound),
        StartOutcome::AlreadyLive => {
            return Err(ApiError::SessionTransition(
                SessionTransitionError::AlreadyLive,
            ));
        }
    };
    // Ids only — never the definition's geography or the operator email.
    tracing::info!(
        account_id = %current.account_id,
        net_session_id = %row.id,
        net_definition_id = %def.id,
        "net session started"
    );

    // Publish strictly POST-COMMIT and BEFORE the summary fold. The broadcast is
    // notify-only, so a rolled-back start is never published and a client that
    // misses it recovers from Postgres; the fold is a separate, fallible read
    // whose transient error must not `?`-propagate past an already-committed
    // event. Every envelope field is an input already in hand, so nothing here
    // can drift from what was persisted. This `SessionHub::publish` call is the
    // one seam a multi-process deployment swaps for a Valkey PUBLISH.
    let started_event = SessionEvent {
        seq: row.last_seq as u64,
        actor_id: Some(current.account_id),
        at: now,
        body: SessionEventBody::SessionStarted {
            definition_id: def.id,
            definition_version: row.definition_version,
        },
    };
    state.hub.publish(row.id, &started_event);

    // Reached only on the Started arm; a refusal is not an event. The net
    // context lets "everything that touched this net" reach a session without
    // the admin first knowing the session's id.
    super::audit::append_audit(
        &state,
        current.account_id,
        AuditAction::SessionStarted.as_str(),
        super::audit::AuditSubject::session(row.id, def.id),
        None,
        now,
    )
    .await;

    // The starter is always an owner of the definition (checked above).
    let summary = session_summary_from_row(&state, &row, Role::Owner).await?;

    Ok((StatusCode::CREATED, Json(summary)).into_response())
}

/// Voluntary-handoff body: the account id of the qualified target.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct HandoffRequest {
    target_account_id: Uuid,
}

/// Maps a control-machine typed refusal to its HTTP problem:
/// `NotActiveNcs` → 403 (only the active NCS may hand off); `NotStalled` → 409
/// `control-not-stalled`; the lifecycle refusals reuse the shipped
/// `SessionTransition` 409 slugs; `AlreadyStalled` (sweep-internal) → the same.
fn control_error_to_api(err: ControlTransitionError) -> ApiError {
    match err {
        ControlTransitionError::NotActiveNcs => ApiError::Forbidden,
        ControlTransitionError::NotStalled => ApiError::ControlNotStalled,
        ControlTransitionError::NotYetLive => {
            ApiError::SessionTransition(SessionTransitionError::NotYetLive)
        }
        ControlTransitionError::AlreadyClosed | ControlTransitionError::AlreadyStalled => {
            ApiError::SessionTransition(SessionTransitionError::AlreadyClosed)
        }
    }
}

/// Resolves an ARBITRARY account's effective role on a session: the
/// target-qualification check for a voluntary handoff.
async fn resolve_account_role(
    state: &AppState,
    session_id: Uuid,
    owner_account_ids: &[Uuid],
    target: Uuid,
) -> Result<Role, ApiError> {
    let granted = state
        .net_session_roles
        .find_role(session_id, target)
        .await?;
    Ok(resolve_role(owner_account_ids, granted, target))
}

/// `POST /api/net-sessions/{id}/handoff` — VOLUNTARY handoff on a healthy
/// session. The CURRENT active NCS transfers control to a NetControl-tier
/// target; the live stream continues uninterrupted. A non-active-NCS caller →
/// 403; a target below the tier → 422 `handoff-target-unqualified`. Both handoff
/// paths mint the SAME `control.handed-off` event. A malformed body is a 400
/// problem+json via `AppJson`, like every other endpoint.
async fn hand_off_control(
    State(state): State<AppState>,
    ConsentedAccount(current): ConsentedAccount,
    Path(session_id): Path<Uuid>,
    AppJson(body): AppJson<HandoffRequest>,
) -> Result<Response, ApiError> {
    // Running the session is the floor to hand it off — RunSession (Owner/NCS).
    let (session, role) =
        authorize_session_role(&state, session_id, current, Capability::RunSession).await?;

    // Fast-path guard: only the current active NCS may voluntarily hand off a
    // HEALTHY (active, live) session. The guarded write re-verifies atomically.
    let transition = session_sm::hand_off(
        session.lifecycle,
        session.control_state,
        session.active_ncs_account_id,
        current.account_id,
        &*state.clock,
    )
    .map_err(control_error_to_api)?;

    // A caller reaches this fast path only as the current active NCS, so a
    // self-target hands off to no one and would mint an inert
    // `control.handed-off` event. No-op success instead.
    if body.target_account_id == current.account_id {
        let summary = session_summary_from_row(&state, &session, role).await?;
        return Ok((StatusCode::OK, Json(summary)).into_response());
    }

    // The target must hold a qualified NetControl-tier role on this session.
    let def = state
        .net_definitions
        .find_by_id(session.definition_id)
        .await?
        .ok_or(ApiError::NetSessionNotFound)?;
    let target_role = resolve_account_role(
        &state,
        session_id,
        &def.owner_account_ids,
        body.target_account_id,
    )
    .await?;
    if rank(target_role) < rank(Role::NetControl) {
        return Err(ApiError::HandoffTargetUnqualified);
    }

    let event = match state
        .net_sessions
        .hand_off(
            session_id,
            current.account_id,
            body.target_account_id,
            transition.at,
        )
        .await?
    {
        HandoffOutcome::HandedOff(event) => event,
        // Lost the race (a stall or a competing handoff slipped in between the
        // pre-check and this write): the pre-check passed but the guard didn't.
        HandoffOutcome::NotApplicable => return Err(ApiError::ControlNotStalled),
        HandoffOutcome::Missing => return Err(ApiError::NetSessionNotFound),
    };

    // POST-COMMIT publish the control.handed-off delta, then return the summary.
    state.hub.publish(session_id, &event);
    let summary = session_summary_from_row(&state, &session, role).await?;
    Ok((StatusCode::OK, Json(summary)).into_response())
}

/// `POST /api/net-sessions/{id}/claim-control` — INVOLUNTARY claim of a STALLED
/// session. Authorized for a caller holding the
/// `ClaimControl` capability (Owner/NetControl/Logger — the deliberately-lower
/// Logger-floor rescue capability); the claimer becomes the new active NCS and
/// the net returns to `active`. A claim on a NON-stalled session → 409
/// `control-not-stalled`; a Relay/Participant → 403. No body — the claimer IS the
/// target. Both handoff paths mint the SAME `control.handed-off` event.
async fn claim_control(
    State(state): State<AppState>,
    ConsentedAccount(current): ConsentedAccount,
    Path(session_id): Path<Uuid>,
) -> Result<Response, ApiError> {
    // The Logger-floor rescue capability gate (404-before-403).
    let (session, role) =
        authorize_session_role(&state, session_id, current, Capability::ClaimControl).await?;

    // Fast-path guard: a claim is legal ONLY while the session is stalled.
    let transition =
        session_sm::claim_control(session.lifecycle, session.control_state, &*state.clock)
            .map_err(control_error_to_api)?;

    let event = match state
        .net_sessions
        .claim_control(session_id, current.account_id, transition.at)
        .await?
    {
        ClaimControlOutcome::Claimed(event) => event,
        // Lost the race (resumed or claimed by another between pre-check and write).
        ClaimControlOutcome::NotStalled => return Err(ApiError::ControlNotStalled),
        ClaimControlOutcome::Missing => return Err(ApiError::NetSessionNotFound),
    };

    state.hub.publish(session_id, &event);
    let summary = session_summary_from_row(&state, &session, role).await?;
    Ok((StatusCode::OK, Json(summary)).into_response())
}

/// `POST /api/net-sessions/{id}/close` — closes a live session. Requires
/// `RunSession`. `session_sm::close` is a fast-path pre-check mapping to a 409
/// per variant; the authority is the repo's guarded UPDATE, which re-verifies
/// `lifecycle = 'live'` in the same transaction as the `session.closed` append,
/// so of two concurrent closes only one wins and the loser's
/// `CloseOutcome::NotLive` maps to the same `AlreadyClosed` 409 with no phantom
/// event. Returns 200 + the folded summary.
async fn close_session(
    State(state): State<AppState>,
    ConsentedAccount(current): ConsentedAccount,
    Path(session_id): Path<Uuid>,
) -> Result<Response, ApiError> {
    let (session, viewer_role) =
        authorize_session_role(&state, session_id, current, Capability::RunSession).await?;

    // Fast-path legality pre-check against an unlocked read; the guarded write
    // in `NetSessionRepo::close` below is the authority.
    let transition = session_sm::close(session.lifecycle, &*state.clock)?;

    // `SystemClock` is wall-clock time, not monotonic: an NTP step between the
    // start and close requests can run it backward. Clamp rather than refuse,
    // so the folded `closed_at >= started_at` invariant holds and an owner is
    // never stranded unable to end their own net over a clock blip.
    let closed_at = match session.started_at_millis {
        Some(started) => transition.at.max(started),
        None => transition.at,
    };

    let closed_event = match state
        .net_sessions
        .close(session_id, closed_at, Some(current.account_id))
        .await?
    {
        CloseOutcome::Closed(event) => event,
        // Lost the check-then-act race against a concurrent close (or a
        // second request racing this one): same 409 a sequential double-close
        // already gets, but no event was appended for this request.
        CloseOutcome::NotLive => {
            return Err(ApiError::SessionTransition(
                SessionTransitionError::AlreadyClosed,
            ));
        }
        // Vanished between the ownership load above and this write — treat
        // exactly like any other raced-to-deleted lookup.
        CloseOutcome::Missing => return Err(ApiError::NetSessionNotFound),
    };
    tracing::info!(
        account_id = %current.account_id,
        net_session_id = %session_id,
        net_definition_id = %session.definition_id,
        "net session closed"
    );
    // OPERATOR closes only. The presence-monitor's
    // abandoned-session auto-close is actorless (`NetSessionRepo::close` takes
    // `actor_id: Option<Uuid>` and the sweep passes `None`) and stays
    // tracing-only, because `audit_log.actor_account_id` is NOT NULL. The admin
    // UI surfaces that gap explicitly rather than letting a missing close read
    // as "still open".
    super::audit::append_audit(
        &state,
        current.account_id,
        AuditAction::SessionClosed.as_str(),
        super::audit::AuditSubject::session(session_id, session.definition_id),
        None,
        // The clamped close instant, so the audit row and the persisted
        // `session.closed` event agree on when it happened.
        closed_at,
    )
    .await;

    // POST-COMMIT, notify-only, and BEFORE the summary fold for the reason
    // `start_session` states. `closed_event` is the event `close()` actually
    // appended, so it cannot drift from the persisted row.
    state.hub.publish(session_id, &closed_event);

    // Fire-and-forget on-close delivery, reached ONLY on the Closed arm. The
    // JoinHandle is DROPPED so a slow SMTP/webhook can never stall this
    // response; delivery-off nets are a no-op inside the deliverer.
    state
        .delivery_service()
        .spawn_for_closed_session(session_id);

    let summary = session_summary(&state, session_id, viewer_role).await?;

    Ok((StatusCode::OK, Json(summary)).into_response())
}

/// `POST /api/net-sessions/{id}/frequency` — moves one connection of a live
/// session. Requires `RunSession`. `ensure_writable` is a fast-path pre-check
/// mapping to the precise 409 slug; the authority is the repo's
/// `AND lifecycle = 'live'` guarded UPDATE in the same transaction as the
/// `frequency.changed` append, so a session closing between the two still
/// refuses the change with no phantom event. Returns 200 + the folded summary.
async fn change_frequency(
    State(state): State<AppState>,
    ConsentedAccount(current): ConsentedAccount,
    Path(session_id): Path<Uuid>,
    AppJson(body): AppJson<ChangeFrequencyRequest>,
) -> Result<Response, ApiError> {
    let (session, viewer_role) =
        authorize_session_role(&state, session_id, current, Capability::RunSession).await?;

    // Fast-path pre-check; the guarded write below is the authority.
    session_sm::ensure_writable(session.lifecycle, session.control_state)?;

    let operating_frequency_hz = parse_frequency_hz(&body.operating_frequency)
        .map_err(|e| ApiError::validation(operating_frequency_message(e)))?;

    let now = state.clock.now_epoch_millis();
    let changed_event = match state
        .net_sessions
        .change_frequency(
            session_id,
            body.connection_id,
            operating_frequency_hz,
            Some(current.account_id),
            now,
        )
        .await?
    {
        ChangeFrequencyOutcome::Changed(event) => event,
        // Not a connection this session froze at start; appending anyway would
        // write a frequency no surface renders.
        ChangeFrequencyOutcome::ConnectionUnknown => {
            return Err(ApiError::NetConnectionNotFound);
        }
        // The connection IS on the session and is reached by NAME — an
        // EchoLink node, a DMR talkgroup. 404 would be false (it exists) and
        // 409 would be false (nothing conflicts): the request is well formed
        // and the operation does not apply, which is a 422.
        ChangeFrequencyOutcome::ConnectionCarriesNoFrequency => {
            return Err(ApiError::ConnectionCarriesNoFrequency);
        }
        // Lost the check-then-act race against a concurrent close: the same 409
        // a sequential change on a closed session already gets, but no event
        // was appended for this request.
        ChangeFrequencyOutcome::NotLive => {
            return Err(ApiError::SessionTransition(
                SessionTransitionError::AlreadyClosed,
            ));
        }
        // Vanished between the ownership load above and this write — treat
        // exactly like any other raced-to-deleted lookup.
        ChangeFrequencyOutcome::Missing => return Err(ApiError::NetSessionNotFound),
    };
    // Ids only — never the definition's geography or the operator email.
    tracing::info!(
        account_id = %current.account_id,
        net_session_id = %session_id,
        net_definition_id = %session.definition_id,
        "net session frequency changed"
    );

    // POST-COMMIT, notify-only, BEFORE the summary fold; see `start_session`.
    state.hub.publish(session_id, &changed_event);

    let summary = session_summary(&state, session_id, viewer_role).await?;

    Ok((StatusCode::OK, Json(summary)).into_response())
}

/// `POST /api/net-sessions/{id}/check-ins` — adds a check-in to a live session.
/// Gated on `SelfCheckIn`, the floor every authenticated role holds, so the load
/// still 404s before 403; the staff-vs-self branch is decided by whether the
/// caller also holds `LogCheckIn`. `ensure_writable` is the fast-path pre-check;
/// the repo's guarded write is the authority, in the same transaction as the
/// `checkin.added` append. Returns 201 + the folded summary.
async fn add_check_in(
    State(state): State<AppState>,
    ConsentedAccount(current): ConsentedAccount,
    Path(session_id): Path<Uuid>,
    AppJson(body): AppJson<AddCheckInRequest>,
) -> Result<Response, ApiError> {
    let (session, viewer_role) =
        authorize_session_role(&state, session_id, current, Capability::SelfCheckIn).await?;

    // Fast-path pre-check; the guarded write below is the authority.
    session_sm::ensure_writable(session.lifecycle, session.control_state)?;

    // `via` is validated against this session's FROZEN snapshot before either
    // arm runs: the answer is identical on both, and a bad id must 404 with no
    // event appended regardless of who is writing.
    let via = parse_via(body.via.as_ref(), &session)?;
    // A `relayedBy` from a caller without `LogCheckIn` is a 403 on mere PRESENCE:
    // a participant self-checking in reached the net through the app, so there
    // was no relaying station. It sits before the PARSE so the refusal never
    // reveals which relaying callsigns would have parsed, and before the LIMITER
    // so a claim this path can never accept costs no quota.
    if body.relayed_by.is_some() && !role_has(viewer_role, Capability::LogCheckIn) {
        return Err(ApiError::Forbidden);
    }
    // Must say `relayed-by`, never `callsign`: the request's own callsign was fine.
    let relayed_by = parse_edit_relayed_by(body.relayed_by.as_deref())?;

    let (callsign, signal_report, staying, name, location, grid, source) =
        if role_has(viewer_role, Capability::LogCheckIn) {
            // Staff path: any callsign, staff fields gated on EditStaffFields.
            let callsign = parse_callsign(&body.callsign)
                .map_err(|e| ApiError::CallsignInvalid(e.to_string()))?;

            // A Relay holds `LogCheckIn` but NOT `EditStaffFields`, so a Relay
            // sending a report is a 403 with NO append. Gate on PRESENCE before
            // parsing; a blank report is `None`.
            let signal_report = match &body.signal_report {
                Some(raw) => {
                    if !role_has(viewer_role, Capability::EditStaffFields) {
                        return Err(ApiError::Forbidden);
                    }
                    parse_signal_report(raw).map_err(|e| {
                        ApiError::SignalReportInvalid(profile_field_message("signal report", e))
                    })?
                }
                None => None,
            };
            let staying = parse_staying(body.staying.as_deref())?;
            let name = parse_edit_name(body.name.as_deref())?;
            let location = parse_edit_location(body.location.as_deref())?;
            let grid = parse_edit_grid(body.grid.as_deref())?;
            (
                callsign,
                signal_report,
                staying,
                name,
                location,
                grid,
                CheckInSource::Staff,
            )
        } else {
            // Self path: a rank-0 Participant checking THEMSELVES in. The staff
            // path stays ungoverned (operators legitimately burst-log); here the
            // limiter keys on the account id, because IP-keying would collapse
            // NAT'd participants.
            //
            // Preconditions that can never succeed are checked BEFORE the limiter
            // is charged, so a broken profile cannot self-inflict a 429 on the
            // first attempt after it is fixed. Every request that COULD touch an
            // entry, including the idempotent re-add below, is still charged.
            let account = state
                .accounts
                .find_by_id(current.account_id)
                .await?
                .ok_or(ApiError::Unauthenticated)?;

            // The server re-checks regardless of the client gate.
            if account.email_verified_at_millis.is_none() {
                return Err(ApiError::EmailUnverified);
            }
            // The frontend routes this 4xx to callsign setup. The body's
            // `callsign` is IGNORED, so no arbitrary-callsign injection is possible.
            let callsign_str = account.callsign.ok_or(ApiError::CallsignRequired)?;
            let callsign = parse_callsign(&callsign_str)
                .map_err(|e| ApiError::CallsignInvalid(e.to_string()))?;

            // One fold serves both the blocklist check here and the idempotency
            // check below, read BEFORE the limiter is charged so a blocked account
            // never spends a burst cell.
            let events = state.session_events.events_since(session_id, 0).await?;
            let folded = replay(&events, 0);

            // Keyed on the ACCOUNT id, never the callsign string, so re-attempting
            // under another callsign is still refused: 403, NO event, NO limiter
            // charge. Fast path only: this fold is not atomic with the write, so
            // `add_check_in` re-checks the blocklist inside its own transaction,
            // which is what closes the race against a concurrent block.
            if folded.blocked_account_ids.contains(&current.account_id) {
                return Err(ApiError::AccountBlocked);
            }

            if let Err(retry_after_secs) = state
                .self_check_in_limiter
                .check(&current.account_id.to_string())
            {
                return Err(ApiError::RateLimited { retry_after_secs });
            }

            // Mere PRESENCE of a signal report is a 403 with no append;
            // precedence/traffic are not add-time fields at all. `relayedBy` was
            // refused above, before the limiter, and that refusal is what makes
            // the check-in history and the personal-data export carry no relaying
            // station BY CONSTRUCTION: a `source = 'self'` row cannot hold one.
            if body.signal_report.is_some() {
                return Err(ApiError::Forbidden);
            }

            // A participant may name a LISTED way in, a choice among ids the owner
            // published, but may NOT write free text: that is participant-supplied
            // PROSE rendered unfiltered on the unauthenticated public roster, the
            // same class as `publicNote`, which is staff-only. Net control still
            // records an unlisted connection mid-net, on the staff path.
            if matches!(body.via, Some(ViaWire::Unlisted { .. })) {
                return Err(ApiError::Forbidden);
            }

            // Derived from the account profile, never the body. `Account.grid` is
            // stored as a plain `Option<String>`, so it is RE-parsed through the
            // same guard, exactly as `location` is.
            let name = parse_edit_name(account.display_name.as_deref())?;
            let location = parse_edit_location(account.location.as_deref())?;
            let grid = parse_edit_grid(account.grid.as_deref())?;
            // `staying` IS the participant's own toggle.
            let staying = parse_staying(body.staying.as_deref())?;

            // A second self-add for an account that already holds a self-entry
            // returns the current summary: no duplicate row, no error.
            let already_checked_in = folded
                .roster
                .iter()
                .any(|e| owns_check_in(e.source, e.added_by, current.account_id));
            if already_checked_in {
                let summary = build_summary(&session, &folded, viewer_role);
                return Ok((StatusCode::OK, Json(summary)).into_response());
            }

            (
                callsign,
                None,
                staying,
                name,
                location,
                grid,
                CheckInSource::SelfService,
            )
        };

    let now = state.clock.now_epoch_millis();
    let check_in_id = Uuid::now_v7();
    // `None` for the basic form, `Some(id)` for the quick-add, so the frontend
    // store's `reconcilePending` has an id to match the authoritative echo on.
    let added_event = match state
        .net_sessions
        .add_check_in(
            session_id,
            &callsign,
            check_in_id,
            body.client_event_id,
            signal_report.as_ref(),
            staying,
            name.as_ref(),
            location.as_ref(),
            grid.as_ref(),
            source,
            via.as_ref(),
            relayed_by.as_ref(),
            Some(current.account_id),
            now,
        )
        .await?
    {
        AddCheckInOutcome::Added(applied) => applied,
        // The SAME (session, clientEventId) hit the partial unique index: a
        // double-click, or a retry after a slow response that committed. Return
        // 200 + the folded summary and do NOT double-publish, or the frontend
        // renders a phantom second row.
        AddCheckInOutcome::Duplicate => {
            let summary = session_summary(&state, session_id, viewer_role).await?;
            return Ok((StatusCode::OK, Json(summary)).into_response());
        }
        // Lost the check-then-act race against a concurrent close: the same 409
        // a sequential add on a closed session already gets, but no event was
        // appended for this request.
        AddCheckInOutcome::NotLive => {
            return Err(ApiError::SessionTransition(
                SessionTransitionError::AlreadyClosed,
            ));
        }
        // Vanished between the ownership load above and this write — treat
        // exactly like any other raced-to-deleted lookup.
        AddCheckInOutcome::Missing => return Err(ApiError::NetSessionNotFound),
        // The handler's out-of-transaction pre-check above passed, but the
        // adapter's re-fold INSIDE the guarded transaction caught a block that
        // committed in the gap. Same 403 the pre-check returns; no event appended.
        AddCheckInOutcome::AccountBlocked => return Err(ApiError::AccountBlocked),
    };
    // Ids only; the callsign is public radio data but parity with the other
    // handlers keeps it out of the log line.
    tracing::info!(
        account_id = %current.account_id,
        net_session_id = %session_id,
        net_definition_id = %session.definition_id,
        "net session check-in added"
    );

    // POST-COMMIT, notify-only, BEFORE the summary fold; see `start_session`.
    // `SessionHub` fans out to owner and public streams alike; the public
    // serializer redacts.
    state.hub.publish(session_id, &added_event.added);
    // Under worked-sink the repo re-sank the new arrival in the SAME
    // transaction; publish that permutation next, in seq order.
    if let Some(reordered) = &added_event.reordered {
        state.hub.publish(session_id, reordered);
    }

    let summary = session_summary(&state, session_id, viewer_role).await?;

    Ok((StatusCode::CREATED, Json(summary)).into_response())
}

/// Maps a fold-derived [`Correction`] to its wire body.
///
/// `connections` is the session's ways in, which the `via` correction resolves
/// its LABEL against. The fold cannot do it — it has never
/// seen a connection set — so this is the one place a `via` becomes readable,
/// and the sum type it arrives in is what stops a UUID reaching the wire by
/// accident.
fn correction_body(correction: &Correction, connections: &[NetConnectionWire]) -> CorrectionBody {
    CorrectionBody {
        field: correction.field.as_str(),
        from: correction_side(correction.from.as_ref(), connections),
        to: correction_side(correction.to.as_ref(), connections),
        at: rfc3339(correction.at),
    }
}

/// One side of a correction as the console renders it.
///
/// `via_label` answers `None` only for `ViaDisplay::NotRecorded`, which
/// `resolve_via` never returns for a `Some(via)` — so a `via` correction that
/// EXISTS always renders words, and the unresolvable case renders the phrase
/// that names the fault rather than a blank or a UUID.
///
/// Resolved against the LIVE set, deliberately: this is the live console's
/// surface, not a historical read.
fn correction_side(
    value: Option<&CorrectionValue>,
    connections: &[NetConnectionWire],
) -> Option<String> {
    match value? {
        CorrectionValue::Text(text) => Some(text.clone()),
        CorrectionValue::Via(via) => via_label(&resolve_via(Some(via), connections)),
    }
}

/// Declares the check-in edit request's editable field set ONCE and derives the
/// three things that must never drift apart: the request struct, the wire-name
/// list [`EDITABLE_CHECK_IN_FIELDS`], and the partial-body predicate that
/// decides whether the stored entry gets folded.
///
/// These used to be three hand-maintained lists, and a field added to the struct
/// but missed from the predicate silently reopened the field-wipe defect with no
/// compile error and no failing test. The wire name is written beside each field
/// because `macro_rules!` cannot case-convert an identifier.
macro_rules! editable_check_in_request {
    (
        $(#[$struct_meta:meta])*
        struct $name:ident {
            $(
                $(#[$field_meta:meta])*
                $wire:literal => $field:ident : $ty:ty
            ),+ $(,)?
        }
    ) => {
        $(#[$struct_meta])*
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct $name {
            callsign: String,
            $(
                $(#[$field_meta])*
                #[serde(default)]
                $field: Undefinable<$ty>,
            )+
            expected_version: u64,
        }

        /// Every editable check-in field's WIRE name, generated from the single
        /// declaration in [`editable_check_in_request!`].
        ///
        /// `api_check_in_editing.rs` asserts its survival-test seed covers
        /// EXACTLY these names, so a field added to the declaration without
        /// test coverage fails the suite rather than passing silently.
        pub const EDITABLE_CHECK_IN_FIELDS: &[&str] = &[$($wire),+];

        impl $name {
            /// Whether ANY editable field was OMITTED from the request body.
            ///
            /// Generated from the same declaration as the struct, so it can
            /// never fall behind it. A partial body needs the stored entry to
            /// resolve its absent fields against; a body carrying every field
            /// does not, which is what keeps the staff hot path off the fold.
            fn is_partial(&self) -> bool {
                $(self.$field.is_missing())||+
            }
        }
    };
}

editable_check_in_request! {
    /// Edit-a-check-in body: the post-edit editable field set plus the
    /// optimistic-concurrency `expectedVersion`.
    ///
    /// Every editable field is [`Undefinable`], not `Option`. The distinction is
    /// the whole fix for the field-wipe defect:
    ///
    /// - **absent** → keep whatever is stored. A UI that edits one field sends one
    /// field, and must not disturb the rest.
    /// - **`null` or blank** → clear it. Staff keep the ability to erase a field
    /// on purpose.
    /// - **valued** → replace it.
    ///
    /// A bare `Option` cannot tell the first case from the second, and that
    /// ambiguity let the public page's three-field staying toggle erase six fields
    /// and reset a seventh.
    struct EditCheckInRequest {
        "name" => name: String,
        "location" => location: String,
        /// `null`/blank clears it (PUT-replace), exactly like `name`/`location`.
        "grid" => grid: String,
        "signalReport" => signal_report: String,
        "staying" => staying: String,
        /// An explicit `null` resets to `routine`.
        "precedence" => precedence: String,
        /// `null`/0 → none.
        "traffic" => traffic: i64,
        /// The STAFF note.
        "notes" => notes: String,
        /// The PUBLIC note; same gate as the staff note. The participant self path
        /// refuses it outright.
        "publicNote" => public_note: String,
        /// The participant self path forces it to the stored value rather than
        /// refusing it: it is not a staff-only field, it is one the participant
        /// may not CHANGE.
        "via" => via: ViaWire,
        /// Same self-path treatment as `via`.
        "relayedBy" => relayed_by: String,
    }
}

/// Remove-a-check-in body: only the `expectedVersion` CAS; the entry id is in
/// the path.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RemoveCheckInRequest {
    expected_version: u64,
}

/// Moderate-a-check-in body: `block` requests an account block
/// alongside the removal; `expectedVersion` is the CAS. The entry id is in the
/// path. The block target's account id is resolved SERVER-SIDE from the folded
/// roster — never client-supplied.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ModerateCheckInRequest {
    block: bool,
    expected_version: u64,
}

/// The soft-lock lease body returned on acquire/renew.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct LockBody {
    holder_callsign: String,
    expires_at: String,
}

/// Parses an optional `staying` wire token: an omitted/`None` field defaults to
/// `in-and-out`; an unknown token is a 400 carrying the domain error's
/// bounded, control/bidi-sanitized `Display` echo. Shared by the check-in add
/// (staff + self) and edit paths so the default/echo posture is identical.
fn parse_staying(submitted: Option<&str>) -> Result<StayingStatus, ApiError> {
    match submitted {
        Some(token) => {
            StayingStatus::try_from(token).map_err(|e| ApiError::StayingInvalid(e.to_string()))
        }
        None => Ok(StayingStatus::default()),
    }
}

/// Whole-request validation copy for a bad operating frequency.
///
/// `parse_frequency_hz` classifies five distinct faults and both call sites used
/// to discard all five with `.map_err(|_| …)`, answering by reciting the rule
/// rather than naming what was wrong. The frequency field has no
/// per-input error slot on the live-session surface, so this is a sentence.
fn operating_frequency_message(error: FrequencyError) -> &'static str {
    match error {
        FrequencyError::Empty => {
            "The operating frequency is missing — enter it in MHz and try again."
        }
        FrequencyError::NotNumeric => {
            "The operating frequency needs to be a number of MHz, like 14.230 — correct it and try again."
        }
        FrequencyError::Negative => {
            "The operating frequency cannot be negative — correct it and try again."
        }
        FrequencyError::OutOfRange => {
            "The operating frequency is outside the amateur bands — correct it and try again."
        }
        FrequencyError::TooPrecise => {
            "The operating frequency is finer than one hertz — round it and try again."
        }
    }
}

/// Parses an optional PUT-replace name field: `None`/blank-after-trim clears it,
/// anything else must pass [`parse_name`]. Mirrors the profile
/// `parse_profile_field` idiom.
fn parse_edit_name(submitted: Option<&str>) -> Result<Option<Name>, ApiError> {
    match submitted {
        None => Ok(None),
        Some(s) if s.trim().is_empty() => Ok(None),
        Some(s) => {
            parse_name(s).map_err(|e| ApiError::validation(profile_field_message("name", e)))
        }
    }
}

/// Parses an optional PUT-replace location field.
fn parse_edit_location(submitted: Option<&str>) -> Result<Option<Location>, ApiError> {
    match submitted {
        None => Ok(None),
        Some(s) if s.trim().is_empty() => Ok(None),
        Some(s) => parse_location(s)
            .map_err(|e| ApiError::validation(profile_field_message("location", e))),
    }
}

/// Parses an optional PUT-replace grid field: `None`/blank-after-trim clears it,
/// anything else must pass the SAME [`netroll_domain::profile::parse_grid`]
/// grammar the profile and net-definition paths use — there is exactly one
/// Maidenhead parser. The blank arm is
/// load-bearing and not symmetric with `parse_edit_location`: `parse_grid("")`
/// returns `Err(GridError::Empty)` where `parse_location("")` returns `Ok`, so
/// without it the modal's deliberate `""`-to-clear would 400 instead of clearing.
fn parse_edit_grid(submitted: Option<&str>) -> Result<Option<Grid>, ApiError> {
    match submitted {
        None => Ok(None),
        Some(s) if s.trim().is_empty() => Ok(None),
        Some(s) => parse_grid(s)
            .map(Some)
            .map_err(|e| ApiError::GridInvalid(e.to_string())),
    }
}

/// Parses an optional PUT-replace note field: `None`/blank-after-
/// trim clears it, anything else must pass [`parse_note`]. An over-bound note is a
/// 400 `/errors/note-invalid`.
///
/// The rejection goes through [`profile_field_message`] rather than
/// `ProfileError`'s `Display`. That `Display` is a FRAGMENT — "must be 2000
/// characters or fewer" — with no subject; rendered straight into `detail` it
/// named no field and matched neither register `problem.rs` codifies. This path
/// renders in the same section-level alert as name and location, so it takes the
/// same whole-request sentence they do.
fn parse_edit_note(submitted: Option<&str>) -> Result<Option<Note>, ApiError> {
    match submitted {
        None => Ok(None),
        Some(s) if s.trim().is_empty() => Ok(None),
        Some(s) => {
            parse_note(s).map_err(|e| ApiError::NoteInvalid(profile_field_message("note", e)))
        }
    }
}

/// Parses an optional PUT-replace `relayedBy` field:
/// `None`/blank-after-trim clears it, anything else must pass the shipped
/// [`parse_callsign`] — the SAME guard the check-in's own callsign uses, so a
/// relaying station is recorded and normalized the way every other station in
/// this system is.
///
/// The rejection is [`ApiError::RelayedByInvalid`] and NOT
/// [`ApiError::CallsignInvalid`]: reusing the latter compiles and validates
/// correctly, then tells the operator their CALLSIGN is wrong on a request whose
/// callsign was fine. An error names the faulting field.
fn parse_edit_relayed_by(submitted: Option<&str>) -> Result<Option<Callsign>, ApiError> {
    match submitted {
        None => Ok(None),
        Some(s) if s.trim().is_empty() => Ok(None),
        Some(s) => Ok(Some(
            parse_callsign(s).map_err(|e| ApiError::RelayedByInvalid(e.to_string()))?,
        )),
    }
}

/// Resolves a submitted `via` against the session's FROZEN snapshot.
///
/// A `connection` id the snapshot does not hold is REFUSED with the same
/// `/errors/net-connection-not-found` 404 `change_frequency` uses, and no event
/// is appended. Resolving against the SNAPSHOT and not the definition's current
/// connections is deliberate: the session replays from its own copy, and a
/// connection the owner has since deleted is still a legitimate `via` for a
/// check-in this session logged.
///
/// This makes `ViaDisplay::Unresolvable` RARE, not unreachable — a row written
/// by a newer deploy during a rollback, or a hand-edited payload, still reaches
/// it — so nothing downstream may stub that case out.
///
/// The free-text arm goes through the domain's own
/// [`netroll_domain::net::connection::parse_via_text`] — a SINGLE-LINE guard
/// bounded at [`netroll_domain::net::connection::MAX_VIA_CHARS`], not
/// `parse_note`'s 2000 multi-line characters: a way in is a one-line chip beside
/// a callsign on the unauthenticated public roster, and the note guard would
/// have let 2000 characters of multi-line prose onto that page.
/// Blank-after-trim is REFUSED rather than folded to `None` — a body that says
/// `{"kind":"unlisted"}` has asserted a way in exists, and answering 200 with
/// "nobody recorded one" rewrites what the operator said.
fn parse_via(
    submitted: Option<&ViaWire>,
    session: &NetSessionRow,
) -> Result<Option<Via>, ApiError> {
    match submitted {
        None => Ok(None),
        Some(ViaWire::Connection { connection_id }) => {
            if session
                .definition_snapshot
                .connections
                .iter()
                .any(|c| c.id == *connection_id)
            {
                Ok(Some(Via::Connection(*connection_id)))
            } else {
                Err(ApiError::NetConnectionNotFound)
            }
        }
        Some(ViaWire::Unlisted { text }) => parse_via_text(text)
            .map(|text| Some(Via::Unlisted(text)))
            .map_err(|e| ApiError::ViaInvalid(via_field_message(e))),
    }
}

/// Renders a [`ViaError`] as a whole-request sentence that names the WAY IN.
///
/// The bounded-text half reuses [`profile_field_message`] with `"way in"` as the
/// subject, so the length and illegal-character sentences read exactly like
/// name's and location's; the blank half has its own words, because "must be 64
/// characters or fewer" says nothing about a value that is empty.
fn via_field_message(error: ViaError) -> String {
    match error {
        ViaError::Blank => {
            "The way in has no words in it — say which way in, or leave it unrecorded.".to_owned()
        }
        ViaError::Text(e) => profile_field_message("way in", e),
    }
}

/// Parses an optional PUT-replace signal report: `None` clears it, anything
/// else must pass [`parse_signal_report`] (which itself maps a blank to `None`).
/// Exists so the report resolves through the same `fn(Option<&str>)` shape as
/// name/location/grid/notes and can share [`resolve_replace`].
fn parse_edit_signal_report(submitted: Option<&str>) -> Result<Option<SignalReport>, ApiError> {
    match submitted {
        None => Ok(None),
        Some(raw) => parse_signal_report(raw)
            .map_err(|e| ApiError::SignalReportInvalid(profile_field_message("signal report", e))),
    }
}

/// Resolves one PUT-replace field against what is currently stored.
///
/// An ABSENT key keeps `stored`; an explicit `null` or a blank value clears;
/// any other value is parsed. Reading absent as "clear" was the field-wipe
/// defect — a partial body from the public participant surface erased every
/// field it did not mention. `stored` is `None` only when the entry could not
/// be folded, in which case the CAS in the repository refuses the edit anyway.
fn resolve_replace<T: Clone>(
    submitted: &Undefinable<String>,
    stored: Option<&T>,
    parse: fn(Option<&str>) -> Result<Option<T>, ApiError>,
) -> Result<Option<T>, ApiError> {
    match submitted {
        Undefinable::Missing => Ok(stored.cloned()),
        Undefinable::Null => parse(None),
        Undefinable::Value(value) => parse(Some(value)),
    }
}

// The editable slice of the stored roster entry a PARTIAL staff edit resolves an
// omitted field against, so the staff arm takes its stored values through ONE
// exhaustive `RosterEntry` destructure instead of independent
// `stored.and_then(|e| e.field)` reads that a new field could never break.
#[derive(Clone, Copy)]
struct StoredEditable<'a> {
    name: Option<&'a Name>,
    location: Option<&'a Location>,
    grid: Option<&'a Grid>,
    signal_report: Option<&'a SignalReport>,
    staying: StayingStatus,
    precedence: Precedence,
    traffic: Option<TrafficCount>,
    notes: Option<&'a Note>,
    public_note: Option<&'a Note>,
    /// Joins `CheckinUpdated`, so a partial body that omits it must resolve to
    /// the STORED value or the edit writes it away.
    via: Option<&'a Via>,
    /// Same reason as `via`.
    relayed_by: Option<&'a Callsign>,
}

/// `PUT /api/net-sessions/{id}/check-ins/{check_in_id}` — edits an
/// already-logged check-in. The order preserves 404-before-403:
/// 1. `EditCheckIn` (Logger+) via [`authorize_session`] — 404 absent, 403 non-
///    Logger, 401 unauth.
/// 2. `ensure_mutable` — a closed session's roster is frozen (409).
/// 3. Soft-lock check: a DIFFERENT account holding a valid lease → 409
///    `lock-held` (the NORMAL collision, caught proactively). Caller-holds or
///    no-valid-lease → proceed.
/// 4. Field validation at the boundary (400 on invalid callsign/report/staying).
/// 5. Version CAS inside the seq-serialized append txn → 409 `stale-version` on
///    mismatch, no append (the RARE lock-expiry race — the CAS is the real
///    correctness authority, the lock is advisory).
///
/// On success: POST-COMMIT `hub.publish` the `checkin.updated` delta, then
/// return the folded summary.
async fn edit_check_in(
    State(state): State<AppState>,
    ConsentedAccount(current): ConsentedAccount,
    Path((session_id, check_in_id)): Path<(Uuid, Uuid)>,
    AppJson(body): AppJson<EditCheckInRequest>,
) -> Result<Response, ApiError> {
    // Generated from the same declaration as the struct, so it cannot fall
    // behind it. Read BEFORE the destructure moves the body.
    let body_is_partial = body.is_partial();

    // EXHAUSTIVE on purpose: adding a field to `EditCheckInRequest` breaks the
    // build right here and forces the author to resolve it against `stored`
    // below, instead of letting a new field quietly rejoin the wipe set. Do not
    // "fix" a future compile error by adding `..`.
    let EditCheckInRequest {
        callsign: submitted_callsign,
        name: submitted_name,
        location: submitted_location,
        grid: submitted_grid,
        signal_report: submitted_report,
        staying: submitted_staying,
        precedence: submitted_precedence,
        traffic: submitted_traffic,
        notes: submitted_notes,
        public_note: submitted_public_note,
        via: submitted_via,
        relayed_by: submitted_relayed_by,
        expected_version,
    } = body;

    // Gate on the rank-0 floor so the session load still 404s-before-403; the
    // staff-vs-self BRANCH is decided below by whether the caller holds `EditCheckIn`.
    let (session, viewer_role) =
        authorize_session_role(&state, session_id, current, Capability::SelfCheckIn).await?;
    session_sm::ensure_writable(session.lifecycle, session.control_state)?;

    let now = state.clock.now_epoch_millis();
    // A DIFFERENT account's valid lease blocks the NORMAL collision proactively;
    // the CAS below is the real authority.
    if let Some((holder_id, holder_callsign)) =
        state.session_locks.holder(session_id, check_in_id, now)
        && holder_id != current.account_id
    {
        return Err(ApiError::LockHeld(holder_callsign));
    }

    let is_staff = role_has(viewer_role, Capability::EditCheckIn);
    // Resolving an ABSENT field to "keep what is stored" needs the current
    // entry, so a PARTIAL body pays for a fold. The detail modal sends every
    // editable field, so the staff hot path folds only when it has to.
    let stored = if !is_staff || body_is_partial {
        let events = state.session_events.events_since(session_id, 0).await?;
        replay(&events, 0)
            .roster
            .into_iter()
            .find(|e| e.check_in_id == check_in_id)
    } else {
        None
    };

    // Compute the post-edit editable field set per path.
    #[allow(clippy::type_complexity)]
    let (
        callsign,
        name,
        location,
        grid,
        signal_report,
        staying,
        precedence,
        traffic,
        notes,
        public_note,
        via,
        relayed_by,
    ) = if is_staff {
        // Staff path. `EditCheckIn` is the Logger floor, which also covers the
        // report's `EditStaffFields` gate, so no second capability check. The
        // destructure is EXHAUSTIVE because `checkin.updated` carries the FULL
        // post-edit field set: a stored field this arm fails to read is WRITTEN
        // AWAY by a body that omits it. A field joins the read half only if it
        // also joins `CheckinUpdated`; `callsign` is REQUIRED on the staff body,
        // and the other `_`-bound fields are envelope or fold-owned, which an
        // edit cannot change. `roster_projection_sites.rs` reds when the dropped
        // set moves.
        let stored_editable = stored.as_ref().map(|entry| {
            let RosterEntry {
                name,
                location,
                grid,
                signal_report,
                staying,
                precedence,
                traffic,
                notes,
                public_note,
                via,
                relayed_by,
                callsign: _,
                check_in_id: _,
                added_at: _,
                added_by: _,
                added_seq: _,
                source: _,
                worked: _,
                version: _,
                corrections: _,
            } = entry;
            StoredEditable {
                name: name.as_ref(),
                location: location.as_ref(),
                grid: grid.as_ref(),
                signal_report: signal_report.as_ref(),
                staying: *staying,
                precedence: *precedence,
                traffic: *traffic,
                notes: notes.as_ref(),
                public_note: public_note.as_ref(),
                via: via.as_ref(),
                relayed_by: relayed_by.as_ref(),
            }
        });
        let callsign = parse_callsign(&submitted_callsign)
            .map_err(|e| ApiError::CallsignInvalid(e.to_string()))?;
        let name = resolve_replace(
            &submitted_name,
            stored_editable.and_then(|e| e.name),
            parse_edit_name,
        )?;
        let location = resolve_replace(
            &submitted_location,
            stored_editable.and_then(|e| e.location),
            parse_edit_location,
        )?;
        let grid = resolve_replace(
            &submitted_grid,
            stored_editable.and_then(|e| e.grid),
            parse_edit_grid,
        )?;
        let signal_report = resolve_replace(
            &submitted_report,
            stored_editable.and_then(|e| e.signal_report),
            parse_edit_signal_report,
        )?;
        let staying = match &submitted_staying {
            Undefinable::Missing => stored_editable.map(|e| e.staying).unwrap_or_default(),
            other => parse_staying(other.value().map(String::as_str))?,
        };
        // A Logger holds precedence/traffic at the same floor as `EditCheckIn`,
        // so no second gate.
        let precedence = match &submitted_precedence {
            Undefinable::Missing => stored_editable.map(|e| e.precedence).unwrap_or_default(),
            Undefinable::Null => Precedence::default(),
            Undefinable::Value(token) => Precedence::try_from(token.as_str())
                .map_err(|e| ApiError::PrecedenceInvalid(e.to_string()))?,
        };
        let traffic = match &submitted_traffic {
            Undefinable::Missing => stored_editable.and_then(|e| e.traffic),
            Undefinable::Null => None,
            Undefinable::Value(count) => parse_traffic_count(Some(*count))
                .map_err(|e| ApiError::TrafficInvalid(e.to_string()))?,
        };
        let notes = resolve_replace(
            &submitted_notes,
            stored_editable.and_then(|e| e.notes),
            parse_edit_note,
        )?;
        let public_note = resolve_replace(
            &submitted_public_note,
            stored_editable.and_then(|e| e.public_note),
            parse_edit_note,
        )?;
        // A `via` is a structured object, not text, so it cannot go through
        // `resolve_replace` and the three arms are written out. An ABSENT key
        // keeps the STORED value.
        let via = match &submitted_via {
            Undefinable::Missing => stored_editable.and_then(|e| e.via).cloned(),
            Undefinable::Null => None,
            Undefinable::Value(wire) => parse_via(Some(wire), &session)?,
        };
        let relayed_by = resolve_replace(
            &submitted_relayed_by,
            stored_editable.and_then(|e| e.relayed_by),
            parse_edit_relayed_by,
        )?;
        (
            callsign,
            name,
            location,
            grid,
            signal_report,
            staying,
            precedence,
            traffic,
            notes,
            public_note,
            via,
            relayed_by,
        )
    } else {
        // Self path: a participant may toggle ONLY their OWN entry's `staying`.
        // An entry that is not theirs, or absent, is a 403, never a leak.
        let entry = match stored.as_ref() {
            Some(e) if owns_check_in(e.source, e.added_by, current.account_id) => e,
            _ => return Err(ApiError::Forbidden),
        };
        // Mere PRESENCE of a staff-only field is a 403. An explicit `null`
        // carries no value to refuse; the arm forces the field to the stored one
        // regardless, so it can change nothing either way.
        if submitted_report.has_value()
                || submitted_precedence.has_value()
                || submitted_traffic.has_value()
                || submitted_notes.has_value()
                // The PUBLIC note is written ABOUT a station by an operator, for
                // observers. Letting the station itself write it would put
                // unreviewed prose on an unauthenticated page under someone
                // else's editorial frame.
                || submitted_public_note.has_value()
        {
            return Err(ApiError::Forbidden);
        }
        let staying = parse_staying(submitted_staying.value().map(String::as_str))?;
        // Every other field is FORCED to the entry's CURRENT value; only
        // `staying` flows from the request. `checkin.updated` carries the FULL
        // post-edit field set, so a field MISSING from this tuple is not merely
        // un-editable, it is WIPED by the participant's own staying toggle. The
        // destructure is EXHAUSTIVE so a new `RosterEntry` field fails to compile
        // HERE until a human decides whether the toggle carries it forward; the
        // cost of missing one is DESTROYED DATA. A field is carried if and only
        // if it also joins `CheckinUpdated`; the `_`-bound rest are envelope or
        // fold-owned and never rewritten. `api_self_check_in.rs` pins the
        // behaviour and `roster_projection_sites.rs` the dropped-arm count.
        let RosterEntry {
            callsign,
            name,
            location,
            grid,
            signal_report,
            precedence,
            traffic,
            notes,
            public_note,
            via,
            relayed_by,
            staying: _,
            check_in_id: _,
            added_at: _,
            added_by: _,
            added_seq: _,
            source: _,
            worked: _,
            version: _,
            corrections: _,
        } = entry;
        (
            callsign.clone(),
            name.clone(),
            location.clone(),
            grid.clone(),
            signal_report.clone(),
            staying,
            *precedence,
            *traffic,
            notes.clone(),
            public_note.clone(),
            // `via` and `relayed_by` are CARRIED FORWARD, not refused: the
            // participant may not CHANGE them, but erasing them from the staying
            // toggle would destroy facts nothing else in the log records.
            via.clone(),
            relayed_by.clone(),
        )
    };

    let updated_event = match state
        .net_sessions
        .edit_check_in(
            session_id,
            check_in_id,
            expected_version,
            &callsign,
            name.as_ref(),
            location.as_ref(),
            grid.as_ref(),
            signal_report.as_ref(),
            staying,
            precedence,
            traffic,
            notes.as_ref(),
            public_note.as_ref(),
            via.as_ref(),
            relayed_by.as_ref(),
            Some(current.account_id),
            now,
        )
        .await?
    {
        EditCheckInOutcome::Applied(event) => event,
        // The CAS lost (a lock-expiry race, or a stale client) — no append; the
        // loser reconciles against the version it already holds.
        EditCheckInOutcome::StaleVersion => return Err(ApiError::StaleVersion),
        EditCheckInOutcome::NotLive => {
            return Err(ApiError::SessionTransition(
                SessionTransitionError::AlreadyClosed,
            ));
        }
        EditCheckInOutcome::Missing => return Err(ApiError::NetSessionNotFound),
    };
    tracing::info!(
        account_id = %current.account_id,
        net_session_id = %session_id,
        "net session check-in edited"
    );

    // POST-COMMIT publish (the repo already committed the CAS-guarded append):
    // notify-only, so a rolled-back edit never broadcasts.
    state.hub.publish(session_id, &updated_event);

    let summary = session_summary(&state, session_id, viewer_role).await?;
    Ok((StatusCode::OK, Json(summary)).into_response())
}

/// `DELETE /api/net-sessions/{id}/check-ins/{check_in_id}` — tombstones a
/// check-in. A staff operator (`EditCheckIn`) removes any entry; a participant
/// may remove ONLY their OWN self-entry. Same guard order as [`edit_check_in`].
/// Emits `checkin.removed`, which the fold drops from the roster while the
/// append-only log retains the tombstone. Distinct from moderation, which
/// removes ANOTHER station under a higher gate.
async fn remove_check_in(
    State(state): State<AppState>,
    ConsentedAccount(current): ConsentedAccount,
    Path((session_id, check_in_id)): Path<(Uuid, Uuid)>,
    AppJson(body): AppJson<RemoveCheckInRequest>,
) -> Result<Response, ApiError> {
    let (session, viewer_role) =
        authorize_session_role(&state, session_id, current, Capability::SelfCheckIn).await?;
    session_sm::ensure_writable(session.lifecycle, session.control_state)?;

    let now = state.clock.now_epoch_millis();
    if let Some((holder_id, holder_callsign)) =
        state.session_locks.holder(session_id, check_in_id, now)
        && holder_id != current.account_id
    {
        return Err(ApiError::LockHeld(holder_callsign));
    }

    // A non-staff caller may remove ONLY their own self-entry; anything else,
    // or an absent entry, is a 403.
    if !role_has(viewer_role, Capability::EditCheckIn) {
        let events = state.session_events.events_since(session_id, 0).await?;
        let folded = replay(&events, 0);
        let owns = folded.roster.iter().any(|e| {
            e.check_in_id == check_in_id && owns_check_in(e.source, e.added_by, current.account_id)
        });
        if !owns {
            return Err(ApiError::Forbidden);
        }
    }

    let removed_event = match state
        .net_sessions
        .remove_check_in(
            session_id,
            check_in_id,
            body.expected_version,
            Some(current.account_id),
            now,
        )
        .await?
    {
        EditCheckInOutcome::Applied(event) => event,
        EditCheckInOutcome::StaleVersion => return Err(ApiError::StaleVersion),
        EditCheckInOutcome::NotLive => {
            return Err(ApiError::SessionTransition(
                SessionTransitionError::AlreadyClosed,
            ));
        }
        EditCheckInOutcome::Missing => return Err(ApiError::NetSessionNotFound),
    };
    tracing::info!(
        account_id = %current.account_id,
        net_session_id = %session_id,
        "net session check-in removed"
    );

    // A removed entry's advisory lock is now meaningless — best-effort release +
    // broadcast so other consoles clear any editing indicator immediately.
    if state
        .session_locks
        .release(session_id, check_in_id, current.account_id, now)
    {
        state.hub.publish_lock(
            session_id,
            crate::ws::hub::LockDelta {
                check_in_id,
                holder_callsign: None,
                expires_at_millis: None,
            },
        );
    }

    state.hub.publish(session_id, &removed_event);
    let summary = session_summary(&state, session_id, viewer_role).await?;
    Ok((StatusCode::OK, Json(summary)).into_response())
}

/// `POST /api/net-sessions/{id}/check-ins/{check_in_id}/moderate` — the NCS
/// disciplinary remove-and/or-block of a disruptive station. DISTINCT from the
/// `DELETE …/check-ins/{id}` correction remove: that is a Logger-floor logging
/// correction; THIS is a `Capability::Moderate` (NetControl floor) judgment
/// call. Both emit `checkin.removed`, under different gates.
///
/// Guard order preserves 404-before-403 (mirroring `remove_check_in`):
/// 1. `Moderate` (NetControl+) via [`authorize_session_role`] — 404 absent, 403
///    for a Logger/Relay/Participant, 401 unauth.
/// 2. `ensure_writable` — a closed/stalled session's roster is frozen (409).
/// 3. The adapter atomically removes the entry and, when `block == true` AND the
///    target is an account-bearing self entry, ALSO appends `station.blocked`
///    (all-or-nothing); an account-less block is refused 422 `nothing-to-block`
///    with NOTHING removed.
///
/// Moderation is an NCS OVERRIDE: it deliberately does NOT honor the advisory
/// soft-lock that `remove_check_in` checks — an NCS disciplining a disruptive
/// station must not be blocked by another operator's editing lease. Correctness is
/// still guaranteed by the version CAS: an operator mid-editing the removed row
/// simply gets `Missing`/`StaleVersion` on their next commit (the lock is
/// advisory only). On success: POST-COMMIT `hub.publish`
/// the `checkin.removed` AND (if present) the `station.blocked` deltas, then
/// return the folded summary.
async fn moderate_check_in(
    State(state): State<AppState>,
    ConsentedAccount(current): ConsentedAccount,
    Path((session_id, check_in_id)): Path<(Uuid, Uuid)>,
    AppJson(body): AppJson<ModerateCheckInRequest>,
) -> Result<Response, ApiError> {
    let (session, viewer_role) =
        authorize_session_role(&state, session_id, current, Capability::Moderate).await?;
    session_sm::ensure_writable(session.lifecycle, session.control_state)?;

    let now = state.clock.now_epoch_millis();
    let (removed_event, blocked_event) = match state
        .net_sessions
        .moderate_check_in(
            session_id,
            check_in_id,
            body.block,
            body.expected_version,
            Some(current.account_id),
            now,
        )
        .await?
    {
        ModerateOutcome::Applied(applied) => (applied.removed, applied.blocked),
        ModerateOutcome::StaleVersion => return Err(ApiError::StaleVersion),
        ModerateOutcome::NothingToBlock => return Err(ApiError::NothingToBlock),
        ModerateOutcome::NotLive => {
            return Err(ApiError::SessionTransition(
                SessionTransitionError::AlreadyClosed,
            ));
        }
        ModerateOutcome::Missing => return Err(ApiError::NetSessionNotFound),
    };
    tracing::info!(
        account_id = %current.account_id,
        net_session_id = %session_id,
        blocked = blocked_event.is_some(),
        "net session check-in moderated"
    );

    // A removed entry's advisory lock is now meaningless — best-effort release +
    // broadcast so other consoles clear any editing indicator (mirrors
    // `remove_check_in`). The moderator never had to HOLD the lock to remove.
    if let Some((holder_id, _)) = state.session_locks.holder(session_id, check_in_id, now)
        && state
            .session_locks
            .release(session_id, check_in_id, holder_id, now)
    {
        state.hub.publish_lock(
            session_id,
            crate::ws::hub::LockDelta {
                check_in_id,
                holder_callsign: None,
                expires_at_millis: None,
            },
        );
    }

    // POST-COMMIT publish (the repo already committed the guarded appends): both
    // the removal and — when present — the block propagate live to all viewers.
    // The public wire redacts the block's account id (`public_payload`).
    state.hub.publish(session_id, &removed_event);
    if let Some(blocked_event) = &blocked_event {
        state.hub.publish(session_id, blocked_event);
    }

    let summary = session_summary(&state, session_id, viewer_role).await?;
    Ok((StatusCode::OK, Json(summary)).into_response())
}

/// `POST /api/net-sessions/{id}/check-ins/{check_in_id}/lock` — acquires OR (if
/// the caller already holds it) RENEWS the soft-lock lease.
/// Requires `EditCheckIn` (404-before-403). A DIFFERENT account's valid lease →
/// 409 `lock-held` naming the holder. On acquire/renew, broadcasts a `lock`
/// frame to OTHER consoles and returns the lease.
async fn acquire_lock(
    State(state): State<AppState>,
    ConsentedAccount(current): ConsentedAccount,
    Path((session_id, check_in_id)): Path<(Uuid, Uuid)>,
) -> Result<Json<LockBody>, ApiError> {
    authorize_session(&state, session_id, current, Capability::EditCheckIn).await?;

    // The holder's callsign labels the "X is editing…" indicator on other
    // consoles. A staff operator always has a reserved callsign (net creation /
    // role-grant both require one); default defensively if somehow absent.
    let account = state
        .accounts
        .find_by_id(current.account_id)
        .await?
        .ok_or(ApiError::Unauthenticated)?;
    let holder_callsign = account.callsign.unwrap_or_default();

    let now = state.clock.now_epoch_millis();
    match state.session_locks.acquire(
        session_id,
        check_in_id,
        current.account_id,
        &holder_callsign,
        now,
    ) {
        crate::ws::locks::AcquireOutcome::Acquired(lease) => {
            state.hub.publish_lock(
                session_id,
                crate::ws::hub::LockDelta {
                    check_in_id,
                    holder_callsign: Some(lease.holder_callsign.clone()),
                    expires_at_millis: Some(lease.expires_at_millis),
                },
            );
            Ok(Json(LockBody {
                holder_callsign: lease.holder_callsign,
                expires_at: rfc3339(lease.expires_at_millis),
            }))
        }
        crate::ws::locks::AcquireOutcome::Held(holder) => Err(ApiError::LockHeld(holder)),
    }
}

/// `DELETE /api/net-sessions/{id}/check-ins/{check_in_id}/lock` — releases the
/// lease. Only the holder releases; idempotent (releasing an
/// absent/expired lease is still 204). Broadcasts a holder-null `lock` frame
/// only when a lease was actually freed.
async fn release_lock(
    State(state): State<AppState>,
    ConsentedAccount(current): ConsentedAccount,
    Path((session_id, check_in_id)): Path<(Uuid, Uuid)>,
) -> Result<Response, ApiError> {
    authorize_session(&state, session_id, current, Capability::EditCheckIn).await?;

    let now = state.clock.now_epoch_millis();
    if state
        .session_locks
        .release(session_id, check_in_id, current.account_id, now)
    {
        state.hub.publish_lock(
            session_id,
            crate::ws::hub::LockDelta {
                check_in_id,
                holder_callsign: None,
                expires_at_millis: None,
            },
        );
    }
    Ok(StatusCode::NO_CONTENT.into_response())
}

/// Reorder-the-roster body: a `by` strategy discriminator so the shape is
/// forward-compatible. `precedence` is the only strategy; any other value is a
/// 400. Omitted defaults to `precedence`.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ReorderRequest {
    #[serde(default = "default_reorder_strategy")]
    by: String,
}

/// The default reorder strategy — `precedence`, the only strategy.
fn default_reorder_strategy() -> String {
    "precedence".to_owned()
}

/// `POST /api/net-sessions/{id}/reorder` — the NCS reorders the SHARED roster by
/// precedence, seen live by every viewer. Requires the
/// `ReorderRoster` capability (Owner/NCS; a Logger/Relay/Participant is 403),
/// preserving 404-before-403. The guard order mirrors the edit handler:
/// 1. `authorize_session(…, ReorderRoster)` — 404 absent, 403 non-NCS, 401 unauth.
/// 2. `ensure_mutable` — a closed session's roster is frozen (409).
/// 3. Reject an unknown `by` strategy (400).
/// 4. `reorder_roster` folds the log, computes the Emergency → Priority → Routine
///    order, and appends `roster.reordered` atomically behind the live-gate.
///
/// Under the worked-sink ordering mode the precedence order is
/// stable-partitioned before the append, so the emitted permutation is
/// precedence WITHIN each group with the worked block last. Still exactly one
/// event; the composition happens at the command boundary.
///
/// On a real reorder: POST-COMMIT `hub.publish` the delta so every console (owner
/// and public) folds the same new order, then return the folded summary. A no-op
/// reorder (already in precedence order) returns 200 with the current summary and
/// no broadcast — never an error.
async fn reorder_roster(
    State(state): State<AppState>,
    ConsentedAccount(current): ConsentedAccount,
    Path(session_id): Path<Uuid>,
    AppJson(body): AppJson<ReorderRequest>,
) -> Result<Response, ApiError> {
    let (session, viewer_role) =
        authorize_session_role(&state, session_id, current, Capability::ReorderRoster).await?;
    session_sm::ensure_writable(session.lifecycle, session.control_state)?;

    // Only `precedence` is a valid strategy today; the discriminator keeps the
    // shape forward-compatible. Any other value is a 400.
    if body.by != "precedence" {
        return Err(ApiError::validation(
            "reorder strategy must be \"precedence\"",
        ));
    }

    let now = state.clock.now_epoch_millis();
    match state
        .net_sessions
        .reorder_roster(session_id, Some(current.account_id), now)
        .await?
    {
        ReorderOutcome::Reordered(event) => {
            tracing::info!(
                account_id = %current.account_id,
                net_session_id = %session_id,
                "net session roster reordered"
            );
            // POST-COMMIT publish (the repo already committed the append):
            // notify-only, so a rolled-back reorder never broadcasts. The public
            // WS subscribers get this same broadcast (the redacted serializer
            // projects only the order list — see `public_payload`).
            state.hub.publish(session_id, &event);
        }
        // Already in precedence order: no event appended, no broadcast — return
        // the current summary, never an error.
        ReorderOutcome::NoOp => {}
        ReorderOutcome::NotLive => {
            return Err(ApiError::SessionTransition(
                SessionTransitionError::AlreadyClosed,
            ));
        }
        ReorderOutcome::Missing => return Err(ApiError::NetSessionNotFound),
    }

    let summary = session_summary(&state, session_id, viewer_role).await?;
    Ok((StatusCode::OK, Json(summary)).into_response())
}

/// Set-worked-station body: the cursor target, or `null` to clear it (complete
/// the current station).
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct WorkedStationRequest {
    #[serde(default)]
    check_in_id: Option<Uuid>,
}

/// `POST /api/net-sessions/{id}/worked-station` — the NCS designates the single
/// station being worked, seen live by every viewer.
/// Requires `SetWorkedStation` (Owner/NCS; Logger/Relay/Participant → 403),
/// preserving 404-before-403. The guard order mirrors the reorder handler:
/// 1. `authorize_session(…, SetWorkedStation)` — 404 absent, 403 non-NCS, 401 unauth.
/// 2. `ensure_mutable` — a closed session's cursor is frozen (409).
/// 3. `set_worked_station` folds the log, rejects an off-roster target (404),
///    and appends `station.worked-set` atomically behind the live-gate.
///
/// On a real move: POST-COMMIT `hub.publish` the delta so every console (owner
/// and public) folds the same cursor, then return the folded summary. A no-op
/// (cursor already there) returns 200 with the current summary and no broadcast.
///
/// Under the worked-sink ordering mode the repo appends UP TO TWO events in one
/// transaction — the cursor move, then a `roster.reordered` sinking the entry
/// the cursor left — and BOTH are published here, post-commit, in seq order.
async fn set_worked_station(
    State(state): State<AppState>,
    ConsentedAccount(current): ConsentedAccount,
    Path(session_id): Path<Uuid>,
    AppJson(body): AppJson<WorkedStationRequest>,
) -> Result<Response, ApiError> {
    let (session, viewer_role) =
        authorize_session_role(&state, session_id, current, Capability::SetWorkedStation).await?;
    session_sm::ensure_writable(session.lifecycle, session.control_state)?;

    let now = state.clock.now_epoch_millis();
    match state
        .net_sessions
        .set_worked_station(session_id, body.check_in_id, Some(current.account_id), now)
        .await?
    {
        WorkedStationOutcome::Set(applied) => {
            tracing::info!(
                account_id = %current.account_id,
                net_session_id = %session_id,
                "net session worked station set"
            );
            // POST-COMMIT, notify-only. The redacted public serializer keeps only
            // checkInId, since the worked station IS public radio data. Both
            // events go out IN SEQ ORDER so every console folds the cursor move
            // before the permutation that follows from it.
            state.hub.publish(session_id, &applied.set);
            if let Some(reordered) = &applied.reordered {
                state.hub.publish(session_id, reordered);
            }
        }
        // Cursor already where requested — no event, no broadcast.
        WorkedStationOutcome::NoOp => {}
        // A target not on the roster is refused with a 404 — a phantom cursor is
        // never recorded.
        WorkedStationOutcome::UnknownCheckIn => return Err(ApiError::NetSessionNotFound),
        WorkedStationOutcome::NotLive => {
            return Err(ApiError::SessionTransition(
                SessionTransitionError::AlreadyClosed,
            ));
        }
        WorkedStationOutcome::Missing => return Err(ApiError::NetSessionNotFound),
    }

    let summary = session_summary(&state, session_id, viewer_role).await?;
    Ok((StatusCode::OK, Json(summary)).into_response())
}

/// Set-net-note body: the net-level note, or `null`/blank to clear it.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct NetNoteRequest {
    #[serde(default)]
    note: Option<String>,
}

/// `PUT /api/net-sessions/{id}/net-note` — a staff operator sets the net-level
/// note. Requires `AnnotateSession` (Owner/NCS/Logger;
/// Relay/Participant → 403), preserving 404-before-403. The guard order mirrors
/// the reorder handler: authorize → `ensure_mutable` → validate → append. An
/// over-bound note is a 400 `/errors/note-invalid`. The note TEXT never crosses
/// the public wire (its public payload is `{}`).
async fn set_net_note(
    State(state): State<AppState>,
    ConsentedAccount(current): ConsentedAccount,
    Path(session_id): Path<Uuid>,
    AppJson(body): AppJson<NetNoteRequest>,
) -> Result<Response, ApiError> {
    let (session, viewer_role) =
        authorize_session_role(&state, session_id, current, Capability::AnnotateSession).await?;
    session_sm::ensure_writable(session.lifecycle, session.control_state)?;

    // Blank/absent → None (clears the note); an over-bound note is a 400.
    let note = parse_edit_note(body.note.as_deref())?;

    let now = state.clock.now_epoch_millis();
    match state
        .net_sessions
        .set_net_note(session_id, note, Some(current.account_id), now)
        .await?
    {
        NoteOutcome::Set(event) => {
            tracing::info!(
                account_id = %current.account_id,
                net_session_id = %session_id,
                "net session note set"
            );
            // POST-COMMIT publish; the public serializer redacts the note text to
            // an empty payload (see `public_payload`).
            state.hub.publish(session_id, &event);
        }
        NoteOutcome::NoOp => {}
        NoteOutcome::NotLive => {
            return Err(ApiError::SessionTransition(
                SessionTransitionError::AlreadyClosed,
            ));
        }
        NoteOutcome::Missing => return Err(ApiError::NetSessionNotFound),
    }

    let summary = session_summary(&state, session_id, viewer_role).await?;
    Ok((StatusCode::OK, Json(summary)).into_response())
}

/// Set-roster-order-mode body: the standing ordering mode as its lowercase-kebab
/// token.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RosterOrderModeRequest {
    mode: String,
}

/// `POST /api/net-sessions/{id}/roster-order-mode` — the NCS sets the session's
/// standing roster ordering mode, seen live by every viewer.
/// Requires the `SetRosterOrderMode` capability (Owner/NCS; a Logger/Relay/
/// Participant is 403), preserving 404-before-403. The guard order mirrors the
/// reorder handler:
/// 1. `authorize_session(…, SetRosterOrderMode)` — 404 absent, 403 non-NCS.
/// 2. `ensure_writable` — a closed or stalled session's roster is frozen (409).
/// 3. Reject an out-of-vocabulary mode token (400).
/// 4. `set_roster_order_mode` folds the log, skips an unchanged mode, appends
///    `roster.order-mode-set` and — when enabling worked-sink over stations that
///    are already worked — the follow-on `roster.reordered`, atomically.
///
/// Both appended events are published POST-COMMIT in seq order, so every console
/// folds the mode and the order it implies together. A no-op (already in the
/// requested mode) returns 200 with the current summary and no broadcast.
async fn set_roster_order_mode(
    State(state): State<AppState>,
    ConsentedAccount(current): ConsentedAccount,
    Path(session_id): Path<Uuid>,
    AppJson(body): AppJson<RosterOrderModeRequest>,
) -> Result<Response, ApiError> {
    let (session, viewer_role) =
        authorize_session_role(&state, session_id, current, Capability::SetRosterOrderMode).await?;
    session_sm::ensure_writable(session.lifecycle, session.control_state)?;

    let mode = RosterOrderMode::try_from(body.mode.as_str())
        .map_err(|_| ApiError::validation("mode must be \"manual\" or \"worked-sink\""))?;

    let now = state.clock.now_epoch_millis();
    match state
        .net_sessions
        .set_roster_order_mode(session_id, mode, Some(current.account_id), now)
        .await?
    {
        OrderModeOutcome::Set(applied) => {
            tracing::info!(
                account_id = %current.account_id,
                net_session_id = %session_id,
                "net session roster order mode set"
            );
            state.hub.publish(session_id, &applied.mode_set);
            if let Some(reordered) = &applied.reordered {
                state.hub.publish(session_id, reordered);
            }
        }
        OrderModeOutcome::NoOp => {}
        OrderModeOutcome::NotLive => {
            return Err(ApiError::SessionTransition(
                SessionTransitionError::AlreadyClosed,
            ));
        }
        OrderModeOutcome::Missing => return Err(ApiError::NetSessionNotFound),
    }

    let summary = session_summary(&state, session_id, viewer_role).await?;
    Ok((StatusCode::OK, Json(summary)).into_response())
}

/// `GET /api/net-sessions/{id}` — the folded summary for reload. Requires
/// `ViewConsole`. The public or participant-facing read is the separate
/// account-less `/live` surface, deliberately NOT opened here.
async fn get_session(
    State(state): State<AppState>,
    ConsentedAccount(current): ConsentedAccount,
    Path(session_id): Path<Uuid>,
) -> Result<Json<SessionSummaryBody>, ApiError> {
    // Resolve the viewer's OWN role alongside the ViewConsole gate so the staff
    // summary can carry `viewerRole` without a second read.
    let (session, viewer_role) =
        authorize_session_role(&state, session_id, current, Capability::ViewConsole).await?;
    Ok(Json(
        session_summary_from_row(&state, &session, viewer_role).await?,
    ))
}

/// The `?format=` selector for the export download. An absent/unknown value is
/// a 400 (`ApiError::Validation`) at the handler, not a serde rejection, so the
/// message is a stable slug.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ExportQuery {
    #[serde(default)]
    format: Option<String>,
}

// The REASON below QUOTES the refusal string at `export_session`'s format
// match, and the coupling is invisible from either end: rewording that literal
// silently turns this argument into a citation of a string that no longer exists.
impl crate::http::QueryKeyPolicy for ExportQuery {
    const REASON: &'static str = "a dropped or misspelled ?format= is ALREADY a loud 400 (\"format must be csv or adif\"), \
         so the harm `deny_unknown_fields` guards against cannot occur here, and the attribute would REPLACE a \
         specific refusal with a generic one";
}

impl crate::http::LenientQuery for ExportQuery {}

/// `GET /api/net-sessions/{id}/export?format=csv|adif` — a SYNCHRONOUS file
/// download of the folded session. NCS/owner-only via `ExportSession`, through
/// [`authorize_session`] so 404-before-403 holds.
///
/// Built from the SAME fold `build_summary` uses, never hand-assembled from
/// projection columns. Pure generation lives in [`export`]; this handler does
/// only the I/O wiring. A file body is exempt from the JSON-envelope rules (as
/// the SPA and WS frames are); errors still return the normal problem+json.
///
/// The server does NOT gate on lifecycle: export is a faithful projection at any
/// point, and a closed-only gate would add an error surface nothing needs. The
/// FRONTEND surfaces the download link only once the session is closed.
async fn export_session(
    State(state): State<AppState>,
    ConsentedAccount(current): ConsentedAccount,
    Path(session_id): Path<Uuid>,
    AppQuery(params): AppQuery<ExportQuery>,
) -> Result<Response, ApiError> {
    // NCS/owner-only, 404-before-403 preserved by the shared helper.
    let session = authorize_session(&state, session_id, current, Capability::ExportSession).await?;

    // Reject an absent/unknown format before the (larger) fold read.
    let format = match params.format.as_deref() {
        Some("csv") => "csv",
        Some("adif") => "adif",
        _ => return Err(ApiError::validation("format must be csv or adif")),
    };

    // The fold is the single source of truth, exactly as the summary.
    let events = state.session_events.events_since(session_id, 0).await?;
    let folded = replay(&events, 0);

    // BOTH formats need the ways in: the CSV renders a check-in's `via` as a
    // label, the ADIF derives each check-in's own band, mode and propagation.
    // The FROZEN snapshot, NOT `live_connections`: each export resolves per
    // entry as at that entry's own seq, so a station worked before a
    // mid-session QSY keeps the frequency it was worked on.
    let snapshot = &session.definition_snapshot.connections;

    let (body, content_type, ext) = match format {
        "csv" => {
            let entering_ops = resolve_entering_operators(&state, &folded).await?;
            (
                export::to_csv(&folded, &entering_ops, snapshot),
                "text/csv; charset=utf-8",
                "csv",
            )
        }
        // "adif" — the only remaining match arm (validated above).
        _ => {
            // CREATED_TIMESTAMP is the export instant; the pure generator takes it
            // as a parameter so it stays deterministic/testable.
            let now = state.clock.now_epoch_millis();
            (
                export::to_adif(&folded, snapshot, now),
                "text/plain; charset=utf-8",
                "adi",
            )
        }
    };

    let filename = export_filename(&session.definition_snapshot.title, &folded, ext);
    // The filename is ASCII-slugified (only `[a-z0-9-]` plus the date/ext), so no
    // CR/LF/quote/control character from the user-controlled title can ever reach
    // this HTTP header — header-injection safety is structural, not a filter.
    let disposition = format!("attachment; filename=\"{filename}\"");
    Ok((
        [
            (header::CONTENT_TYPE, content_type.to_owned()),
            (header::CONTENT_DISPOSITION, disposition),
        ],
        body,
    )
        .into_response())
}

/// Resolves the distinct entering-operator callsigns for a folded roster. Only
/// STAFF entries carry a meaningful entering operator — a
/// `self` entry entered itself — so only their `added_by` ids are looked up. The
/// set is bounded (≤50 rows, few distinct operators), so a per-id lookup over the
/// distinct set is acceptable (YAGNI: no batch repo method). An id with no
/// claimed callsign is skipped (its CSV cell stays blank).
///
/// The operator id is PII redacted from PUBLIC surfaces, but this export
/// is NCS/owner-authorized, so surfacing the entering operator's callsign to the
/// owner is intended.
async fn resolve_entering_operators(
    state: &AppState,
    folded: &SessionState,
) -> Result<BTreeMap<Uuid, String>, ApiError> {
    let mut distinct: Vec<Uuid> = Vec::new();
    for entry in &folded.roster {
        if entry.source == CheckInSource::Staff
            && let Some(id) = entry.added_by
            && !distinct.contains(&id)
        {
            distinct.push(id);
        }
    }
    let mut resolved = BTreeMap::new();
    for id in distinct {
        if let Some(account) = state.accounts.find_by_id(id).await?
            && let Some(callsign) = account.callsign
        {
            resolved.insert(id, callsign);
        }
    }
    Ok(resolved)
}

/// Builds the download filename `<slug>-<yyyymmdd>.<ext>` from the net title and
/// the session date. The slug is ASCII-only (`[a-z0-9-]`),
/// which is what makes the surrounding `Content-Disposition` header injection-
/// safe; an empty slug falls back to `net-session`. The date is the session's
/// close (or start) instant in UTC; when neither is present the date segment is
/// omitted.
fn export_filename(title: &str, folded: &SessionState, ext: &str) -> String {
    let slug = slugify(title);
    let date = folded
        .closed_at
        .or(folded.started_at)
        .and_then(|ms| chrono::DateTime::from_timestamp_millis(ms as i64))
        .map(|dt| dt.format("%Y%m%d").to_string());
    match date {
        Some(date) => format!("{slug}-{date}.{ext}"),
        None => format!("{slug}.{ext}"),
    }
}

/// ASCII-slugifies a net title for use in a filename/HTTP header: keeps ASCII
/// alphanumerics (lowercased), collapses every other run to a single `-`, trims
/// trailing/leading `-`. Because the output alphabet is exactly `[a-z0-9-]`, any
/// CR/LF/quote/control character in the user-controlled title is dropped, making
/// the header structurally injection-proof. An empty result → `net-session`.
fn slugify(title: &str) -> String {
    let mut slug = String::new();
    let mut pending_dash = false;
    for c in title.chars() {
        if c.is_ascii_alphanumeric() {
            if pending_dash && !slug.is_empty() {
                slug.push('-');
            }
            slug.push(c.to_ascii_lowercase());
            pending_dash = false;
        } else {
            pending_dash = true;
        }
    }
    if slug.is_empty() {
        "net-session".to_owned()
    } else {
        slug
    }
}

/// The `?callsign=` query for the roster-memory lookup.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RosterMemoryQuery {
    #[serde(default)]
    callsign: String,
}

impl crate::http::QueryKeyPolicy for RosterMemoryQuery {
    const REASON: &'static str = "strictness would reverse a shipped design decision: a blank/invalid callsign is a silent \
         no-op, NEVER a 400, because the blur-prefill must not block the operator hot path. \
         The silent-empty outcome is a known gap that still needs its own decision";
}

impl crate::http::LenientQuery for RosterMemoryQuery {}

/// The roster-memory lookup response: the remembered station
/// IDENTITY — name/location only, both nullable. A miss is an empty shape
/// (`{ name: null, location: null }`), never an error — a best-effort prefill.
/// No operational field (report/staying/precedence/traffic/notes) is ever here.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct RosterMemoryResponse {
    name: Option<String>,
    location: Option<String>,
}

/// One source's raw name/location contribution to the check-in autofill merge.
/// All three sources — the callsign owner's NetRoll profile, this net's
/// roster-memory, and the external callbook — are normalized to this uniform
/// `Option<String>` shape so the per-field
/// precedence resolution is branch-free and every candidate crosses the same
/// check-in validation boundary. Both fields are `Option` because any source may
/// supply one, both, or neither.
#[derive(Debug, Default)]
struct AutofillCandidate {
    name: Option<String>,
    location: Option<String>,
}

/// Merges the three prefill sources into the best-effort `(name, location)` a
/// check-in autofill returns, applying the precedence rule independently to
/// each field: **profile > roster-memory > callbook**. (Operator-typed text
/// wins over all three, but that seed-if-empty
/// guard is enforced client-side in `QuickAddRow`; the server only ranks the
/// three source candidates.) Every candidate is re-validated through the
/// check-in boundary grammar and anything that fails is dropped — the callbook
/// is untrusted external text, and the profile/roster values were validated
/// against a different grammar, so a single uniform boundary guarantees only a
/// legal check-in value is ever returned. Pure — no DB, no egress — so the
/// precedence policy lives in one unit-testable place.
fn merge_check_in_autofill(
    profile: AutofillCandidate,
    roster: AutofillCandidate,
    callbook: AutofillCandidate,
) -> (Option<String>, Option<String>) {
    let name = first_valid_candidate([profile.name, roster.name, callbook.name], parse_name);
    let location = first_valid_candidate(
        [profile.location, roster.location, callbook.location],
        parse_location,
    );
    (name, location)
}

/// Returns the first candidate (in precedence order) that parses to a non-blank
/// value through the check-in boundary `parse`, or `None` if none do. A `None`
/// candidate is skipped; a candidate that is blank-after-trim (`Ok(None)`) or
/// fails validation (`Err`) falls through to the next — so an untrusted or
/// empty higher-precedence value never blocks a valid lower one.
fn first_valid_candidate<T: core::fmt::Display>(
    candidates: [Option<String>; 3],
    parse: impl Fn(&str) -> Result<Option<T>, netroll_domain::profile::ProfileError>,
) -> Option<String> {
    candidates
        .into_iter()
        .flatten()
        .find_map(|raw| match parse(&raw) {
            Ok(Some(valid)) => Some(valid.to_string()),
            Ok(None) | Err(_) => None,
        })
}

/// `GET /api/net-sessions/{id}/roster-memory?callsign=<call>` — the staff-gated,
/// definition-scoped roster-memory prefill. The quick-add calls it on callsign
/// blur to seed the editable Name/Location fields for a returning station.
/// Requires `LogCheckIn`, preserving 404-before-403.
///
/// `definition_id` is derived SERVER-SIDE from the resolved session, so a caller
/// can never read another definition's memory. A blank/invalid callsign is a
/// silent no-op returning the empty shape, never a 400: the blur-prefill must
/// not block the operator hot path. A miss returns the empty shape too. Logs ids
/// only — never name/location.
async fn get_roster_memory(
    State(state): State<AppState>,
    ConsentedAccount(current): ConsentedAccount,
    Path(session_id): Path<Uuid>,
    AppQuery(params): AppQuery<RosterMemoryQuery>,
) -> Result<Json<RosterMemoryResponse>, ApiError> {
    let (session, _role) =
        authorize_session_role(&state, session_id, current, Capability::LogCheckIn).await?;

    let callsign = match parse_callsign(&params.callsign) {
        Ok(callsign) => callsign,
        // A blank/invalid callsign is not an error here — the prefill is
        // best-effort and must never block the hot path.
        Err(_) => {
            return Ok(Json(RosterMemoryResponse {
                name: None,
                location: None,
            }));
        }
    };

    let remembered = state
        .roster_memory
        .lookup(session.definition_id, callsign.as_str())
        .await?;
    let (name, location) = match remembered {
        Some(entry) => (entry.name, entry.location),
        None => (None, None),
    };
    Ok(Json(RosterMemoryResponse { name, location }))
}

/// `GET /api/net-sessions/{id}/check-in-autofill?callsign=<call>` — the merged,
/// server-side, best-effort check-in prefill. It resolves the three prefill
/// sources and merges them per-field by [`merge_check_in_autofill`]'s
/// precedence rule (profile > roster-memory > callbook), returning the same
/// editable `{ name, location }` shape the roster-memory endpoint returns. The
/// quick-add calls it on callsign blur; the
/// looked-up values seed only the empty Name/Location fields client-side, so
/// autofill speeds logging but NEVER blocks it.
///
/// Merging server-side is deliberate: the callbook lookup resolves the ACTING
/// operator's stored QRZ credentials (which must never reach the client),
/// and one place owns the precedence rule. The three sources use two different
/// identities — the callbook lookup keys on the acting operator (`current`) to
/// resolve THEIR QRZ subscription, while the profile override keys on the
/// CALLSIGN'S OWNER account (a different account) via [`find_by_callsign`],
/// skipping a soft-deleted owner.
///
/// Best-effort throughout: staff-gated (`LogCheckIn`; Participant → 403, missing
/// session → 404, unauthenticated → 401 via [`authorize_session_role`]); a
/// blank/invalid callsign or a total miss is a `200` empty shape, never a 400/404.
/// A genuine DB-read error on the profile or roster source degrades that source
/// to absent (logged id-only, never name/location) rather than failing the
/// whole autofill; the callbook `LookupService::lookup` cannot error. Every merged
/// candidate is re-validated at the check-in boundary, so a hostile callbook
/// string can never reach the response.
async fn get_check_in_autofill(
    State(state): State<AppState>,
    ConsentedAccount(current): ConsentedAccount,
    Path(session_id): Path<Uuid>,
    AppQuery(params): AppQuery<RosterMemoryQuery>,
) -> Result<Json<RosterMemoryResponse>, ApiError> {
    let (session, _role) =
        authorize_session_role(&state, session_id, current, Capability::LogCheckIn).await?;

    let callsign = match parse_callsign(&params.callsign) {
        Ok(callsign) => callsign,
        // A blank/invalid callsign is not an error — the prefill is best-effort
        // and must never block the hot path (mirrors `get_roster_memory`).
        Err(_) => {
            return Ok(Json(RosterMemoryResponse {
                name: None,
                location: None,
            }));
        }
    };

    // Profile: the CALLSIGN OWNER's self-asserted identity. A genuine DB
    // error degrades to "no profile" rather than failing the autofill.
    let profile = match state.accounts.find_by_callsign(callsign.as_str()).await {
        Ok(Some(account)) if account.deleted_at_millis.is_none() => AutofillCandidate {
            name: account.display_name,
            location: account.location,
        },
        Ok(_) => AutofillCandidate::default(),
        Err(error) => {
            tracing::warn!(
                account_id = %current.account_id,
                "check-in autofill: profile read failed: {error}"
            );
            AutofillCandidate::default()
        }
    };

    // Roster-memory: THIS net's last-logged identity for the callsign.
    let roster = match state
        .roster_memory
        .lookup(session.definition_id, callsign.as_str())
        .await
    {
        Ok(Some(remembered)) => AutofillCandidate {
            name: remembered.name,
            location: remembered.location,
        },
        Ok(None) => AutofillCandidate::default(),
        Err(error) => {
            tracing::warn!(
                account_id = %current.account_id,
                "check-in autofill: roster-memory read failed: {error}"
            );
            AutofillCandidate::default()
        }
    };

    // Callbook: keyed on the ACTING operator's account so it resolves THEIR QRZ
    // subscription. Returns `Option`, never errors.
    let callbook = match state
        .lookup_service()
        .lookup(current.account_id, callsign.as_str())
        .await
    {
        Some(record) => AutofillCandidate {
            name: record.name,
            location: record.location,
        },
        None => AutofillCandidate::default(),
    };

    let (name, location) = merge_check_in_autofill(profile, roster, callbook);
    Ok(Json(RosterMemoryResponse { name, location }))
}

/// `GET /api/net-sessions/{id}/events?since={seq}` — the HTTP catch-up
/// endpoint: the canonical reconnect path for a client whose
/// socket dropped entirely. It returns the raw ordered event gap (`seq > since`)
/// the client folds, then reopens a WS from its now-current cursor. This is a
/// STATELESS read — unlike the WS `?since=` continuation, there is no live
/// stream to strand, so a caught-up or stale cursor is answered with an empty
/// `200`, not a `1008` close.
///
/// Requires `ViewConsole`, reusing [`load_owned_session`] so this endpoint's
/// authz posture is identical to the WS: non-staff → 403, missing session → 404
/// (decided BEFORE authority), unauthenticated → 401. The account-less read path
/// is the separate `/live` surface, deliberately not opened here.
///
/// Session existence is confirmed by the authz `find` inside
/// `load_owned_session` — never inferred from `events_since`'s
/// empty-on-unknown-session silence. Each event is serialized through the
/// shared [`WireEvent`] element, so a catch-up delta is byte-shape-identical to
/// a live WS `event` frame body.
async fn session_events_since(
    State(state): State<AppState>,
    ConsentedAccount(current): ConsentedAccount,
    Path(session_id): Path<Uuid>,
    AppQuery(params): AppQuery<EventsQuery>,
) -> Result<Json<Vec<WireEvent>>, ApiError> {
    // Owner-only + existence (404-before-403) in one read; this find IS the
    // existence authority, so `events_since`'s silence is never trusted to
    // prove the session is real.
    load_owned_session(&state, session_id, current).await?;

    // A cursor above i64::MAX cannot name any real seq (`seq` is bigint-backed
    // and never exceeds i64::MAX), so it is a stale/foreign cursor pointing
    // past everything — the same "nothing newer" case as `since > latest_seq`,
    // answered with an empty 200 rather than the adapter's decode error. It can
    // ONLY arise from a client bug, unlike a merely-stale `since > latest_seq`;
    // the lenient contract is kept so a client mid-reconnect is not broken, and
    // a warning makes the condition visible to operators.
    if params.since > i64::MAX as u64 {
        tracing::warn!(
            net_session_id = %session_id,
            since = params.since,
            "catch-up requested with a since cursor above i64::MAX — cannot name any real \
             seq; likely a client bug, answering with an empty 200 per the lenient resume contract"
        );
        return Ok(Json(Vec::new()));
    }

    let events = state
        .session_events
        .events_since(session_id, params.since)
        .await?;
    let wire: Vec<WireEvent> = events.iter().map(WireEvent::from_event).collect();
    Ok(Json(wire))
}

/// One explicit role grant on the roles-list wire. `callsign` and `grantedBy`
/// are present-null when absent (a grantee without a reserved callsign; a
/// grantor whose account was later deleted). Ids + callsign only — never email.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct RoleGrantBody {
    account_id: Uuid,
    callsign: Option<String>,
    role: Role,
    granted_by: Option<Uuid>,
    granted_at: String,
}

/// `GET /api/net-sessions/{id}/roles` — lists the EXPLICIT role grants on a
/// session. Requires `ManageRoles`, the SAME gate as grant/revoke: only a
/// role-manager needs to audit who holds a grant. The owner is derived, never
/// stored, so the list is the granted staff tiers only. NOT on the public
/// router. Ids + callsign only, never email.
async fn list_roles(
    State(state): State<AppState>,
    ConsentedAccount(current): ConsentedAccount,
    Path(session_id): Path<Uuid>,
) -> Result<Json<Vec<RoleGrantBody>>, ApiError> {
    let (_session, _role) =
        authorize_session_role(&state, session_id, current, Capability::ManageRoles).await?;
    let grants = state.net_session_roles.list_grants(session_id).await?;
    let body: Vec<RoleGrantBody> = grants
        .into_iter()
        .map(|g| RoleGrantBody {
            account_id: g.account_id,
            callsign: g.callsign,
            role: g.role,
            granted_by: g.granted_by,
            granted_at: rfc3339(g.granted_at_millis),
        })
        .collect();
    // Ids only — never callsign/email in the log line.
    tracing::info!(
        net_session_id = %session_id,
        account_id = %current.account_id,
        "session roles listed"
    );
    Ok(Json(body))
}

/// Grant-a-role body: the target station's callsign (the ham-facing identifier,
/// mirroring `add_owner_handler`) and the role to grant as its lowercase-kebab
/// wire token.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct GrantRoleRequest {
    callsign: String,
    role: String,
}

/// The granted-role response body: the resolved target account id and the role
/// now in effect.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct GrantedRoleBody {
    account_id: Uuid,
    role: Role,
}

/// `POST /api/net-sessions/{id}/roles` — grants a per-net role to an account by
/// callsign. Requires the `ManageRoles` capability
/// (Owner/NCS reach it; Logger/Relay/Participant → 403) AND the object-level
/// ceiling `can_manage_role(actor_role, requested)` — an actor may only grant a
/// role STRICTLY below its own rank, so an NCS granting `net-control`/`owner`
/// is refused, and `owner` is never grantable through this surface (owner-set
/// changes go through the owner endpoints). The target is resolved by
/// callsign with the `deleted_at` → 404 guard (mirroring `add_owner_handler`).
/// Upserts on the `(session, account)` key, recording `granted_by`. Returns 200
/// + `{ accountId, role }`.
///
/// A target already IN the definition's owner set is also refused (403): a
/// session-scoped grant to a current owner would be silently inert
/// ([`resolve_role`] checks Owner first), and the row would persist,
/// dormant, until the target is later removed as owner — at which point it
/// would reactivate as a live, un-re-vetted staff grant nobody necessarily
/// remembers making. Refusing at write time closes off that path entirely.
async fn grant_role(
    State(state): State<AppState>,
    ConsentedAccount(current): ConsentedAccount,
    Path(session_id): Path<Uuid>,
    AppJson(body): AppJson<GrantRoleRequest>,
) -> Result<Json<GrantedRoleBody>, ApiError> {
    // ManageRoles gate over the acting account's resolved role (404-before-403).
    // `session` is bound (not `_session`) because the audit append below needs
    // its `definition_id` as the row's net context.
    let (session, actor_role, owner_account_ids) =
        load_session_role(&state, session_id, current).await?;
    if !role_has(actor_role, Capability::ManageRoles) {
        return Err(ApiError::Forbidden);
    }

    // Unknown role token → 400; the deny reason for the ceiling below is a
    // generic 403.
    let requested: Role = body
        .role
        .parse()
        .map_err(|e: netroll_domain::authz::RoleParseError| ApiError::RoleInvalid(e.to_string()))?;

    // Object-level ceiling: strictly above the granted role. This is the
    // single place `owner` is refused via the session surface — no role
    // outranks Owner, so `can_manage_role(_, Owner)` is always false.
    if !can_manage_role(actor_role, requested) {
        return Err(ApiError::Forbidden);
    }

    let callsign =
        parse_callsign(&body.callsign).map_err(|e| ApiError::CallsignInvalid(e.to_string()))?;
    let target = state
        .accounts
        .find_by_callsign(callsign.as_str())
        .await?
        .ok_or(ApiError::OwnerNotFound)?;
    // A departing account (soft-deleted, callsign not yet freed) is treated as
    // absent — the same call `add_owner_handler` makes: never hand a role to
    // an account on its way out.
    if target.deleted_at_millis.is_some() {
        return Err(ApiError::OwnerNotFound);
    }
    // See the doc comment above: a current owner is never a valid grant
    // target through this surface.
    if owner_account_ids.contains(&target.id) {
        return Err(ApiError::Forbidden);
    }

    state
        .net_session_roles
        .grant(session_id, target.id, requested, current.account_id)
        .await?;
    // Ids only — never callsign/email.
    tracing::info!(
        net_session_id = %session_id,
        account_id = %current.account_id,
        target_account_id = %target.id,
        "session role granted"
    );
    // Actor = the granter, target = the grantee account. Metadata carries the
    // role kebab verb + the session uuid, both non-PII; NEVER the target's
    // callsign/email. Post-commit swallow-and-log via the shared seam.
    let now = state.clock.now_epoch_millis();
    super::audit::append_audit(
        &state,
        current.account_id,
        AuditAction::RoleGranted.as_str(),
        // Target = the grantee ACCOUNT (what was acted on); context = the
        // session and its net (where it happened). Both are recorded so the
        // admin object-filter reaches this row from either direction.
        super::audit::AuditSubject::account_in_session(
            target.id,
            session_id,
            session.definition_id,
        ),
        Some(serde_json::json!({ "role": requested.as_str(), "netSessionId": session_id })),
        now,
    )
    .await;

    Ok(Json(GrantedRoleBody {
        account_id: target.id,
        role: requested,
    }))
}

/// `DELETE /api/net-sessions/{id}/roles/{account_id}` — revokes a per-net role
/// Requires the `ManageRoles` capability AND
/// `can_manage_role(actor_role, existing_target_role)` — an actor may only
/// revoke a role it could have granted (revoking a peer/superior → 403). A
/// `(session, account)` pair with no grant → 404 `/errors/role-grant-not-found`
/// (symmetric with `remove_owner`, so a caller can't silently believe a revoke
/// landed). Returns 204.
///
/// The delete is a compare-and-delete against the exact `existing` role just
/// read (`revoke_if_role`), not an unconditional delete by id: without it,
/// a concurrent `grant` could upgrade the target's role between the
/// `can_manage_role` check and the delete, and an unconditional delete would
/// remove whatever is present at that later moment — including a role the
/// actor was never authorized to touch. A mismatch
/// reports the same `RoleGrantNotFound` a sequential double-revoke gets.
async fn revoke_role(
    State(state): State<AppState>,
    ConsentedAccount(current): ConsentedAccount,
    Path((session_id, account_id)): Path<(Uuid, Uuid)>,
) -> Result<Response, ApiError> {
    // `session` is bound (not `_session`) because the audit append below needs
    // its `definition_id` as the row's net context.
    let (session, actor_role, _owner_account_ids) =
        load_session_role(&state, session_id, current).await?;
    if !role_has(actor_role, Capability::ManageRoles) {
        return Err(ApiError::Forbidden);
    }

    // The role must exist to be revoked (404), and the actor must outrank it
    // (403). Owner is never stored here, so an attempt to "revoke" an owner's
    // authority resolves to no grant → 404, never a 200.
    let existing = state
        .net_session_roles
        .find_role(session_id, account_id)
        .await?
        .ok_or(ApiError::RoleGrantNotFound)?;
    if !can_manage_role(actor_role, existing) {
        return Err(ApiError::Forbidden);
    }

    match state
        .net_session_roles
        .revoke_if_role(session_id, account_id, existing)
        .await?
    {
        RevokeOutcome::Revoked => {}
        // Raced to already-revoked OR to a DIFFERENT role between the lookup
        // and here — the same 404 a sequential double-revoke gets; the
        // actor's authorization decision no longer applies to whatever is (or
        // isn't) there now.
        RevokeOutcome::NotAMember => return Err(ApiError::RoleGrantNotFound),
    }
    // Ids only.
    tracing::info!(
        net_session_id = %session_id,
        account_id = %current.account_id,
        target_account_id = %account_id,
        "session role revoked"
    );
    // MUST be written here at the handler: the revoke DELETEs the row, so
    // `granted_by` cannot later reconstruct who revoked. Metadata carries the
    // now-removed role verb + the session uuid; no PII.
    let now = state.clock.now_epoch_millis();
    super::audit::append_audit(
        &state,
        current.account_id,
        AuditAction::RoleRevoked.as_str(),
        // See grant_role: target = the account, context = where it happened.
        super::audit::AuditSubject::account_in_session(
            account_id,
            session_id,
            session.definition_id,
        ),
        Some(serde_json::json!({ "role": existing.as_str(), "netSessionId": session_id })),
        now,
    )
    .await;

    Ok(StatusCode::NO_CONTENT.into_response())
}

/// The PUBLIC, account-less net-session read routes. Merged into
/// `public_read_routes()` so they ride the read IP governor under
/// `api_router_ip_limited` and sit OUTSIDE `require_session` — NO session
/// cookie is consulted on any of them. Three routes, all redacted:
/// - `GET /api/net-sessions/{id}/live` — the redacted snapshot;
/// - `GET /api/net-sessions/{id}/live/events?since=` — the redacted catch-up;
/// - `GET /api/net-sessions/{id}/live/ws` — the public (redacted) WebSocket.
///
/// The literals (`live`, `live/events`, `live/ws`) are distinct from every owner
/// route (`{id}`, `close`, `frequency`, `check-ins`, `events`, `ws`), so there is
/// no matchit collision; the owner surfaces stay session-gated + owner-only,
/// untouched.
pub fn public_net_session_routes() -> Router<AppState> {
    Router::new()
        .route("/api/net-sessions/{id}/live", get(get_public_session))
        .route(
            "/api/net-sessions/{id}/live/events",
            get(public_session_events_since),
        )
        .merge(crate::ws::public_ws_routes())
}

/// `GET /api/net-sessions/{id}/live` — the REDACTED public snapshot.
/// Account-less: NO session extractor, NO owner authz — the session
/// id in the URL is the read capability (exactly as a link token gates the
/// public net page). A missing session → 404. The body OMITS the operator
/// account ids and internal net ids (see [`PublicSessionView`]).
async fn get_public_session(
    State(state): State<AppState>,
    Path(session_id): Path<Uuid>,
) -> Result<Json<PublicSessionView>, ApiError> {
    let row = state
        .net_sessions
        .find(session_id)
        .await?
        .ok_or(ApiError::NetSessionNotFound)?;
    Ok(Json(public_view_from_row(&state, &row).await?))
}

/// `GET /api/net-sessions/{id}/live/events?since={seq}` — the REDACTED public
/// catch-up. Mirrors [`session_events_since`] but with NO
/// owner authz (account-less) and the REDACTED [`WireEvent::from_event_public`]
/// serializer, so no `actorId` and no internal net ids appear in any frame. The
/// session `find` is the existence authority — a missing session → 404, never an
/// empty-200 inferred from `events_since`'s silence.
async fn public_session_events_since(
    State(state): State<AppState>,
    Path(session_id): Path<Uuid>,
    AppQuery(params): AppQuery<EventsQuery>,
) -> Result<Json<Vec<WireEvent>>, ApiError> {
    // The find IS the existence authority (404-on-missing), independent of the
    // events read — the same posture the owner catch-up uses.
    state
        .net_sessions
        .find(session_id)
        .await?
        .ok_or(ApiError::NetSessionNotFound)?;

    // A cursor above i64::MAX cannot name any real seq — the same lenient
    // empty-200 the owner catch-up returns (a client mid-reconnect must not be
    // broken by a strict-mode change).
    if params.since > i64::MAX as u64 {
        tracing::warn!(
            net_session_id = %session_id,
            since = params.since,
            "public catch-up requested with a since cursor above i64::MAX — answering empty 200"
        );
        return Ok(Json(Vec::new()));
    }

    let events = state
        .session_events
        .events_since(session_id, params.since)
        .await?;
    let wire: Vec<WireEvent> = events.iter().map(WireEvent::from_event_public).collect();
    Ok(Json(wire))
}

#[cfg(test)]
mod tests {
    use super::{
        AutofillCandidate, export_filename, merge_check_in_autofill, resolve_role, slugify,
    };
    use netroll_domain::authz::Role;
    use netroll_domain::fold::SessionState;
    use uuid::Uuid;

    #[test]
    fn an_owner_in_the_set_always_resolves_to_owner_regardless_of_any_grant() {
        let owner = Uuid::now_v7();
        // Owner beats any granted staff role (Owner is the apex).
        assert_eq!(resolve_role(&[owner], None, owner), Role::Owner);
        assert_eq!(
            resolve_role(&[owner], Some(Role::Relay), owner),
            Role::Owner
        );
    }

    #[test]
    fn a_non_owner_with_a_grant_resolves_to_the_granted_role() {
        let owner = Uuid::now_v7();
        let staff = Uuid::now_v7();
        assert_eq!(
            resolve_role(&[owner], Some(Role::Logger), staff),
            Role::Logger
        );
    }

    #[test]
    fn a_non_owner_without_a_grant_resolves_to_the_participant_floor() {
        let owner = Uuid::now_v7();
        let stranger = Uuid::now_v7();
        assert_eq!(resolve_role(&[owner], None, stranger), Role::Participant);
    }

    // --- slugify / export_filename ------------------------------------------
    //
    // The export filename is interpolated directly into a `Content-Disposition`
    // header, so an adversarially-crafted net title is a header-injection surface.

    #[test]
    fn an_empty_title_falls_back_to_net_session() {
        assert_eq!(slugify(""), "net-session");
    }

    #[test]
    fn an_all_non_ascii_title_falls_back_to_net_session() {
        // No ASCII alphanumeric survives, so the slug would otherwise be empty —
        // the empty-slug fallback must still trigger for non-Latin titles.
        assert_eq!(slugify("こんにちは"), "net-session");
        assert_eq!(slugify("Привет"), "net-session");
    }

    #[test]
    fn a_punctuation_or_whitespace_only_title_falls_back_to_net_session() {
        assert_eq!(slugify("!!! ... ???"), "net-session");
        assert_eq!(slugify("   "), "net-session");
    }

    #[test]
    fn a_path_traversal_looking_title_has_no_slash_or_dot_in_the_slug() {
        // `/` and `.` are not ASCII alphanumeric, so they collapse to `-` like
        // any other separator — no path-traversal-shaped output is possible.
        assert_eq!(slugify("../../etc/passwd"), "etc-passwd");
    }

    #[test]
    fn a_title_carrying_header_injection_characters_is_neutralized() {
        // CR/LF/quote are exactly the characters that would let a title break out
        // of the `Content-Disposition` header's quoted filename; none survive.
        let slug = slugify("Sunday Net\r\nSet-Cookie: evil=1\"");
        assert!(!slug.contains(['\r', '\n', '"']));
        assert_eq!(slug, "sunday-net-set-cookie-evil-1");
    }

    #[test]
    fn export_filename_appends_the_date_when_present_and_omits_it_otherwise() {
        let mut state = SessionState::default();
        assert_eq!(
            export_filename("Sunday Net", &state, "csv"),
            "sunday-net.csv"
        );

        state.started_at = Some(0); // 1970-01-01T00:00:00Z
        assert_eq!(
            export_filename("Sunday Net", &state, "csv"),
            "sunday-net-19700101.csv"
        );
    }

    #[test]
    fn export_filename_falls_back_for_an_empty_or_non_ascii_title() {
        let state = SessionState::default();
        assert_eq!(export_filename("", &state, "adi"), "net-session.adi");
        assert_eq!(export_filename("日本語", &state, "adi"), "net-session.adi");
    }

    // --- check-in autofill merge --------------------------------------------
    //
    // The precedence rule (per field, highest wins): profile > roster-memory >
    // callbook. Operator-typed wins over all three,
    // but that seed-if-empty guard lives client-side (QuickAddRow) — the
    // server-side merge only ranks the three source candidates. Every candidate
    // is re-validated through the check-in boundary grammar; anything that fails
    // is dropped and the merge falls through.

    fn candidate(name: Option<&str>, location: Option<&str>) -> AutofillCandidate {
        AutofillCandidate {
            name: name.map(str::to_owned),
            location: location.map(str::to_owned),
        }
    }

    #[test]
    fn profile_only_supplies_both_fields() {
        let (name, location) = merge_check_in_autofill(
            candidate(Some("Fred"), Some("Scottsdale, AZ")),
            AutofillCandidate::default(),
            AutofillCandidate::default(),
        );
        assert_eq!(name.as_deref(), Some("Fred"));
        assert_eq!(location.as_deref(), Some("Scottsdale, AZ"));
    }

    #[test]
    fn roster_only_supplies_both_fields() {
        let (name, location) = merge_check_in_autofill(
            AutofillCandidate::default(),
            candidate(Some("Maria"), Some("Hartford, CT")),
            AutofillCandidate::default(),
        );
        assert_eq!(name.as_deref(), Some("Maria"));
        assert_eq!(location.as_deref(), Some("Hartford, CT"));
    }

    #[test]
    fn callbook_only_supplies_both_fields() {
        let (name, location) = merge_check_in_autofill(
            AutofillCandidate::default(),
            AutofillCandidate::default(),
            candidate(Some("Ham Call"), Some("Newington, CT")),
        );
        assert_eq!(name.as_deref(), Some("Ham Call"));
        assert_eq!(location.as_deref(), Some("Newington, CT"));
    }

    #[test]
    fn profile_beats_callbook_on_both_fields() {
        let (name, location) = merge_check_in_autofill(
            candidate(Some("Profile Name"), Some("Profile Loc")),
            AutofillCandidate::default(),
            candidate(Some("Callbook Name"), Some("Callbook Loc")),
        );
        assert_eq!(name.as_deref(), Some("Profile Name"));
        assert_eq!(location.as_deref(), Some("Profile Loc"));
    }

    #[test]
    fn roster_beats_callbook_on_both_fields() {
        let (name, location) = merge_check_in_autofill(
            AutofillCandidate::default(),
            candidate(Some("Roster Name"), Some("Roster Loc")),
            candidate(Some("Callbook Name"), Some("Callbook Loc")),
        );
        assert_eq!(name.as_deref(), Some("Roster Name"));
        assert_eq!(location.as_deref(), Some("Roster Loc"));
    }

    #[test]
    fn profile_beats_roster_on_both_fields() {
        let (name, location) = merge_check_in_autofill(
            candidate(Some("Profile Name"), Some("Profile Loc")),
            candidate(Some("Roster Name"), Some("Roster Loc")),
            AutofillCandidate::default(),
        );
        assert_eq!(name.as_deref(), Some("Profile Name"));
        assert_eq!(location.as_deref(), Some("Profile Loc"));
    }

    #[test]
    fn all_three_present_resolves_to_profile() {
        let (name, location) = merge_check_in_autofill(
            candidate(Some("Profile Name"), Some("Profile Loc")),
            candidate(Some("Roster Name"), Some("Roster Loc")),
            candidate(Some("Callbook Name"), Some("Callbook Loc")),
        );
        assert_eq!(name.as_deref(), Some("Profile Name"));
        assert_eq!(location.as_deref(), Some("Profile Loc"));
    }

    #[test]
    fn fields_resolve_independently_across_sources() {
        // Profile supplies only the name; roster supplies only the location.
        // Each field resolves on its own precedence chain.
        let (name, location) = merge_check_in_autofill(
            candidate(Some("Profile Name"), None),
            candidate(None, Some("Roster Loc")),
            candidate(Some("Callbook Name"), Some("Callbook Loc")),
        );
        assert_eq!(name.as_deref(), Some("Profile Name"));
        assert_eq!(location.as_deref(), Some("Roster Loc"));
    }

    #[test]
    fn a_blank_higher_candidate_is_skipped_for_the_next_source() {
        // A whitespace-only profile name is blank-after-trim (Ok(None)) → the
        // merge falls through to the roster name rather than emitting a blank.
        let (name, _location) = merge_check_in_autofill(
            candidate(Some("   "), None),
            candidate(Some("Roster Name"), None),
            AutofillCandidate::default(),
        );
        assert_eq!(name.as_deref(), Some("Roster Name"));
    }

    #[test]
    fn an_over_length_callbook_value_is_dropped_not_returned() {
        // An untrusted callbook string that fails the check-in grammar
        // (here, over the 64-char name bound) is dropped; with no other source
        // the field is null — the invalid string never reaches the response.
        let over_long = "A".repeat(65);
        let (name, location) = merge_check_in_autofill(
            AutofillCandidate::default(),
            AutofillCandidate::default(),
            candidate(Some(&over_long), Some("Valid City")),
        );
        assert!(
            name.is_none(),
            "the over-length name is dropped, not returned"
        );
        assert_eq!(
            location.as_deref(),
            Some("Valid City"),
            "a sibling valid field still resolves"
        );
    }

    #[test]
    fn an_over_length_location_is_dropped_not_returned() {
        // Location's own bound (128 chars, `MAX_LOCATION_CHARS`) — the name
        // bound (64) has its own case above; this mirrors it for location so both
        // fields' boundary-drop behavior is directly exercised, not just implied.
        let over_long = "A".repeat(129);
        let (name, location) = merge_check_in_autofill(
            AutofillCandidate::default(),
            AutofillCandidate::default(),
            candidate(Some("Valid Name"), Some(&over_long)),
        );
        assert_eq!(
            name.as_deref(),
            Some("Valid Name"),
            "a sibling valid field still resolves"
        );
        assert!(
            location.is_none(),
            "the over-length location is dropped, not returned"
        );
    }

    #[test]
    fn an_invalid_higher_candidate_falls_through_to_a_valid_lower_one() {
        // Fall-through: a profile name that fails validation (control char)
        // is dropped, and the merge continues to the next source.
        let (name, _location) = merge_check_in_autofill(
            candidate(Some("bad\u{0007}name"), None),
            candidate(Some("Roster Name"), None),
            AutofillCandidate::default(),
        );
        assert_eq!(name.as_deref(), Some("Roster Name"));
    }

    #[test]
    fn no_source_present_yields_no_autofill() {
        let (name, location) = merge_check_in_autofill(
            AutofillCandidate::default(),
            AutofillCandidate::default(),
            AutofillCandidate::default(),
        );
        assert!(name.is_none());
        assert!(location.is_none());
    }
}
