// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
//! The `net_sessions` row: a definition frozen BY VALUE at start, plus the
//! projected lifecycle and control columns. Most writes are a guarded UPDATE
//! and an event append in ONE transaction — `create` is the exception, an
//! unguarded INSERT with nothing yet to append. The roster lives only in the
//! event log; the session never re-reads `net_definitions`.

// Every write outcome carries the EXACT appended `SessionEvent` so a caller can
// broadcast it without a second, fallible re-query. Boxing it would put a heap
// allocation on every session write to shrink a value that is moved once and
// immediately matched on.
#![allow(clippy::large_enum_variant)]

use chrono::{DateTime, Utc};
use netroll_domain::callsign::Callsign;
use netroll_domain::check_in::{
    CheckInSource, Location, Name, Note, Precedence, SignalReport, StayingStatus, TrafficCount,
};
use netroll_domain::event::{SessionEvent, SessionEventBody};
use netroll_domain::fold::{
    ControlState, RosterOrderMode, SessionLifecycle, SessionState, order_by_precedence,
    partition_worked_last, replay,
};
use netroll_domain::net::NetDefinition;
use netroll_domain::net::connection::Via;
use netroll_domain::net::wire::{NetConnectionWire, kind_carries_frequency, wire_connections};
use netroll_domain::profile::Grid;
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use uuid::Uuid;

use super::roster_memory::{MergePolicy, upsert_roster_memory_in_tx};
use super::session_events::{append_in_tx, events_since_in_tx};
use super::{UnreplayableLog, millis_from_utc, utc_from_millis};

/// The definition fields a live-session view renders, frozen BY VALUE at
/// session start. Stored as JSONB, so a new definition field never forces a
/// `net_sessions` migration. Enum-ish fields are their stable string tokens.
///
/// **`connections` is REQUIRED and carries no `#[serde(default)]`.** A default
/// cannot tell "a row written before connections existed" from "the writer
/// regressed and dropped the key", and it would render such a net with no way
/// to reach it. A snapshot without the key fails to decode, and that failure
/// reaches the operator as `/errors/unreplayable-log`, not as a blank roster.
///
/// Historical snapshots still carry `band`, `mode` and `plannedFrequencyHz`
/// flat, since MOVED into `connections[0]` (the data survives, just relocated),
/// and six genuinely retired flat keys whose data is gone (`repeaterOffsetHz`,
/// `toneMode`, `toneValue`, `echolinkNode`, `reflector`, `allstarNode`); serde
/// ignores an unknown key either way. This struct must NEVER gain
/// `deny_unknown_fields`, which would turn every one of them into a dead
/// session.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DefinitionSnapshot {
    /// Net title.
    pub title: String,
    /// Free-text description, or `None` (omitted from JSON when absent).
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub description: Option<String>,
    /// Every way to reach this net, in the owner's order. Required, with NO
    /// `#[serde(default)]` — see the type's own doc. The ids are the
    /// DEFINITION's, not freshly minted: a check-in's `via` keys on exactly this
    /// value, and a re-minted id would leave every `via` dangling.
    pub connections: Vec<NetConnectionWire>,
    /// Net category token.
    pub net_category: String,
    /// Net type token.
    pub net_type: String,
    /// Country, or `None`.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub country: Option<String>,
    /// State/province, or `None`.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub state: Option<String>,
    /// Canonical Maidenhead grid, or `None`.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub grid: Option<String>,
}

impl DefinitionSnapshot {
    // EXHAUSTIVE destructure, no `..` rest pattern: this function once read the
    // fields one by one, and when `NetDefinition` gained `connections` it still
    // compiled and silently froze a snapshot with no way to reach the net. A new
    // field must be named here, and a human must decide whether the snapshot
    // carries it. `roster_projection_sites.rs` reds when the `_` arms move.
    fn from_definition(definition: &NetDefinition) -> Self {
        let NetDefinition {
            title,
            description,
            connections,
            net_category,
            net_type,
            country,
            state,
            grid,
            // Provenance the row carries in its OWN columns.
            id: _,
            definition_version: _,
            // Not render-relevant once started, and re-reading them would
            // reintroduce exactly the cascade the snapshot exists to prevent.
            expected_duration_minutes: _,
            visibility: _,
            link_token: _,
            owner_account_ids: _,
            created_at_millis: _,
            updated_at_millis: _,
            archived_at_millis: _,
        } = definition;
        Self {
            title: title.clone(),
            description: description.clone(),
            connections: wire_connections(connections),
            net_category: net_category.as_str().to_owned(),
            net_type: net_type.as_str().to_owned(),
            country: country.clone(),
            state: state.clone(),
            grid: grid.clone(),
        }
    }
}

/// A `net_sessions` row: provenance, the decoded frozen snapshot, and the
/// projected lifecycle/timing/counter columns.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NetSessionRow {
    /// UUIDv7 primary key.
    pub id: Uuid,
    /// The definition this session was spawned from (provenance; the FK is
    /// non-cascading so the session outlives definition deletion).
    pub definition_id: Uuid,
    /// The definition version frozen at start.
    pub definition_version: i32,
    /// The definition's render-relevant fields, frozen at start.
    pub definition_snapshot: DefinitionSnapshot,
    /// The projected lifecycle.
    pub lifecycle: SessionLifecycle,
    /// When the session started, epoch millis, or `None`.
    pub started_at_millis: Option<u64>,
    /// When the session closed, epoch millis, or `None`.
    pub closed_at_millis: Option<u64>,
    /// Orthogonal to `lifecycle`; denormalized so the presence sweep reads it
    /// without folding the log.
    pub control_state: ControlState,
    /// The single server-authoritative active NCS, or `None` when no starter
    /// is recoverable.
    pub active_ncs_account_id: Option<Uuid>,
    /// When the session stalled, or `None` while active — the auto-close clock.
    pub stalled_at_millis: Option<u64>,
    /// The highest `seq` assigned so far (the per-session counter).
    pub last_seq: i64,
    /// Creation instant, epoch millis.
    pub created_at_millis: u64,
    /// Last-update instant, epoch millis.
    pub updated_at_millis: u64,
}

/// The outcome of [`NetSessionRepo::start`]: success carries the row; refusal
/// names WHY, so the HTTP layer maps each reason to its own problem+json.
#[derive(Debug)]
pub enum StartOutcome {
    /// The session was created and its first `session.started` event appended,
    /// atomically. Boxed so the unit arms do not carry the row's size.
    Started(Box<NetSessionRow>),
    /// The definition was archived (or vanished) at INSERT time — including a
    /// race where it was archived AFTER the caller's own pre-flight check.
    DefinitionArchived,
    /// A live session already exists for this definition; the partial unique
    /// index refused a second one.
    AlreadyLive,
}

/// The outcome of [`NetSessionRepo::close`]: refusal names WHY, so two racing
/// closes cannot both append `session.closed`.
#[derive(Debug)]
pub enum CloseOutcome {
    /// The `session.closed` event was appended and the projection flipped,
    /// atomically. Carries the exact appended event so the post-commit publish
    /// never depends on a later, fallible re-query.
    Closed(SessionEvent),
    /// The row exists but was no longer `live` at write time (already closed
    /// by this same request's stale pre-flight read, or by a concurrent
    /// winner). No event was appended.
    NotLive,
    /// No `net_sessions` row exists for the id.
    Missing,
}

/// The outcome of [`NetSessionRepo::change_frequency`]. The guarded UPDATE is
/// the authority, not the caller's unlocked pre-check: a session closing
/// between the two still refuses the change here with no phantom event.
#[derive(Debug)]
pub enum ChangeFrequencyOutcome {
    /// The `frequency.changed` event was appended behind the live/active guard.
    Changed(SessionEvent),
    /// The named connection is not one this session froze at start. Appending
    /// anyway would write a frequency no surface could render, because every
    /// one of them reads the snapshot's set.
    ConnectionUnknown,
    /// The named connection IS on this session, and is not a thing a frequency
    /// belongs to — an EchoLink node, a DMR talkgroup, a reflector. The same
    /// reasoning as [`Self::ConnectionUnknown`], one kind-check further: every
    /// renderer hides a frequency on such a connection, so appending the event
    /// would write a permanent record no surface could ever show.
    ConnectionCarriesNoFrequency,
    /// The row exists but was no longer `live` at write time (already closed by
    /// this same request's stale pre-flight read, or by a concurrent winner).
    /// No event was appended.
    NotLive,
    /// No `net_sessions` row exists for the id.
    Missing,
}

/// The outcome of [`NetSessionRepo::add_check_in`]. The roster has NO
/// projection column — it lives entirely in the event log — so the atomic
/// authority is a guarded `updated_at`-only UPDATE that takes the row lock and
/// enforces liveness; the append happens only if it matched, in the SAME
/// transaction.
#[derive(Debug)]
pub enum AddCheckInOutcome {
    /// The `checkin.added` event was appended behind the live-gate, with the
    /// follow-on worked-sink permutation when the mode called for one.
    Added(Box<CheckInApplied>),
    /// A `checkin.added` with the SAME `(session, clientEventId)` already
    /// exists: the partial unique idempotency index refused the second append.
    /// The caller returns an idempotent success and does NOT re-publish. Only
    /// reachable when `client_event_id` is `Some`; a `NULL` id is exempt from
    /// the partial index.
    Duplicate,
    /// The row exists but was no longer `live` at write time. No event appended.
    NotLive,
    /// No `net_sessions` row exists for the id.
    Missing,
    /// A SELF check-in whose account is on this session's blocklist at write
    /// time, re-checked INSIDE this transaction: a block that commits between
    /// the handler's out-of-transaction pre-check and this append is still
    /// caught here. A staff-logged add is never blocked.
    AccountBlocked,
}

/// The outcome of [`NetSessionRepo::edit_check_in`] /
/// [`NetSessionRepo::remove_check_in`]. The version compare-and-swap runs INSIDE
/// the append transaction on the row-locked connection, so no interleaving edit
/// can slip a lost update past it.
#[derive(Debug)]
pub enum EditCheckInOutcome {
    /// The `checkin.updated`/`checkin.removed` event was appended behind the
    /// live-gate AND the version CAS.
    Applied(SessionEvent),
    /// The entry's current version did not match `expectedVersion` (a
    /// lost-update race, or the entry was already removed). No event appended.
    StaleVersion,
    /// The row exists but was no longer `live` at write time. No event appended.
    NotLive,
    /// No `net_sessions` row exists for the id.
    Missing,
}

/// The outcome of [`NetSessionRepo::moderate_check_in`]. Removal and the
/// optional block run in ONE transaction behind the same live-gate and version
/// CAS as [`EditCheckInOutcome`], so they are all-or-nothing.
#[derive(Debug)]
pub enum ModerateOutcome {
    /// The `checkin.removed` was appended, and the `station.blocked` too when a
    /// block was requested AND the target carried an account.
    Applied(Box<ModerateApplied>),
    /// The entry's current version did not match `expectedVersion`. No event
    /// appended.
    StaleVersion,
    /// A block was requested against an account-less staff-logged entry: there
    /// is no account to key the block on. NOTHING appended, so the removal does
    /// not partially succeed.
    NothingToBlock,
    /// The row exists but was no longer `live` at write time. No event appended.
    NotLive,
    /// No `net_sessions` row exists for the id.
    Missing,
}

/// The appended events a successful [`ModerateOutcome::Applied`] carries.
#[derive(Debug)]
pub struct ModerateApplied {
    /// The `checkin.removed` event dropping the target row.
    pub removed: SessionEvent,
    /// The `station.blocked` event, present only when a block was requested
    /// against an account-bearing (self-checked-in) entry; `None` for a
    /// remove-only moderation.
    pub blocked: Option<SessionEvent>,
}

/// The outcome of [`NetSessionRepo::reorder_roster`]. The fold, sort and append
/// run on the row-locked connection, so no interleaving edit can slip a stale
/// order past it.
#[derive(Debug)]
pub enum ReorderOutcome {
    /// The `roster.reordered` event was appended behind the live-gate.
    Reordered(SessionEvent),
    /// The computed order already equals the current one: no event appended, to
    /// avoid log churn. The caller returns the current summary, never an error.
    NoOp,
    /// The row exists but was no longer `live` at write time. No event appended.
    NotLive,
    /// No `net_sessions` row exists for the id.
    Missing,
}

/// The outcome of [`NetSessionRepo::set_worked_station`]. Mirrors
/// [`ReorderOutcome`]: fold, validate and append on the row-locked connection.
#[derive(Debug)]
pub enum WorkedStationOutcome {
    /// The `station.worked-set` event was appended behind the live-gate, with
    /// the follow-on worked-sink permutation when the mode called for one.
    Set(Box<WorkedStationApplied>),
    /// The cursor already pointed where requested: no event, no broadcast. The
    /// caller returns the current summary, never an error.
    NoOp,
    /// `Some(id)` named a `check_in_id` not on the roster; refused before any
    /// append so a phantom cursor can never be recorded.
    UnknownCheckIn,
    /// The row exists but was no longer `live` at write time. No event appended.
    NotLive,
    /// No `net_sessions` row exists for the id.
    Missing,
}

/// The appended events a successful [`AddCheckInOutcome::Added`] carries. Under
/// [`RosterOrderMode::WorkedSink`] the fold appends a new arrival at the END of
/// the roster, BELOW the worked block, so the command boundary re-sinks it in
/// the SAME transaction.
#[derive(Debug)]
pub struct CheckInApplied {
    /// The `checkin.added` event appending the new roster row.
    pub added: SessionEvent,
    /// The `roster.reordered` event re-sinking the worked block, present only
    /// when the mode is worked-sink AND the order actually changed.
    pub reordered: Option<SessionEvent>,
}

/// The appended events a successful [`WorkedStationOutcome::Set`] carries. Under
/// [`RosterOrderMode::WorkedSink`] the entry the cursor LEAVES flips to `worked`
/// in place, so the command boundary sinks it in the SAME transaction.
#[derive(Debug)]
pub struct WorkedStationApplied {
    /// The `station.worked-set` event moving the cursor.
    pub set: SessionEvent,
    /// The `roster.reordered` event sinking the entry the cursor left, present
    /// only when the mode is worked-sink AND the order actually changed.
    pub reordered: Option<SessionEvent>,
}

/// The appended events a successful [`OrderModeOutcome::Set`] carries. Enabling
/// worked-sink stable-partitions the CURRENT order in the same transaction, so
/// the roster does not wait for the next cursor move to obey the new mode.
#[derive(Debug)]
pub struct OrderModeApplied {
    /// The `roster.order-mode-set` event recording the new standing mode.
    pub mode_set: SessionEvent,
    /// The `roster.reordered` event sinking the already-worked stations, present
    /// only when the new mode is worked-sink AND the order actually changed.
    pub reordered: Option<SessionEvent>,
}

/// The outcome of [`NetSessionRepo::set_roster_order_mode`]. Mirrors
/// [`ReorderOutcome`].
#[derive(Debug)]
pub enum OrderModeOutcome {
    /// The `roster.order-mode-set` was appended behind the live-gate, with its
    /// follow-on permutation when one was needed.
    Set(Box<OrderModeApplied>),
    /// The session already stood in the requested mode: no event, no
    /// broadcast. The caller returns the current summary, never an error.
    NoOp,
    /// The row exists but was no longer `live` at write time. No event appended.
    NotLive,
    /// No `net_sessions` row exists for the id.
    Missing,
}

/// The outcome of [`NetSessionRepo::set_net_note`]. Mirrors [`ReorderOutcome`].
#[derive(Debug)]
pub enum NoteOutcome {
    /// The `session.note-set` event was appended behind the live-gate.
    Set(SessionEvent),
    /// The net note already equals the requested value: no event, no
    /// broadcast. The caller returns the current summary, never an error.
    NoOp,
    /// The row exists but was no longer `live` at write time. No event appended.
    NotLive,
    /// No `net_sessions` row exists for the id.
    Missing,
}

/// The outcome of [`NetSessionRepo::stall`]. The `AND control_state = 'active'`
/// guard is the idempotency authority: a second stall tick over an
/// already-stalled session matches zero rows.
#[derive(Debug)]
pub enum StallOutcome {
    /// The session went `active → stalled`; carries the appended `ncs.stalled`.
    Stalled(SessionEvent),
    /// The row exists but was not `live` + `active` at write time (already
    /// stalled, resumed, claimed, or closed). No event appended — idempotent.
    NotApplicable,
    /// No `net_sessions` row exists for the id.
    Missing,
}

/// The outcome of [`NetSessionRepo::resume`]. Mirrors [`StallOutcome`]; the
/// `AND control_state = 'stalled'` guard is the idempotency authority.
#[derive(Debug)]
pub enum ResumeOutcome {
    /// The session went `stalled → active` (same NCS); carries `ncs.resumed`.
    Resumed(SessionEvent),
    /// The row exists but was not `live` + `stalled` at write time (already
    /// active/claimed/closed). No event appended — idempotent.
    NotApplicable,
    /// No `net_sessions` row exists for the id.
    Missing,
}

/// The outcome of [`NetSessionRepo::hand_off`], the VOLUNTARY handoff on a
/// healthy session. The `AND control_state = 'active' AND
/// active_ncs_account_id = caller` guard is the race authority behind the
/// handler's fast-path checks.
#[derive(Debug)]
pub enum HandoffOutcome {
    /// Control moved to the target; carries the appended `control.handed-off`.
    /// `control_state` stays `active` (stream uninterrupted).
    HandedOff(SessionEvent),
    /// The row exists but was not `live` + `active` with the caller as the
    /// current active NCS at write time (a lost race, or a stall slipped in). No
    /// event appended — the handler maps this to a 409 conflict.
    NotApplicable,
    /// No `net_sessions` row exists for the id.
    Missing,
}

/// The outcome of [`NetSessionRepo::claim_control`], the INVOLUNTARY claim of a
/// STALLED session. The `AND control_state = 'stalled'` guard is the race
/// authority.
#[derive(Debug)]
pub enum ClaimControlOutcome {
    /// The claimer took control; carries the appended `control.handed-off`.
    /// `control_state` returns to `active` under the claimer.
    Claimed(SessionEvent),
    /// The row exists but was not `live` + `stalled` at write time (never
    /// stalled, already resumed, or claimed by a racing winner). No event
    /// appended — the handler maps this to a 409 `control-not-stalled`.
    NotStalled,
    /// No `net_sessions` row exists for the id.
    Missing,
}

/// Whether a refused control write's session row is absent or merely
/// guard-mismatched.
enum ControlNoop {
    /// The row exists but the control guard did not match at write time.
    Present,
    /// No `net_sessions` row exists for the id.
    Missing,
}

/// A live session's denormalized control snapshot: the columns the presence
/// sweep needs each tick WITHOUT folding the event log.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LiveSessionControl {
    /// The session id.
    pub session_id: Uuid,
    /// The projected control status (`Active`/`Stalled`).
    pub control_state: ControlState,
    /// The single active NCS, or `None` (no active NCS → the safe/inert reading).
    pub active_ncs_account_id: Option<Uuid>,
    /// When the session stalled, epoch millis, or `None` while active — the
    /// auto-close clock.
    pub stalled_at_millis: Option<u64>,
}

/// The raw `net_sessions` row shape, shared by create/find so the decode lives
/// in one place.
struct SessionRowRaw {
    id: Uuid,
    definition_id: Uuid,
    definition_version: i32,
    definition_snapshot: serde_json::Value,
    lifecycle: String,
    started_at: Option<DateTime<Utc>>,
    closed_at: Option<DateTime<Utc>>,
    control_state: String,
    active_ncs_account_id: Option<Uuid>,
    stalled_at_millis: Option<i64>,
    last_seq: i64,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

/// The partial unique index enforcing check-in idempotency on
/// `(session_id, clientEventId)`. [`NetSessionRepo::add_check_in`] maps a
/// violation to [`AddCheckInOutcome::Duplicate`].
const CHECKIN_CLIENT_EVENT_ID_IDEMPOTENCY_INDEX: &str =
    "session_events_checkin_client_event_id_idempotent";

/// Maps an out-of-vocabulary stored value to a decode error.
fn decode_error(column: &str) -> sqlx::Error {
    sqlx::Error::Decode(format!("net_sessions.{column} holds an unrepresentable value").into())
}

/// Maps a snapshot this version can no longer read to a DISTINGUISHABLE decode
/// error. Its own error type is the whole point: `ApiError`'s
/// `From<sqlx::Error>` downcasts to it, so this failure cannot be swallowed
/// into the generic `/errors/internal` 500. An operator must be TOLD the log is
/// gone, not told to try again forever.
fn unreplayable_log(column: &str) -> sqlx::Error {
    sqlx::Error::Decode(Box::new(UnreplayableLog {
        what: format!("net_sessions.{column}"),
    }))
}

/// Decodes a stored snapshot, distinguishing the ONE permanently unreadable
/// shape — an object with **no `connections` key** — from every other decode
/// failure (a hand-edited row, a writer regression, a snapshot a NEWER deploy
/// wrote that a rollback is now reading), which answers as a plain decode
/// error. `/errors/unreplayable-log` tells the operator nothing will bring it
/// back; for a row that a roll-forward would restore, that is a confident
/// wrong answer.
fn snapshot_from(value: serde_json::Value) -> Result<DefinitionSnapshot, sqlx::Error> {
    let predates_the_connection_set = value
        .as_object()
        .is_some_and(|object| !object.contains_key("connections"));
    serde_json::from_value(value).map_err(|_| {
        if predates_the_connection_set {
            unreplayable_log("definition_snapshot")
        } else {
            decode_error("definition_snapshot")
        }
    })
}

/// Decodes the stored `lifecycle` text; an unknown value is only reachable via
/// a non-Rust writer.
fn lifecycle_from(value: &str) -> Result<SessionLifecycle, sqlx::Error> {
    match value {
        "scheduled" => Ok(SessionLifecycle::Scheduled),
        "live" => Ok(SessionLifecycle::Live),
        "closed" => Ok(SessionLifecycle::Closed),
        _ => Err(decode_error("lifecycle")),
    }
}

/// Decodes the stored `control_state` text; an unknown value is only reachable
/// via a non-Rust writer.
fn control_state_from(value: &str) -> Result<ControlState, sqlx::Error> {
    match value {
        "active" => Ok(ControlState::Active),
        "stalled" => Ok(ControlState::Stalled),
        _ => Err(decode_error("control_state")),
    }
}

impl SessionRowRaw {
    fn into_row(self) -> Result<NetSessionRow, sqlx::Error> {
        Ok(NetSessionRow {
            id: self.id,
            definition_id: self.definition_id,
            definition_version: self.definition_version,
            definition_snapshot: snapshot_from(self.definition_snapshot)?,
            lifecycle: lifecycle_from(&self.lifecycle)?,
            started_at_millis: self.started_at.map(millis_from_utc),
            closed_at_millis: self.closed_at.map(millis_from_utc),
            control_state: control_state_from(&self.control_state)?,
            active_ncs_account_id: self.active_ncs_account_id,
            stalled_at_millis: self.stalled_at_millis.map(|m| m as u64),
            last_seq: self.last_seq,
            created_at_millis: millis_from_utc(self.created_at),
            updated_at_millis: millis_from_utc(self.updated_at),
        })
    }
}

// Two distinct refusals occur INSIDE the same INSERT statement. The `WHERE
// EXISTS (… archived_at IS NULL)` subquery is evaluated by Postgres at statement
// execution time, so an archive that committed after the handler's unlocked
// pre-flight read is observed here. The partial unique index
// `net_sessions_one_live_per_definition` rejects a second live session.
enum CreateOutcome {
    Created(Box<NetSessionRow>),
    DefinitionArchived,
    AlreadyLive,
}

// Shared by `create` and `start` so the row INSERT commits atomically with the
// first event append. Both guards are evaluated by Postgres in this ONE
// statement; no `SELECT … FOR UPDATE` round trip is needed to close either race.
async fn create_in_tx(
    conn: &mut sqlx::PgConnection,
    definition: &NetDefinition,
    started_at_millis: u64,
    active_ncs_account_id: Option<Uuid>,
) -> Result<CreateOutcome, sqlx::Error> {
    let now = utc_from_millis(started_at_millis);
    let id = Uuid::now_v7();
    let snapshot = serde_json::to_value(DefinitionSnapshot::from_definition(definition))
        .expect("definition snapshot serializes");

    let result = sqlx::query_as!(
        SessionRowRaw,
        "INSERT INTO net_sessions
            (id, definition_id, definition_version, definition_snapshot,
             lifecycle, started_at, active_ncs_account_id,
             last_seq, created_at, updated_at)
         SELECT $1, $2, $3, $4, 'live', $5, $6, 0, $5, $5
         WHERE EXISTS (
             SELECT 1 FROM net_definitions WHERE id = $2 AND archived_at IS NULL
         )
         RETURNING id, definition_id, definition_version, definition_snapshot,
                   lifecycle, started_at, closed_at,
                   control_state, active_ncs_account_id, stalled_at_millis, last_seq,
                   created_at, updated_at",
        id,
        definition.id,
        definition.definition_version,
        snapshot,
        now,
        active_ncs_account_id,
    )
    .fetch_optional(&mut *conn)
    .await;

    match result {
        Ok(Some(row)) => Ok(CreateOutcome::Created(Box::new(row.into_row()?))),
        Ok(None) => Ok(CreateOutcome::DefinitionArchived),
        Err(err) => {
            let is_already_live = err.as_database_error().and_then(|e| e.constraint())
                == Some("net_sessions_one_live_per_definition");
            if is_already_live {
                Ok(CreateOutcome::AlreadyLive)
            } else {
                Err(err)
            }
        }
    }
}

// The `AND lifecycle = 'live'` guard, re-evaluated by Postgres at UPDATE time,
// is what stops two concurrent closes from both appending `session.closed`: the
// loser matches zero rows once the winner has committed.
async fn mark_closed_in_tx(
    conn: &mut sqlx::PgConnection,
    session_id: Uuid,
    closed_at_millis: u64,
) -> Result<bool, sqlx::Error> {
    let closed = utc_from_millis(closed_at_millis);
    let result = sqlx::query!(
        "UPDATE net_sessions SET lifecycle = 'closed', closed_at = $2, updated_at = $2
         WHERE id = $1 AND lifecycle = 'live'",
        session_id,
        closed,
    )
    .execute(&mut *conn)
    .await?;
    Ok(result.rows_affected() == 1)
}

// A frequency belongs to a CONNECTION in the frozen snapshot, so there is no
// projection column to write: this `updated_at`-only UPDATE exists to take the
// row lock and re-evaluate the live/active predicate at write time, which is
// what keeps a `frequency.changed` from landing past a close or a stall.
async fn guard_frequency_change_in_tx(
    conn: &mut sqlx::PgConnection,
    session_id: Uuid,
    at_millis: u64,
) -> Result<bool, sqlx::Error> {
    let at = utc_from_millis(at_millis);
    let result = sqlx::query!(
        "UPDATE net_sessions SET updated_at = $2
         WHERE id = $1 AND lifecycle = 'live' AND control_state = 'active'",
        session_id,
        at,
    )
    .execute(&mut *conn)
    .await?;
    Ok(result.rows_affected() == 1)
}

// The no-op compare matters: a 60-station net must not double its log and its
// WS traffic on the hot path. `folded` must ALREADY include the event this
// permutation follows, so the partition cannot disagree with what the fold
// will project.
async fn append_worked_sink_in_tx(
    conn: &mut sqlx::PgConnection,
    session_id: Uuid,
    folded: &SessionState,
    actor_id: Option<Uuid>,
    at_millis: u64,
) -> Result<Option<SessionEvent>, sqlx::Error> {
    if folded.roster_order_mode != RosterOrderMode::WorkedSink {
        return Ok(None);
    }
    let current: Vec<Uuid> = folded
        .roster
        .iter()
        .map(|entry| entry.check_in_id)
        .collect();
    let order = partition_worked_last(&folded.roster, folded.working_check_in_id, &current);
    if order == current {
        return Ok(None);
    }
    let event = append_in_tx(
        conn,
        session_id,
        &SessionEventBody::RosterReordered { order },
        actor_id,
        at_millis,
    )
    .await?;
    Ok(Some(event))
}

// The roster has no projection column, so this `updated_at`-only UPDATE exists
// to take the row lock and re-evaluate the live/active predicate at write time;
// a write racing a close or a stall loses here, not at the caller's unlocked
// pre-check. `RETURNING definition_id` hands back the roster-memory scope key
// under the SAME lock, so memory can never be keyed to a definition the
// committed check-in did not belong to. `None` ⇒ not live (or absent).
async fn guard_live_in_tx(
    conn: &mut sqlx::PgConnection,
    session_id: Uuid,
    at_millis: u64,
) -> Result<Option<Uuid>, sqlx::Error> {
    let at = utc_from_millis(at_millis);
    let row = sqlx::query!(
        "UPDATE net_sessions SET updated_at = $2
         WHERE id = $1 AND lifecycle = 'live' AND control_state = 'active'
         RETURNING definition_id",
        session_id,
        at,
    )
    .fetch_optional(&mut *conn)
    .await?;
    Ok(row.map(|r| r.definition_id))
}

/// Postgres repository for `net_sessions`.
#[derive(Clone)]
pub struct NetSessionRepo {
    pool: PgPool,
}

impl NetSessionRepo {
    /// Wraps the shared connection pool.
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    /// Creates a live session row with no events. An archived definition, or
    /// one that already has a live session, surfaces as
    /// `Err(sqlx::Error::RowNotFound)`; callers that need the refusal reason or
    /// an atomic founding event use [`NetSessionRepo::start`].
    pub async fn create(
        &self,
        definition: &NetDefinition,
        started_at_millis: u64,
    ) -> Result<NetSessionRow, sqlx::Error> {
        let mut tx = self.pool.begin().await?;
        let row = match create_in_tx(&mut tx, definition, started_at_millis, None).await? {
            CreateOutcome::Created(row) => *row,
            CreateOutcome::DefinitionArchived | CreateOutcome::AlreadyLive => {
                tx.rollback().await?;
                return Err(sqlx::Error::RowNotFound);
            }
        };
        tx.commit().await?;
        Ok(row)
    }

    /// Starts a session ATOMICALLY: the row INSERT and the first
    /// `session.started` append commit in ONE transaction, or neither does, so
    /// a live session can never exist without its founding event. The starter
    /// is recorded as the initial active NCS in the same INSERT.
    pub async fn start(
        &self,
        definition: &NetDefinition,
        actor_id: Option<Uuid>,
        started_at_millis: u64,
    ) -> Result<StartOutcome, sqlx::Error> {
        let mut tx = self.pool.begin().await?;
        let mut row = match create_in_tx(&mut tx, definition, started_at_millis, actor_id).await? {
            CreateOutcome::Created(row) => row,
            CreateOutcome::DefinitionArchived => {
                tx.rollback().await?;
                return Ok(StartOutcome::DefinitionArchived);
            }
            CreateOutcome::AlreadyLive => {
                tx.rollback().await?;
                return Ok(StartOutcome::AlreadyLive);
            }
        };
        let body = SessionEventBody::SessionStarted {
            definition_id: row.definition_id,
            definition_version: row.definition_version,
        };
        let event = append_in_tx(&mut tx, row.id, &body, actor_id, started_at_millis).await?;
        tx.commit().await?;
        // The append advanced the counter; the returned row must match the
        // persisted state.
        row.last_seq = event.seq as i64;
        Ok(StartOutcome::Started(row))
    }

    /// Closes a session ATOMICALLY: the guarded projection flip and the
    /// `session.closed` append commit in ONE transaction, or neither does. The
    /// flip is checked FIRST, so a losing race never produces a phantom event.
    /// `closed_at_millis` is the caller's already-clamped instant.
    pub async fn close(
        &self,
        session_id: Uuid,
        closed_at_millis: u64,
        actor_id: Option<Uuid>,
    ) -> Result<CloseOutcome, sqlx::Error> {
        let mut tx = self.pool.begin().await?;
        if !mark_closed_in_tx(&mut tx, session_id, closed_at_millis).await? {
            // Distinguish "never existed" from "no longer live" without appending.
            let exists = sqlx::query!(
                "SELECT 1 as one FROM net_sessions WHERE id = $1",
                session_id
            )
            .fetch_optional(&mut *tx)
            .await?
            .is_some();
            tx.rollback().await?;
            return Ok(if exists {
                CloseOutcome::NotLive
            } else {
                CloseOutcome::Missing
            });
        }
        let event = append_in_tx(
            &mut tx,
            session_id,
            &SessionEventBody::SessionClosed,
            actor_id,
            closed_at_millis,
        )
        .await?;
        // The delivery legs are planned INSIDE this transaction, so there is no
        // instant at which the session is closed and nothing durable records
        // that a summary is owed; an enqueue after `commit()` leaves exactly
        // that window, and a restart inside it loses the summary silently.
        super::delivery_jobs::plan_in_tx(&mut tx, session_id, closed_at_millis).await?;
        tx.commit().await?;
        Ok(CloseOutcome::Closed(event))
    }

    /// Changes ONE snapshot connection's operating frequency on a LIVE session
    /// ATOMICALLY: the guarded live-gate and the `frequency.changed` append
    /// commit in ONE transaction, or neither does.
    ///
    /// `connection_id` is resolved against the session's OWN frozen snapshot,
    /// not the definition: the two may legitimately differ on the night, and a
    /// frequency written against an id this session never froze would be
    /// invisible on every surface, because every one renders the snapshot's set.
    pub async fn change_frequency(
        &self,
        session_id: Uuid,
        connection_id: Uuid,
        operating_frequency_hz: i64,
        actor_id: Option<Uuid>,
        at_millis: u64,
    ) -> Result<ChangeFrequencyOutcome, sqlx::Error> {
        let mut tx = self.pool.begin().await?;
        if !guard_frequency_change_in_tx(&mut tx, session_id, at_millis).await? {
            let exists = sqlx::query!(
                "SELECT 1 as one FROM net_sessions WHERE id = $1",
                session_id
            )
            .fetch_optional(&mut *tx)
            .await?
            .is_some();
            tx.rollback().await?;
            return Ok(if exists {
                ChangeFrequencyOutcome::NotLive
            } else {
                ChangeFrequencyOutcome::Missing
            });
        }
        // The KIND comes back with the membership answer, not as a second probe:
        // "is this connection on the session" and "is it a thing a frequency
        // belongs to" are one decision about one stored element, and two reads
        // could disagree about which element they looked at.
        let kind = sqlx::query_scalar!(
            r#"SELECT c->>'kind' AS "kind?"
                 FROM net_sessions ns,
                      jsonb_array_elements(ns.definition_snapshot->'connections') AS c
                WHERE ns.id = $1 AND c->>'id' = $2::text
                LIMIT 1"#,
            session_id,
            connection_id.to_string(),
        )
        .fetch_optional(&mut *tx)
        .await?
        .flatten();
        let Some(kind) = kind else {
            tx.rollback().await?;
            return Ok(ChangeFrequencyOutcome::ConnectionUnknown);
        };
        if !kind_carries_frequency(&kind) {
            tx.rollback().await?;
            return Ok(ChangeFrequencyOutcome::ConnectionCarriesNoFrequency);
        }
        let event = append_in_tx(
            &mut tx,
            session_id,
            &SessionEventBody::FrequencyChanged {
                connection_id,
                operating_frequency_hz,
            },
            actor_id,
            at_millis,
        )
        .await?;
        tx.commit().await?;
        Ok(ChangeFrequencyOutcome::Changed(event))
    }

    /// Adds a check-in to a LIVE session ATOMICALLY: the row-locking live-gate
    /// and the `checkin.added` append commit in ONE transaction, or neither
    /// does. A SELF check-in also re-checks the session's blocklist inside this
    /// same transaction; see [`AddCheckInOutcome::AccountBlocked`]. Do NOT
    /// append via the unguarded
    /// [`super::session_events::SessionEventLog::append`]: its bare
    /// `UPDATE … last_seq WHERE id` has no lifecycle guard.
    // A params struct would only relocate the same arity behind a name.
    #[allow(clippy::too_many_arguments)]
    pub async fn add_check_in(
        &self,
        session_id: Uuid,
        callsign: &Callsign,
        check_in_id: Uuid,
        client_event_id: Option<Uuid>,
        signal_report: Option<&SignalReport>,
        staying: StayingStatus,
        name: Option<&Name>,
        location: Option<&Location>,
        grid: Option<&Grid>,
        source: CheckInSource,
        via: Option<&Via>,
        relayed_by: Option<&Callsign>,
        actor_id: Option<Uuid>,
        at_millis: u64,
    ) -> Result<AddCheckInOutcome, sqlx::Error> {
        let mut tx = self.pool.begin().await?;
        let definition_id = match guard_live_in_tx(&mut tx, session_id, at_millis).await? {
            Some(definition_id) => definition_id,
            None => {
                let exists = sqlx::query!(
                    "SELECT 1 as one FROM net_sessions WHERE id = $1",
                    session_id
                )
                .fetch_optional(&mut *tx)
                .await?
                .is_some();
                tx.rollback().await?;
                return Ok(if exists {
                    AddCheckInOutcome::NotLive
                } else {
                    AddCheckInOutcome::Missing
                });
            }
        };

        // The handler's blocklist pre-check reads an out-of-transaction fold, so
        // a block committed in the gap would slip through; re-folding on this
        // row-locked connection closes that race. The ONE fold also carries the
        // ordering mode the permutation below needs, so it is taken
        // unconditionally: two folds of the same connection could only diverge.
        let folded = replay(&events_since_in_tx(&mut tx, session_id).await?, 0);
        if source == CheckInSource::SelfService
            && let Some(acting) = actor_id
            && folded.blocked_account_ids.contains(&acting)
        {
            tx.rollback().await?;
            return Ok(AddCheckInOutcome::AccountBlocked);
        }

        let event = match append_in_tx(
            &mut tx,
            session_id,
            &SessionEventBody::CheckinAdded {
                check_in_id,
                callsign: callsign.clone(),
                client_event_id,
                signal_report: signal_report.cloned(),
                staying,
                name: name.cloned(),
                location: location.cloned(),
                grid: grid.cloned(),
                source,
                via: via.cloned(),
                relayed_by: relayed_by.cloned(),
            },
            actor_id,
            at_millis,
        )
        .await
        {
            Ok(event) => event,
            Err(err) => {
                // The failed append aborted the transaction and reverted the
                // in-txn `last_seq` bump, so the log stays gapless and the
                // original check-in stands.
                let is_duplicate = err.as_database_error().and_then(|e| e.constraint())
                    == Some(CHECKIN_CLIENT_EVENT_ID_IDEMPOTENCY_INDEX);
                tx.rollback().await?;
                if is_duplicate {
                    return Ok(AddCheckInOutcome::Duplicate);
                }
                return Err(err);
            }
        };
        // In the same transaction, so memory never diverges from the log.
        // `PreserveOnNull`: a callsign-only add must not erase a previously
        // remembered name/location. The edit path uses the opposite policy.
        upsert_roster_memory_in_tx(
            &mut tx,
            definition_id,
            callsign.as_str(),
            name.map(Name::as_str),
            location.map(Location::as_str),
            at_millis,
            MergePolicy::PreserveOnNull,
        )
        .await?;
        // The fold appends a new check-in at the END of the roster, BELOW the
        // worked block, so under worked-sink it is re-sunk here at the next seq.
        let reordered = append_worked_sink_in_tx(
            &mut tx,
            session_id,
            &netroll_domain::fold::fold(folded, &event),
            actor_id,
            at_millis,
        )
        .await?;
        tx.commit().await?;
        Ok(AddCheckInOutcome::Added(Box::new(CheckInApplied {
            added: event,
            reordered,
        })))
    }

    /// Edits a roster entry ATOMICALLY behind the live-gate AND a version CAS.
    /// The row lock serializes concurrent same-session appends, so comparing
    /// the entry's current fold version to `expected_version` cannot interleave
    /// with another edit.
    #[allow(clippy::too_many_arguments)]
    pub async fn edit_check_in(
        &self,
        session_id: Uuid,
        check_in_id: Uuid,
        expected_version: u64,
        callsign: &Callsign,
        name: Option<&Name>,
        location: Option<&Location>,
        grid: Option<&Grid>,
        signal_report: Option<&SignalReport>,
        staying: StayingStatus,
        precedence: Precedence,
        traffic: Option<TrafficCount>,
        notes: Option<&Note>,
        public_note: Option<&Note>,
        via: Option<&Via>,
        relayed_by: Option<&Callsign>,
        actor_id: Option<Uuid>,
        at_millis: u64,
    ) -> Result<EditCheckInOutcome, sqlx::Error> {
        self.apply_guarded_check_in_edit(
            session_id,
            check_in_id,
            expected_version,
            SessionEventBody::CheckinUpdated {
                check_in_id,
                callsign: callsign.clone(),
                name: name.cloned(),
                location: location.cloned(),
                grid: grid.cloned(),
                signal_report: signal_report.cloned(),
                staying,
                precedence,
                traffic,
                notes: notes.cloned(),
                public_note: public_note.cloned(),
                via: via.cloned(),
                relayed_by: relayed_by.cloned(),
            },
            actor_id,
            at_millis,
        )
        .await
    }

    /// Removes (tombstones) a roster entry ATOMICALLY behind the same live-gate
    /// and version CAS as [`NetSessionRepo::edit_check_in`]. The fold drops the
    /// row; the append-only log retains the `checkin.removed` tombstone.
    pub async fn remove_check_in(
        &self,
        session_id: Uuid,
        check_in_id: Uuid,
        expected_version: u64,
        actor_id: Option<Uuid>,
        at_millis: u64,
    ) -> Result<EditCheckInOutcome, sqlx::Error> {
        self.apply_guarded_check_in_edit(
            session_id,
            check_in_id,
            expected_version,
            SessionEventBody::CheckinRemoved { check_in_id },
            actor_id,
            at_millis,
        )
        .await
    }

    // The version read runs on the SAME connection the live-gate row-locked, so
    // the compare-and-append is atomic.
    async fn apply_guarded_check_in_edit(
        &self,
        session_id: Uuid,
        check_in_id: Uuid,
        expected_version: u64,
        body: SessionEventBody,
        actor_id: Option<Uuid>,
        at_millis: u64,
    ) -> Result<EditCheckInOutcome, sqlx::Error> {
        let mut tx = self.pool.begin().await?;
        let definition_id = match guard_live_in_tx(&mut tx, session_id, at_millis).await? {
            Some(definition_id) => definition_id,
            None => {
                let exists = sqlx::query!(
                    "SELECT 1 as one FROM net_sessions WHERE id = $1",
                    session_id
                )
                .fetch_optional(&mut *tx)
                .await?
                .is_some();
                tx.rollback().await?;
                return Ok(if exists {
                    EditCheckInOutcome::NotLive
                } else {
                    EditCheckInOutcome::Missing
                });
            }
        };

        // An absent entry (already removed, or never existed) can never match,
        // so it is a stale-version refusal.
        let events = events_since_in_tx(&mut tx, session_id).await?;
        let folded = replay(&events, 0);
        let current_version = folded
            .roster
            .iter()
            .find(|entry| entry.check_in_id == check_in_id)
            .map(|entry| entry.version);
        if current_version != Some(expected_version) {
            tx.rollback().await?;
            return Ok(EditCheckInOutcome::StaleVersion);
        }

        let event = append_in_tx(&mut tx, session_id, &body, actor_id, at_millis).await?;
        // A `checkin.removed` is a tombstone, not an identity statement, so it
        // never writes memory. `ReplaceOnNull`, unlike the add path: an edit
        // carries the FULL post-edit field set, so a deliberately blanked
        // Name/Location must overwrite the memory too, not be preserved forever.
        if let SessionEventBody::CheckinUpdated {
            callsign,
            name,
            location,
            ..
        } = &body
        {
            upsert_roster_memory_in_tx(
                &mut tx,
                definition_id,
                callsign.as_str(),
                name.as_ref().map(Name::as_str),
                location.as_ref().map(Location::as_str),
                at_millis,
                MergePolicy::ReplaceOnNull,
            )
            .await?;
        }
        tx.commit().await?;
        Ok(EditCheckInOutcome::Applied(event))
    }

    /// Disciplinary remove, and optional account block, of a disruptive station
    /// ATOMICALLY behind the same live-gate + version CAS as
    /// [`NetSessionRepo::remove_check_in`]. When `block` is `true` but the
    /// target is account-less, it appends NOTHING and returns
    /// [`ModerateOutcome::NothingToBlock`]: the removal must not partially
    /// succeed. The block is keyed on the entry's resolved account id, NEVER a
    /// client-supplied id or the callsign string.
    pub async fn moderate_check_in(
        &self,
        session_id: Uuid,
        check_in_id: Uuid,
        block: bool,
        expected_version: u64,
        actor_id: Option<Uuid>,
        at_millis: u64,
    ) -> Result<ModerateOutcome, sqlx::Error> {
        let mut tx = self.pool.begin().await?;
        if guard_live_in_tx(&mut tx, session_id, at_millis)
            .await?
            .is_none()
        {
            let exists = sqlx::query!(
                "SELECT 1 as one FROM net_sessions WHERE id = $1",
                session_id
            )
            .fetch_optional(&mut *tx)
            .await?
            .is_some();
            tx.rollback().await?;
            return Ok(if exists {
                ModerateOutcome::NotLive
            } else {
                ModerateOutcome::Missing
            });
        }

        // An absent entry can never match the version, so it is a stale-version
        // refusal.
        let events = events_since_in_tx(&mut tx, session_id).await?;
        let folded = replay(&events, 0);
        let target = folded
            .roster
            .iter()
            .find(|entry| entry.check_in_id == check_in_id);
        let (current_version, block_account_id) = match target {
            Some(entry) => {
                // A staff-logged, account-less entry has nothing to key a block on.
                let account = if entry.source == CheckInSource::SelfService {
                    entry.added_by
                } else {
                    None
                };
                (Some(entry.version), account)
            }
            None => (None, None),
        };
        if current_version != Some(expected_version) {
            tx.rollback().await?;
            return Ok(ModerateOutcome::StaleVersion);
        }

        if block && block_account_id.is_none() {
            tx.rollback().await?;
            return Ok(ModerateOutcome::NothingToBlock);
        }

        let removed = append_in_tx(
            &mut tx,
            session_id,
            &SessionEventBody::CheckinRemoved { check_in_id },
            actor_id,
            at_millis,
        )
        .await?;

        // Matched rather than `.expect()`-unwrapped, so a future refactor of the
        // guard above degrades to a silent no-block instead of a
        // mid-transaction panic.
        let blocked = match (block, block_account_id) {
            (true, Some(account_id)) => {
                let event = append_in_tx(
                    &mut tx,
                    session_id,
                    &SessionEventBody::StationBlocked { account_id },
                    actor_id,
                    at_millis,
                )
                .await?;
                Some(event)
            }
            _ => None,
        };

        tx.commit().await?;
        Ok(ModerateOutcome::Applied(Box::new(ModerateApplied {
            removed,
            blocked,
        })))
    }

    /// Reorders the roster by precedence ATOMICALLY behind the live-gate, in ONE
    /// transaction so the order the event records is consistent with the log it
    /// was computed from. Under [`RosterOrderMode::WorkedSink`] the precedence
    /// order is then stable-partitioned so worked stations sink last: precedence
    /// WITHIN each group, never across the boundary.
    pub async fn reorder_roster(
        &self,
        session_id: Uuid,
        actor_id: Option<Uuid>,
        at_millis: u64,
    ) -> Result<ReorderOutcome, sqlx::Error> {
        let mut tx = self.pool.begin().await?;
        if guard_live_in_tx(&mut tx, session_id, at_millis)
            .await?
            .is_none()
        {
            let exists = sqlx::query!(
                "SELECT 1 as one FROM net_sessions WHERE id = $1",
                session_id
            )
            .fetch_optional(&mut *tx)
            .await?
            .is_some();
            tx.rollback().await?;
            return Ok(if exists {
                ReorderOutcome::NotLive
            } else {
                ReorderOutcome::Missing
            });
        }

        let events = events_since_in_tx(&mut tx, session_id).await?;
        let folded = replay(&events, 0);
        let order = order_by_precedence(&folded.roster);
        // Worked-sink is the OUTER key and precedence the INNER one. The
        // partition is STABLE, so it preserves the precedence order inside each
        // group and no "precedence mode is active" flag is needed.
        let order = if folded.roster_order_mode == RosterOrderMode::WorkedSink {
            partition_worked_last(&folded.roster, folded.working_check_in_id, &order)
        } else {
            order
        };
        let current: Vec<Uuid> = folded
            .roster
            .iter()
            .map(|entry| entry.check_in_id)
            .collect();
        if order == current {
            tx.rollback().await?;
            return Ok(ReorderOutcome::NoOp);
        }

        let event = append_in_tx(
            &mut tx,
            session_id,
            &SessionEventBody::RosterReordered { order },
            actor_id,
            at_millis,
        )
        .await?;
        tx.commit().await?;
        Ok(ReorderOutcome::Reordered(event))
    }

    /// Moves the working-station cursor ATOMICALLY behind the live-gate,
    /// mirroring [`NetSessionRepo::reorder_roster`]. Under
    /// [`RosterOrderMode::WorkedSink`] this appends UP TO TWO events, the cursor
    /// move then a `roster.reordered` sinking the entry the cursor left, at
    /// consecutive `seq` values in the SAME transaction: marking a station
    /// worked DOES change the roster order, even though the fold arm itself only
    /// moves a pointer.
    pub async fn set_worked_station(
        &self,
        session_id: Uuid,
        check_in_id: Option<Uuid>,
        actor_id: Option<Uuid>,
        at_millis: u64,
    ) -> Result<WorkedStationOutcome, sqlx::Error> {
        let mut tx = self.pool.begin().await?;
        if guard_live_in_tx(&mut tx, session_id, at_millis)
            .await?
            .is_none()
        {
            let exists = sqlx::query!(
                "SELECT 1 as one FROM net_sessions WHERE id = $1",
                session_id
            )
            .fetch_optional(&mut *tx)
            .await?
            .is_some();
            tx.rollback().await?;
            return Ok(if exists {
                WorkedStationOutcome::NotLive
            } else {
                WorkedStationOutcome::Missing
            });
        }

        let events = events_since_in_tx(&mut tx, session_id).await?;
        let folded = replay(&events, 0);
        if let Some(id) = check_in_id
            && !folded.roster.iter().any(|e| e.check_in_id == id)
        {
            tx.rollback().await?;
            return Ok(WorkedStationOutcome::UnknownCheckIn);
        }
        if folded.working_check_in_id == check_in_id {
            tx.rollback().await?;
            return Ok(WorkedStationOutcome::NoOp);
        }

        let event = append_in_tx(
            &mut tx,
            session_id,
            &SessionEventBody::StationWorkedSet { check_in_id },
            actor_id,
            at_millis,
        )
        .await?;
        // Apply the just-appended event rather than re-deriving `worked` by
        // hand, then sink the entry the cursor LEFT in this same transaction, so
        // a failure appends neither event.
        let reordered = append_worked_sink_in_tx(
            &mut tx,
            session_id,
            &netroll_domain::fold::fold(folded, &event),
            actor_id,
            at_millis,
        )
        .await?;
        tx.commit().await?;
        Ok(WorkedStationOutcome::Set(Box::new(WorkedStationApplied {
            set: event,
            reordered,
        })))
    }

    /// Sets or clears the net-level note ATOMICALLY behind the live-gate,
    /// mirroring [`NetSessionRepo::reorder_roster`].
    pub async fn set_net_note(
        &self,
        session_id: Uuid,
        note: Option<Note>,
        actor_id: Option<Uuid>,
        at_millis: u64,
    ) -> Result<NoteOutcome, sqlx::Error> {
        let mut tx = self.pool.begin().await?;
        if guard_live_in_tx(&mut tx, session_id, at_millis)
            .await?
            .is_none()
        {
            let exists = sqlx::query!(
                "SELECT 1 as one FROM net_sessions WHERE id = $1",
                session_id
            )
            .fetch_optional(&mut *tx)
            .await?
            .is_some();
            tx.rollback().await?;
            return Ok(if exists {
                NoteOutcome::NotLive
            } else {
                NoteOutcome::Missing
            });
        }

        let events = events_since_in_tx(&mut tx, session_id).await?;
        let folded = replay(&events, 0);
        if folded.net_note == note {
            tx.rollback().await?;
            return Ok(NoteOutcome::NoOp);
        }

        let event = append_in_tx(
            &mut tx,
            session_id,
            &SessionEventBody::SessionNoteSet { note },
            actor_id,
            at_millis,
        )
        .await?;
        tx.commit().await?;
        Ok(NoteOutcome::Set(event))
    }

    /// Sets the session's standing ROSTER ORDERING MODE atomically behind the
    /// live-gate, mirroring [`NetSessionRepo::set_net_note`]. Turning worked-sink
    /// ON also stable-partitions the CURRENT order in the same transaction, so
    /// already-worked stations sink immediately; that second append is skipped
    /// when the order is already correct.
    pub async fn set_roster_order_mode(
        &self,
        session_id: Uuid,
        mode: RosterOrderMode,
        actor_id: Option<Uuid>,
        at_millis: u64,
    ) -> Result<OrderModeOutcome, sqlx::Error> {
        let mut tx = self.pool.begin().await?;
        if guard_live_in_tx(&mut tx, session_id, at_millis)
            .await?
            .is_none()
        {
            let exists = sqlx::query!(
                "SELECT 1 as one FROM net_sessions WHERE id = $1",
                session_id
            )
            .fetch_optional(&mut *tx)
            .await?
            .is_some();
            tx.rollback().await?;
            return Ok(if exists {
                OrderModeOutcome::NotLive
            } else {
                OrderModeOutcome::Missing
            });
        }

        let events = events_since_in_tx(&mut tx, session_id).await?;
        let folded = replay(&events, 0);
        if folded.roster_order_mode == mode {
            tx.rollback().await?;
            return Ok(OrderModeOutcome::NoOp);
        }

        let mode_set = append_in_tx(
            &mut tx,
            session_id,
            &SessionEventBody::RosterOrderModeSet { mode },
            actor_id,
            at_millis,
        )
        .await?;
        let reordered = append_worked_sink_in_tx(
            &mut tx,
            session_id,
            &netroll_domain::fold::fold(folded, &mode_set),
            actor_id,
            at_millis,
        )
        .await?;
        tx.commit().await?;
        Ok(OrderModeOutcome::Set(Box::new(OrderModeApplied {
            mode_set,
            reordered,
        })))
    }

    /// Marks a LIVE + ACTIVE session STALLED atomically: the guarded flip and
    /// the `ncs.stalled` append commit in ONE transaction, or neither. The
    /// `AND control_state = 'active'` guard makes a second stall tick a no-op.
    /// System-originated: `actor_id = None`.
    pub async fn stall(
        &self,
        session_id: Uuid,
        at_millis: u64,
    ) -> Result<StallOutcome, sqlx::Error> {
        let mut tx = self.pool.begin().await?;
        let at = utc_from_millis(at_millis);
        let flipped = sqlx::query!(
            "UPDATE net_sessions
             SET control_state = 'stalled', stalled_at_millis = $2, updated_at = $3
             WHERE id = $1 AND lifecycle = 'live' AND control_state = 'active'",
            session_id,
            at_millis as i64,
            at,
        )
        .execute(&mut *tx)
        .await?
        .rows_affected()
            == 1;
        if !flipped {
            let outcome = self.classify_control_noop(&mut tx, session_id).await?;
            tx.rollback().await?;
            return Ok(match outcome {
                ControlNoop::Missing => StallOutcome::Missing,
                ControlNoop::Present => StallOutcome::NotApplicable,
            });
        }
        let event = append_in_tx(
            &mut tx,
            session_id,
            &SessionEventBody::NcsStalled,
            None,
            at_millis,
        )
        .await?;
        tx.commit().await?;
        Ok(StallOutcome::Stalled(event))
    }

    /// Returns a STALLED session to ACTIVE atomically when the original active
    /// NCS's heartbeat returns. The `AND control_state = 'stalled'` guard makes
    /// it idempotent, and inert once a claim already brought the net back.
    /// System-originated: `actor_id = None`.
    pub async fn resume(
        &self,
        session_id: Uuid,
        at_millis: u64,
    ) -> Result<ResumeOutcome, sqlx::Error> {
        let mut tx = self.pool.begin().await?;
        let at = utc_from_millis(at_millis);
        let flipped = sqlx::query!(
            "UPDATE net_sessions
             SET control_state = 'active', stalled_at_millis = NULL, updated_at = $2
             WHERE id = $1 AND lifecycle = 'live' AND control_state = 'stalled'",
            session_id,
            at,
        )
        .execute(&mut *tx)
        .await?
        .rows_affected()
            == 1;
        if !flipped {
            let outcome = self.classify_control_noop(&mut tx, session_id).await?;
            tx.rollback().await?;
            return Ok(match outcome {
                ControlNoop::Missing => ResumeOutcome::Missing,
                ControlNoop::Present => ResumeOutcome::NotApplicable,
            });
        }
        let event = append_in_tx(
            &mut tx,
            session_id,
            &SessionEventBody::NcsResumed,
            None,
            at_millis,
        )
        .await?;
        tx.commit().await?;
        Ok(ResumeOutcome::Resumed(event))
    }

    /// VOLUNTARY handoff on a healthy session: moves the active NCS to
    /// `new_ncs_account_id` atomically, keeping `control_state` `active`.
    /// Guarded on `control_state = 'active' AND active_ncs_account_id = caller`
    /// so only the CURRENT active NCS can hand off. Both handoff paths mint the
    /// same `control.handed-off` kind.
    pub async fn hand_off(
        &self,
        session_id: Uuid,
        caller: Uuid,
        new_ncs_account_id: Uuid,
        at_millis: u64,
    ) -> Result<HandoffOutcome, sqlx::Error> {
        let mut tx = self.pool.begin().await?;
        let at = utc_from_millis(at_millis);
        let flipped = sqlx::query!(
            "UPDATE net_sessions
             SET active_ncs_account_id = $2, control_state = 'active',
                 stalled_at_millis = NULL, updated_at = $3
             WHERE id = $1 AND lifecycle = 'live' AND control_state = 'active'
               AND active_ncs_account_id = $4",
            session_id,
            new_ncs_account_id,
            at,
            caller,
        )
        .execute(&mut *tx)
        .await?
        .rows_affected()
            == 1;
        if !flipped {
            let outcome = self.classify_control_noop(&mut tx, session_id).await?;
            tx.rollback().await?;
            return Ok(match outcome {
                ControlNoop::Missing => HandoffOutcome::Missing,
                ControlNoop::Present => HandoffOutcome::NotApplicable,
            });
        }
        let event = append_in_tx(
            &mut tx,
            session_id,
            &SessionEventBody::ControlHandedOff { new_ncs_account_id },
            Some(caller),
            at_millis,
        )
        .await?;
        tx.commit().await?;
        Ok(HandoffOutcome::HandedOff(event))
    }

    /// INVOLUNTARY claim of a STALLED session: the claimer becomes the active
    /// NCS and the net returns to `active` under them. Guarded on
    /// `control_state = 'stalled'` so a claim on a non-stalled session appends
    /// nothing. Same `control.handed-off` kind as the voluntary path.
    pub async fn claim_control(
        &self,
        session_id: Uuid,
        claimer: Uuid,
        at_millis: u64,
    ) -> Result<ClaimControlOutcome, sqlx::Error> {
        let mut tx = self.pool.begin().await?;
        let at = utc_from_millis(at_millis);
        let flipped = sqlx::query!(
            "UPDATE net_sessions
             SET active_ncs_account_id = $2, control_state = 'active',
                 stalled_at_millis = NULL, updated_at = $3
             WHERE id = $1 AND lifecycle = 'live' AND control_state = 'stalled'",
            session_id,
            claimer,
            at,
        )
        .execute(&mut *tx)
        .await?
        .rows_affected()
            == 1;
        if !flipped {
            let outcome = self.classify_control_noop(&mut tx, session_id).await?;
            tx.rollback().await?;
            return Ok(match outcome {
                ControlNoop::Missing => ClaimControlOutcome::Missing,
                ControlNoop::Present => ClaimControlOutcome::NotStalled,
            });
        }
        let event = append_in_tx(
            &mut tx,
            session_id,
            &SessionEventBody::ControlHandedOff {
                new_ncs_account_id: claimer,
            },
            Some(claimer),
            at_millis,
        )
        .await?;
        tx.commit().await?;
        Ok(ClaimControlOutcome::Claimed(event))
    }

    async fn classify_control_noop(
        &self,
        tx: &mut sqlx::PgConnection,
        session_id: Uuid,
    ) -> Result<ControlNoop, sqlx::Error> {
        let exists = sqlx::query!(
            "SELECT 1 as one FROM net_sessions WHERE id = $1",
            session_id
        )
        .fetch_optional(&mut *tx)
        .await?
        .is_some();
        Ok(if exists {
            ControlNoop::Present
        } else {
            ControlNoop::Missing
        })
    }

    /// Lists every LIVE session's denormalized control snapshot for the presence
    /// sweep: one indexed scan over the live set, never folding an event log.
    pub async fn list_live_control_states(&self) -> Result<Vec<LiveSessionControl>, sqlx::Error> {
        let rows = sqlx::query!(
            "SELECT id, control_state, active_ncs_account_id, stalled_at_millis
             FROM net_sessions
             WHERE lifecycle = 'live'"
        )
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter()
            .map(|r| {
                Ok(LiveSessionControl {
                    session_id: r.id,
                    control_state: control_state_from(&r.control_state)?,
                    active_ncs_account_id: r.active_ncs_account_id,
                    stalled_at_millis: r.stalled_at_millis.map(|m| m as u64),
                })
            })
            .collect()
    }

    /// The definition's current live session id, or `None`. At most one row can
    /// ever match: the `net_sessions_one_live_per_definition` partial unique
    /// index is the authority.
    pub async fn find_live_session_id_by_definition(
        &self,
        definition_id: Uuid,
    ) -> Result<Option<Uuid>, sqlx::Error> {
        let row = sqlx::query!(
            "SELECT id FROM net_sessions WHERE definition_id = $1 AND lifecycle = 'live'",
            definition_id,
        )
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(|r| r.id))
    }

    /// Looks up a session by id. A missing id → `None`.
    pub async fn find(&self, id: Uuid) -> Result<Option<NetSessionRow>, sqlx::Error> {
        let row = sqlx::query_as!(
            SessionRowRaw,
            "SELECT id, definition_id, definition_version, definition_snapshot,
                    lifecycle, started_at, closed_at,
                    control_state, active_ncs_account_id, stalled_at_millis, last_seq,
                    created_at, updated_at
             FROM net_sessions WHERE id = $1",
            id,
        )
        .fetch_optional(&self.pool)
        .await?;

        match row {
            None => Ok(None),
            Some(row) => Ok(Some(row.into_row()?)),
        }
    }
}
