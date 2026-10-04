// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
//! The append-only session event log, and the durable seat of the per-session
//! monotonic `seq`. `append` assigns it in ONE transaction, bumping
//! `net_sessions.last_seq` under the row lock `UPDATE ... WHERE id` takes, so
//! same-session appends serialize and a rollback keeps the sequence gapless.
//! `payload` holds only the body's own fields; the envelope lives in columns.

use netroll_domain::callsign::{Callsign, parse_callsign};
use netroll_domain::check_in::{
    CheckInSource, Location, Name, Precedence, SignalReport, StayingStatus, parse_location,
    parse_name, parse_note, parse_signal_report, parse_traffic_count,
};
use netroll_domain::event::{SessionEvent, SessionEventBody};
use netroll_domain::fold::{
    FrequencyMove, RosterOrderMode, frequencies_as_at, overlay_frequencies,
};
use netroll_domain::net::connection::{Via, parse_via_text};
use netroll_domain::net::wire::{
    NetConnectionWire, ViaDisplay, ViaWire, resolve_via, via_from_wire, via_label, via_of,
};
use netroll_domain::profile::parse_grid;
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use uuid::Uuid;

use netroll_domain::admin::PageCursor;

use super::{Page, into_page, millis_from_utc, utc_from_millis};

/// Postgres repository for the append-only `session_events` log.
#[derive(Clone)]
pub struct SessionEventLog {
    pool: PgPool,
}

/// One SELF check-in in an account's personal-data export: a session the
/// account checked ITSELF into (`source = self`), with the
/// session's snapshotted net title and the check-in's own fields. Staff-entered
/// check-ins the account logged for OTHER stations are deliberately excluded —
/// see [`SessionEventLog::self_check_ins`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SelfCheckIn {
    /// The session the account checked into.
    pub session_id: Uuid,
    /// The net's title, snapshotted onto the session at start.
    pub net_title: String,
    /// The account's own logged callsign.
    pub callsign: Callsign,
    /// The mode-shaped signal report, or `None`.
    pub signal_report: Option<SignalReport>,
    /// The staying status at check-in.
    pub staying: StayingStatus,
    /// The prefilled operator name, or `None`.
    pub name: Option<Name>,
    /// The prefilled free-text location, or `None`.
    pub location: Option<Location>,
    /// WHICH way in the account came in on, as its LABEL, or `None`
    /// when nobody recorded one.
    ///
    /// The LABEL and not the id: this is the account's own copy of its own data,
    /// read by a person, and a snapshot-local UUID is not a fact anybody outside
    /// this database can use. An unresolvable one still says so in words rather
    /// than going blank — going blank would make it indistinguishable from "not
    /// recorded", which is a different fact.
    ///
    /// Resolved against the way in AS IT STOOD when this check-in was logged
    /// — an RF label's frequency is the one in force at
    /// this row's own `seq`, not the planned one and not the one the net later
    /// moved to.
    pub via: Option<String>,
    /// When the check-in was logged, epoch millis.
    pub checked_in_at_millis: u64,
}

/// One entry in the account's OWN check-in history, shaped for the profile
/// widget rather than for a bulk export.
///
/// Deliberately not [`SelfCheckIn`]. The two answer the same ownership question
/// but publish different contracts, and widening the export's row type to serve
/// a widget would widen the export itself — which AC pins.
///
/// Every field here is stable AT ADD TIME. That is the selection rule, not an
/// accident: this read serves `checkin.added` rows and folds neither
/// `checkin.updated` nor `checkin.removed`, so any operator-editable field
/// (signal report, staying, name, location) would be rendered stale with no way
/// for the reader to tell. `callsign` is exempt because the self path forces it
/// from the account profile and ignores any client value; `net_title`/`band`/
/// `mode` are exempt because they come from the session's frozen
/// `definition_snapshot`, which by construction never changes after start.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SelfCheckInEntry {
    /// The event row's id. Carried even though the widget never renders it,
    /// because it is the keyset tiebreak WITHIN a shared `created_at` — without
    /// it a page boundary landing inside a tie skips or repeats a row.
    pub id: Uuid,
    /// The session the account checked into.
    pub session_id: Uuid,
    /// The net's title, snapshotted onto the session at start.
    pub net_title: String,
    /// The band of the connection THIS check-in came in on, or `None`.
    ///
    /// It was once the net's FIRST connection's
    /// band, which is a fact about the net and not about the check-in: on a
    /// cross-mode net it told every EchoLink participant they had been on 20m.
    /// It is now derived from the entry's own `via`, and a check-in whose `via`
    /// is unrecorded, unresolvable or free text reports NO band rather than
    /// position zero's — the substitution is the defect, and an absent band is
    /// already a shape this widget renders (an internet-only net has none).
    pub band: Option<String>,
    /// The mode of the connection this check-in came in on — see [`Self::band`].
    pub mode: Option<String>,
    /// WHICH way in this check-in came in on, as its LABEL, or
    /// `None` when nobody recorded one. The reader's actual answer when `band`
    /// and `mode` are absent because the way in carries neither.
    ///
    /// The label is of the way in AS IT STOOD when this check-in was logged
    /// — an RF frequency in it is the one in force at
    /// this row's own `seq`. Still stable at add time, which is this read's
    /// selection rule: the log before a row does not change after it.
    pub via: Option<String>,
    /// The callsign the account was logged under — meaningful in its own right
    /// for an account that has since changed callsigns.
    pub callsign: Callsign,
    /// When the check-in was logged, epoch millis.
    pub checked_in_at_millis: u64,
}

// The camelCase payload DTOs — one per body variant. Only the variant's OWN
// fields; the envelope is columns. `SessionClosed` has no DTO (it serializes to
// an empty object).

/// `deny_unknown_fields` is what makes refusal reachable: an older
/// `session.started` carries `operatingFrequencyHz`, and serde
/// would otherwise ignore the key and decode the record as if the session had
/// never had a session-level frequency at all. That is precisely the silent
/// translation this decode deliberately forbids — the record would replay
/// with the operator's chosen frequency quietly discarded.
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct SessionStartedPayload {
    definition_id: Uuid,
    definition_version: i32,
}

/// `connection_id` is REQUIRED and has no `default`. An older
/// `frequency.changed` names no connection, and there is no connection in its
/// session's snapshot to attribute it to; it is refused rather than translated.
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct FrequencyChangedPayload {
    connection_id: Uuid,
    operating_frequency_hz: i64,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CheckinAddedPayload {
    check_in_id: Uuid,
    // The callsign crosses as its normalized string form; re-parsed via the
    // Callsign newtype on read.
    callsign: String,
    // Omitted from JSON when None (the omit-optional-fields wire rule).
    #[serde(skip_serializing_if = "Option::is_none", default)]
    client_event_id: Option<Uuid>,
    // The mode-shaped signal report, omitted when absent (the same
    // omit-optional-fields rule as `client_event_id`). Re-validated via
    // `parse_signal_report` on read.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    signal_report: Option<String>,
    // The staying status as its kebab token. `#[serde(default)]` so a
    // HISTORICAL field-less `checkin.added` payload decodes
    // to `in-and-out` rather than failing — the additive-compat guarantee.
    #[serde(default = "default_staying_token")]
    staying: String,
    // The prefilled operator name captured at check-in, omitted when
    // absent (the same omit-optional rule as `signal_report`). `#[serde(default)]`
    // so an older payload with no key decodes to `None`. Re-validated via
    // `parse_name` on read.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    name: Option<String>,
    // The prefilled free-text location, mirroring `name`.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    location: Option<String>,
    // The canonical Maidenhead grid, a SEPARATE field from the
    // free-text `location` above. `skip_serializing_if` is the load-bearing half:
    // it keeps a later grid-less event byte-indistinguishable from one
    // written before it (no `"grid": null`), which is what preserves byte-identity
    // with the already-stored, never-rewritten older payloads. `default` is
    // REDUNDANT for an `Option<T>` — serde's derive already yields `None` for a
    // missing optional field, verified by removing it and watching the
    // historical-decode test stay green — and is kept only for consistency with the
    // four existing precedents above. Re-validated via `parse_grid` on read.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    grid: Option<String>,
    // The staff/self provenance token. `#[serde(default)]` so a
    // HISTORICAL `checkin.added` payload with no key decodes to `staff`
    // (the additive-compat guarantee) — no self entry predates the field.
    #[serde(default = "default_source_token")]
    source: String,
    // WHICH way in the station arrived on — a `kind`-discriminated
    // object, never two sibling nullable keys, so "both set" and "neither set"
    // cannot be stored. `skip_serializing_if` is the load-bearing half, exactly
    // as for `grid`: it keeps a later via-less event
    // byte-indistinguishable from one written before it, which is what preserves
    // byte-identity with the already-stored, never-rewritten older payloads.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    via: Option<StoredVia>,
    // WHICH STATION passed this check-in's traffic, as its
    // normalized callsign string — re-parsed through `parse_callsign` on read,
    // the same posture the entry's own callsign has. A plain string and not a
    // `kind`-discriminated object, because unlike `via` there is only ever one
    // kind of answer. `skip_serializing_if` is the load-bearing half, exactly as
    // for `via` and `grid`: it keeps a later relay-less event
    // byte-indistinguishable from one written before it.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    relayed_by: Option<String>,
}

/// A stored `via`, read back TOTALLY.
///
/// The write side only ever produces [`StoredVia::Readable`], and `untagged`
/// makes that variant serialize as the bare [`ViaWire`] object — byte-identical
/// to what the write side stores. The second variant exists for the READ side only.
///
/// **Why it exists.** `ViaWire` is `kind`-discriminated, so an unknown `kind`
/// token — a row written by a newer deploy during a rollback, or a hand-edited
/// payload — failed the WHOLE `CheckinAddedPayload` deserialize, which took the
/// check-in-history page, the personal-data export and the session's WS replay
/// down with it. One unreadable value now costs one check-in's LABEL and nothing
/// else. `connections_of` in this same file was already total for exactly this
/// class; this is the same answer for the per-event half, found the same way.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
enum StoredVia {
    /// A `via` this build can read.
    Readable(ViaWire),
    /// A `via` this build cannot read — kept as stored, never rewritten.
    Unreadable(serde_json::Value),
}

/// The sentinel a `via` this build cannot read resolves through.
///
/// `Uuid::nil()` is never minted — every connection id is a `now_v7` — so it can
/// never name a real way in, and `resolve_via` therefore answers
/// [`ViaDisplay::Unresolvable`], which SAYS SO IN WORDS. That is the point: the
/// one answer forbidden here is `None`, which would say *nobody recorded a way
/// in* about a check-in that plainly did record one. It is the same choice
/// `connections_of` makes when it cannot read a snapshot — an empty set, so
/// every `via` on it reads as unresolvable rather than as absent.
const UNREADABLE_VIA: Via = Via::Connection(Uuid::nil());

/// Reads one stored `via` back into the domain, re-validating its free text
/// through the same guard the WRITE path uses.
///
/// The re-parse mirrors `name`/`location`/`grid`/`signal_report`/`notes`
/// directly above the call sites — the callsign-reparse posture — and `via` has
/// the widest render surface of all of them: it reaches account-less viewers on
/// the public roster. What differs from those five is the FAILURE answer: they
/// raise a decode error, which for `via` would mean one bad stored string costs
/// a whole session's log. This degrades to [`UNREADABLE_VIA`] and logs, so the
/// bad text never renders anywhere and the rest of the log still reads.
fn via_from_stored(stored: &StoredVia) -> Via {
    let wire = match stored {
        StoredVia::Readable(wire) => wire,
        StoredVia::Unreadable(value) => {
            tracing::warn!(
                stored_via = %value,
                "session_events: a stored via this build cannot read; it renders as unresolvable"
            );
            return UNREADABLE_VIA;
        }
    };
    match via_from_wire(wire) {
        Via::Unlisted(text) => match parse_via_text(&text) {
            Ok(text) => Via::Unlisted(text),
            Err(error) => {
                tracing::warn!(
                    %error,
                    "session_events: a stored free-text via fails the current guard; \
                     it renders as unresolvable rather than reaching a surface unchecked"
                );
                UNREADABLE_VIA
            }
        },
        connection => connection,
    }
}

/// The default staying token for a historical `checkin.added` payload that
/// predates the field — `in-and-out`, the domain default.
fn default_staying_token() -> String {
    StayingStatus::default().as_str().to_owned()
}

/// The default source token for a historical `checkin.added` payload that
/// predates the field — `staff`, the domain default.
fn default_source_token() -> String {
    CheckInSource::default().as_str().to_owned()
}

/// The default precedence token for a historical `checkin.updated` payload that
/// predates the field — `routine`, the domain default.
fn default_precedence_token() -> String {
    Precedence::default().as_str().to_owned()
}

/// The `checkin.updated` payload: the FULL post-edit editable
/// field set. `name`/`location`/`signalReport`/`traffic` are omitted from JSON
/// when absent (the omit-optional-fields wire rule); `staying`/`precedence` are
/// always present. Corrections are NEVER carried here — the fold derives them.
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CheckinUpdatedPayload {
    check_in_id: Uuid,
    callsign: String,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    location: Option<String>,
    // The post-edit canonical Maidenhead grid, independent of
    // `location`. Same attribute pair, same two guarantees — see
    // [`CheckinAddedPayload::grid`].
    #[serde(skip_serializing_if = "Option::is_none", default)]
    grid: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    signal_report: Option<String>,
    #[serde(default = "default_staying_token")]
    staying: String,
    // The precedence kebab token. `#[serde(default)]` so an older
    // `checkin.updated` payload decodes to `routine` (the additive-compat
    // guarantee) rather than failing.
    #[serde(default = "default_precedence_token")]
    precedence: String,
    // The optional traffic count as a JSON number, omitted when absent.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    traffic: Option<i64>,
    // The per-station note, omitted when absent (the omit-optional
    // rule). A historical payload has no key → decodes to None.
    //
    // This key STAYS `notes` and now MEANS the STAFF note. It was
    // deliberately not renamed to `staffNote`: the log is append-only, so a
    // rename would need a decode alias on every historical event or a backfill,
    // and the careless version of it BLANKS every note written before the split
    // instead of migrating it to the private field. Keeping the key makes the
    // intended outcome — existing notes become staff notes, nothing published,
    // nothing blanked — fall out by construction.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    notes: Option<String>,
    // The per-station PUBLIC note, omitted when absent. A payload
    // written before the split has no key → decodes to None, which IS the
    // migration outcome.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    public_note: Option<String>,
    // The post-edit way in. Same shape and same two guarantees as
    // [`CheckinAddedPayload::via`]; an absent key is the historical case and the
    // "nobody recorded it" case alike, which are the same fact here.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    via: Option<StoredVia>,
    // The post-edit relaying station. Same shape and same two
    // guarantees as [`CheckinAddedPayload::relayed_by`]; an absent key is the
    // historical case and the "not relayed" case alike, which are the same fact
    // here.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    relayed_by: Option<String>,
}

/// The `station.worked-set` payload: the cursor target, or
/// `null` to clear the cursor. `checkInId` is PRESENT valued `null` on a clear
/// (NOT omitted) — the frontend reads `null` as "no station worked".
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct StationWorkedSetPayload {
    check_in_id: Option<Uuid>,
}

/// The `session.note-set` payload: the net-level note, OMITTED
/// when cleared (the omit-optional rule; an absent key means the note is cleared).
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SessionNoteSetPayload {
    #[serde(skip_serializing_if = "Option::is_none", default)]
    note: Option<String>,
}

/// The `roster.order-mode-set` payload: the standing ordering mode
/// as its lowercase-kebab token. Always PRESENT (there is no "cleared" state —
/// the legacy behaviour is its own token), but tolerant on the way back in: see
/// the decode arm.
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RosterOrderModeSetPayload {
    #[serde(default)]
    mode: Option<String>,
}

/// The `checkin.removed` payload: a bare tombstone.
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CheckinRemovedPayload {
    check_in_id: Uuid,
}

/// The `roster.reordered` payload: the explicit ordered list of
/// `check_in_id`s (a permutation). The fold applies it as a dumb projection.
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RosterReorderedPayload {
    order: Vec<Uuid>,
}

/// The `control.handed-off` payload: the new active NCS's
/// account id. Carried on BOTH the voluntary-handoff and involuntary-claim
/// paths (one kind). Operator id — the public wire redacts it to `{}`; this
/// is the OWNER/durable-log shape.
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ControlHandedOffPayload {
    new_ncs_account_id: Uuid,
}

/// The `station.blocked` payload: the blocked account id. This
/// is the OWNER/durable-log shape — the public wire redacts it to `{}`.
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct StationBlockedPayload {
    account_id: Uuid,
}

/// Reads a stored `definition_snapshot->'connections'` back into the wire shape a
/// `via` resolves against.
///
/// TOTAL: a session whose snapshot this version cannot read yields an EMPTY set,
/// so every `via` on it resolves to `Unresolvable` — which says so in words —
/// rather than costing the caller a whole page of history. The named refusal for
/// such a session still answers on that SESSION's own surfaces, which is where
/// an operator asks for that log; this is a LIST of many sessions.
///
/// **Per ELEMENT, not all-or-nothing, and never silently.** Decoding the whole
/// array with one `from_value::<Vec<_>>().ok()` meant a single malformed element
/// emptied the set, and then EVERY `via` on that session read "A way in this net
/// no longer lists" — indistinguishable from a connection the owner genuinely
/// deleted, with nothing anywhere saying which had happened. Each element is
/// read on its own so one bad row costs one label, and a skip is logged so the
/// degradation is visible in the operator's own logs. Found the same way.
fn connections_of(stored: Option<serde_json::Value>) -> Vec<NetConnectionWire> {
    let Some(value) = stored else {
        return Vec::new();
    };
    let elements = match serde_json::from_value::<Vec<serde_json::Value>>(value) {
        Ok(elements) => elements,
        Err(error) => {
            tracing::warn!(
                %error,
                "session_events: a stored connection set is not an array; every via on that \
                 session reads as unresolvable"
            );
            return Vec::new();
        }
    };
    elements
        .into_iter()
        .filter_map(
            |element| match serde_json::from_value::<NetConnectionWire>(element) {
                Ok(connection) => Some(connection),
                Err(error) => {
                    tracing::warn!(
                        %error,
                        "session_events: skipping one unreadable stored connection; a via naming \
                         it reads as unresolvable"
                    );
                    None
                }
            },
        )
        .collect()
}

/// Reads the `frequency.changed` rows a history read's lateral aggregated for
/// one check-in — `[{"seq": …, "payload": {…}}, …]`, already `ORDER BY seq` in
/// SQL — into the domain's ordered run.
///
/// SQL only SELECTS and ORDERS the moves; the as-at fold runs in Rust through
/// `frequencies_as_at`/`overlay_frequencies`, so the "as-at" logic has ONE home.
/// A last-write-wins done in SQL would be a second, private implementation of
/// the fold whose drift is invisible — `discovery.rs`'s `list_active_now`
/// docstring rules that out by name.
///
/// **Per ELEMENT, skip-and-warn, never `?` — the `connections_of` posture, for
/// the same reason.** Each payload goes through the module's own `body_from`,
/// so an older `frequency.changed` (no `connectionId`, refused on decode) is
/// skipped rather than translated. Such a move can only sit on
/// an older session, whose snapshot has no connection set either: the widget
/// never sees that session (the staff-entry exclusion) and the export resolves its `via`
/// against an EMPTY set. A move that cannot be read cannot affect a set that
/// cannot be read, and a data-rights export must not 500 over it. `None`
/// (no lateral row) is a session that never moved.
fn frequency_moves_of(stored: Option<serde_json::Value>) -> Vec<FrequencyMove> {
    #[derive(Deserialize)]
    struct StoredMove {
        seq: i64,
        payload: serde_json::Value,
    }
    let Some(value) = stored else {
        return Vec::new();
    };
    // Two decode steps on purpose: the ARRAY first, then each element on its
    // own. One `from_value::<Vec<StoredMove>>` would empty the whole run over a
    // single bad element — the exact failure `connections_of` was fixed for.
    let elements = match serde_json::from_value::<Vec<serde_json::Value>>(value) {
        Ok(elements) => elements,
        Err(error) => {
            tracing::warn!(
                %error,
                "session_events: a stored frequency-move run is not an array; the via on \
                 that check-in resolves against the planned frequency"
            );
            return Vec::new();
        }
    };
    elements
        .into_iter()
        .filter_map(|element| {
            let element = match serde_json::from_value::<StoredMove>(element) {
                Ok(element) => element,
                Err(error) => {
                    tracing::warn!(
                        %error,
                        "session_events: skipping one unreadable frequency-move row"
                    );
                    return None;
                }
            };
            let Ok(seq) = u64::try_from(element.seq) else {
                tracing::warn!(
                    seq = element.seq,
                    "session_events: skipping one frequency move with an out-of-range seq; a \
                     via on that check-in may resolve against an earlier frequency"
                );
                return None;
            };
            match body_from("frequency.changed", element.payload) {
                Ok(SessionEventBody::FrequencyChanged {
                    connection_id,
                    operating_frequency_hz,
                }) => Some(FrequencyMove {
                    seq,
                    connection_id,
                    operating_frequency_hz,
                }),
                Ok(_) => None,
                Err(error) => {
                    tracing::warn!(
                        %error,
                        seq,
                        "session_events: skipping one unreadable frequency move; a via on \
                         that check-in may resolve against an earlier frequency"
                    );
                    None
                }
            }
        })
        .collect()
}

/// The connection set a historical check-in's `via` resolves against: the
/// stored snapshot with every move up to and including `seq` overlaid (Ruling
/// 4b). The two history reads share it so they cannot disagree
/// about what "as at this check-in" means.
fn connections_as_at(
    stored_connections: Option<serde_json::Value>,
    stored_moves: Option<serde_json::Value>,
    seq: u64,
) -> Vec<NetConnectionWire> {
    overlay_frequencies(
        &connections_of(stored_connections),
        &frequencies_as_at(frequency_moves_of(stored_moves), seq),
    )
}

/// Maps an out-of-vocabulary or malformed stored value to a decode error (only
/// reachable if a non-Rust writer bypassed the domain — the `net_definitions`
/// enum-decode posture).
fn decode_error(what: &str) -> sqlx::Error {
    sqlx::Error::Decode(format!("session_events.{what} holds an unrepresentable value").into())
}

/// Maps a stored event payload this version can no longer read to a
/// DISTINGUISHABLE decode error — see
/// [`super::UnreplayableLog`].
fn unreplayable_log(kind: &str) -> sqlx::Error {
    sqlx::Error::Decode(Box::new(super::UnreplayableLog {
        what: format!("session_events.payload for {kind}"),
    }))
}

/// Serializes a body's own fields to the `payload` JSONB shape. Infallible:
/// every field is a plain scalar/uuid/string.
///
/// `pub` so both the atomic start composition in [`super::net_sessions`] and the
/// WebSocket `event` wire frame reuse this ONE camelCase serializer
/// — guaranteeing the streamed `event.payload` is byte-identical to the stored
/// `session_events.payload` for that kind (wire/DB parity).
pub fn to_payload(body: &SessionEventBody) -> serde_json::Value {
    match body {
        SessionEventBody::SessionStarted {
            definition_id,
            definition_version,
        } => serde_json::to_value(SessionStartedPayload {
            definition_id: *definition_id,
            definition_version: *definition_version,
        })
        .expect("session.started payload serializes"),
        SessionEventBody::FrequencyChanged {
            connection_id,
            operating_frequency_hz,
        } => serde_json::to_value(FrequencyChangedPayload {
            connection_id: *connection_id,
            operating_frequency_hz: *operating_frequency_hz,
        })
        .expect("frequency.changed payload serializes"),
        SessionEventBody::CheckinAdded {
            check_in_id,
            callsign,
            client_event_id,
            signal_report,
            staying,
            name,
            location,
            grid,
            source,
            via,
            relayed_by,
        } => serde_json::to_value(CheckinAddedPayload {
            check_in_id: *check_in_id,
            callsign: callsign.as_str().to_owned(),
            client_event_id: *client_event_id,
            signal_report: signal_report.as_ref().map(|r| r.as_str().to_owned()),
            staying: staying.as_str().to_owned(),
            name: name.as_ref().map(|n| n.as_str().to_owned()),
            location: location.as_ref().map(|l| l.as_str().to_owned()),
            grid: grid.as_ref().map(|g| g.as_str().to_owned()),
            source: source.as_str().to_owned(),
            via: via.as_ref().map(|v| StoredVia::Readable(via_of(v))),
            relayed_by: relayed_by.as_ref().map(|c| c.as_str().to_owned()),
        })
        .expect("checkin.added payload serializes"),
        SessionEventBody::CheckinUpdated {
            check_in_id,
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
        } => serde_json::to_value(CheckinUpdatedPayload {
            check_in_id: *check_in_id,
            callsign: callsign.as_str().to_owned(),
            name: name.as_ref().map(|n| n.as_str().to_owned()),
            location: location.as_ref().map(|l| l.as_str().to_owned()),
            grid: grid.as_ref().map(|g| g.as_str().to_owned()),
            signal_report: signal_report.as_ref().map(|r| r.as_str().to_owned()),
            staying: staying.as_str().to_owned(),
            precedence: precedence.as_str().to_owned(),
            traffic: traffic.map(|t| t.get() as i64),
            notes: notes.as_ref().map(|n| n.as_str().to_owned()),
            public_note: public_note.as_ref().map(|n| n.as_str().to_owned()),
            via: via.as_ref().map(|v| StoredVia::Readable(via_of(v))),
            relayed_by: relayed_by.as_ref().map(|c| c.as_str().to_owned()),
        })
        .expect("checkin.updated payload serializes"),
        SessionEventBody::CheckinRemoved { check_in_id } => {
            serde_json::to_value(CheckinRemovedPayload {
                check_in_id: *check_in_id,
            })
            .expect("checkin.removed payload serializes")
        }
        SessionEventBody::RosterReordered { order } => {
            serde_json::to_value(RosterReorderedPayload {
                order: order.clone(),
            })
            .expect("roster.reordered payload serializes")
        }
        SessionEventBody::StationWorkedSet { check_in_id } => {
            serde_json::to_value(StationWorkedSetPayload {
                check_in_id: *check_in_id,
            })
            .expect("station.worked-set payload serializes")
        }
        SessionEventBody::SessionNoteSet { note } => serde_json::to_value(SessionNoteSetPayload {
            note: note.as_ref().map(|n| n.as_str().to_owned()),
        })
        .expect("session.note-set payload serializes"),
        SessionEventBody::RosterOrderModeSet { mode } => {
            serde_json::to_value(RosterOrderModeSetPayload {
                mode: Some(mode.as_str().to_owned()),
            })
            .expect("roster.order-mode-set payload serializes")
        }
        SessionEventBody::SessionClosed => serde_json::json!({}),
        // The stall/resume transitions are payload-free (the fact IS
        // the transition; the envelope carries when/who).
        SessionEventBody::NcsStalled | SessionEventBody::NcsResumed => serde_json::json!({}),
        SessionEventBody::ControlHandedOff { new_ncs_account_id } => {
            serde_json::to_value(ControlHandedOffPayload {
                new_ncs_account_id: *new_ncs_account_id,
            })
            .expect("control.handed-off payload serializes")
        }
        SessionEventBody::StationBlocked { account_id } => {
            serde_json::to_value(StationBlockedPayload {
                account_id: *account_id,
            })
            .expect("station.blocked payload serializes")
        }
    }
}

/// Reconstructs a body from its stored `kind` token and `payload`, dispatching
/// on `kind` (the single vocabulary owned by [`SessionEventBody::kind`]).
fn body_from(kind: &str, payload: serde_json::Value) -> Result<SessionEventBody, sqlx::Error> {
    match kind {
        // These two payload shapes changed, and the OLD shapes are
        // REFUSED rather than translated. The
        // refusal carries `UnreplayableLog` so the HTTP layer can answer 410
        // with a `detail` that names the fault, instead of the generic 500 that
        // `decode_error` becomes.
        "session.started" => {
            // The pre-Epic-16 shape, and the ONLY one that is permanently
            // unreadable: it carried a session-level `operatingFrequencyHz`.
            // Any OTHER decode failure here is corruption or a writer
            // regression, which is a plain decode error — telling an operator
            // "nothing you do will bring it back" about a row a roll-forward
            // would restore is its own confident wrong answer.
            let predates_the_connection_set = payload
                .as_object()
                .is_some_and(|object| object.contains_key("operatingFrequencyHz"));
            let p: SessionStartedPayload = serde_json::from_value(payload).map_err(|_| {
                if predates_the_connection_set {
                    unreplayable_log("session.started")
                } else {
                    decode_error("payload")
                }
            })?;
            Ok(SessionEventBody::SessionStarted {
                definition_id: p.definition_id,
                definition_version: p.definition_version,
            })
        }
        "frequency.changed" => {
            // The pre-Epic-16 shape named no connection; see the arm above for
            // why every other failure is a plain decode error.
            let predates_the_connection_set = payload
                .as_object()
                .is_some_and(|object| !object.contains_key("connectionId"));
            let p: FrequencyChangedPayload = serde_json::from_value(payload).map_err(|_| {
                if predates_the_connection_set {
                    unreplayable_log("frequency.changed")
                } else {
                    decode_error("payload")
                }
            })?;
            Ok(SessionEventBody::FrequencyChanged {
                connection_id: p.connection_id,
                operating_frequency_hz: p.operating_frequency_hz,
            })
        }
        "checkin.added" => {
            let p: CheckinAddedPayload =
                serde_json::from_value(payload).map_err(|_| decode_error("payload"))?;
            let callsign = parse_callsign(&p.callsign).map_err(|_| decode_error("callsign"))?;
            // Re-validate the stored report through the domain guard (the
            // callsign-reparse posture); a non-empty stored value yields Some.
            let signal_report = match p.signal_report {
                Some(s) => parse_signal_report(&s).map_err(|_| decode_error("signal_report"))?,
                None => None,
            };
            let staying =
                StayingStatus::try_from(p.staying.as_str()).map_err(|_| decode_error("staying"))?;
            // Re-validate the stored name/location through the domain guards (the
            // callsign-reparse posture); an absent key is None.
            let name = match p.name {
                Some(s) => parse_name(&s).map_err(|_| decode_error("name"))?,
                None => None,
            };
            let location = match p.location {
                Some(s) => parse_location(&s).map_err(|_| decode_error("location"))?,
                None => None,
            };
            // Re-validate the stored grid through the SAME domain
            // grammar the write path used; an absent key is None.
            let grid = match p.grid {
                Some(s) => Some(parse_grid(&s).map_err(|_| decode_error("grid"))?),
                None => None,
            };
            // Re-validate the stored source token; a historical payload with no
            // key decoded to `staff` via the serde default.
            let source =
                CheckInSource::try_from(p.source.as_str()).map_err(|_| decode_error("source"))?;
            Ok(SessionEventBody::CheckinAdded {
                check_in_id: p.check_in_id,
                callsign,
                client_event_id: p.client_event_id,
                signal_report,
                staying,
                name,
                location,
                grid,
                source,
                // An absent key decodes to `None` — "nobody recorded
                // it" — exactly like `grid` and the notes before it. Nothing is
                // re-validated against the session's connection set here: a
                // `via` naming a connection the snapshot has since lost is a
                // READABLE record that renders as the unresolvable phrase, not a
                // corrupt one, and refusing it would make one deleted connection
                // cost a whole session's log.
                via: p.via.as_ref().map(via_from_stored),
                // Re-validated through the SAME `parse_callsign`
                // grammar the write path used — the callsign-reparse posture
                // every other callsign on this read already has. An absent key
                // decodes to `None`, which means "not relayed".
                relayed_by: match p.relayed_by {
                    Some(ref call) => {
                        Some(parse_callsign(call).map_err(|_| decode_error("relayed_by"))?)
                    }
                    None => None,
                },
            })
        }
        "checkin.updated" => {
            let p: CheckinUpdatedPayload =
                serde_json::from_value(payload).map_err(|_| decode_error("payload"))?;
            let callsign = parse_callsign(&p.callsign).map_err(|_| decode_error("callsign"))?;
            let name = match p.name {
                Some(s) => parse_name(&s).map_err(|_| decode_error("name"))?,
                None => None,
            };
            let location = match p.location {
                Some(s) => parse_location(&s).map_err(|_| decode_error("location"))?,
                None => None,
            };
            // Mirrors the `checkin.added` re-validation above.
            let grid = match p.grid {
                Some(s) => Some(parse_grid(&s).map_err(|_| decode_error("grid"))?),
                None => None,
            };
            let signal_report = match p.signal_report {
                Some(s) => parse_signal_report(&s).map_err(|_| decode_error("signal_report"))?,
                None => None,
            };
            let staying =
                StayingStatus::try_from(p.staying.as_str()).map_err(|_| decode_error("staying"))?;
            let precedence = Precedence::try_from(p.precedence.as_str())
                .map_err(|_| decode_error("precedence"))?;
            let traffic = parse_traffic_count(p.traffic).map_err(|_| decode_error("traffic"))?;
            let notes = match p.notes {
                Some(s) => parse_note(&s).map_err(|_| decode_error("notes"))?,
                None => None,
            };
            // The SAME shared guard, on the SAME read path. An absent
            // key is the historical case and decodes to None.
            let public_note = match p.public_note {
                Some(s) => parse_note(&s).map_err(|_| decode_error("public_note"))?,
                None => None,
            };
            Ok(SessionEventBody::CheckinUpdated {
                check_in_id: p.check_in_id,
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
                via: p.via.as_ref().map(via_from_stored),
                // Mirrors the `checkin.added` re-validation above.
                relayed_by: match p.relayed_by {
                    Some(ref call) => {
                        Some(parse_callsign(call).map_err(|_| decode_error("relayed_by"))?)
                    }
                    None => None,
                },
            })
        }
        "checkin.removed" => {
            let p: CheckinRemovedPayload =
                serde_json::from_value(payload).map_err(|_| decode_error("payload"))?;
            Ok(SessionEventBody::CheckinRemoved {
                check_in_id: p.check_in_id,
            })
        }
        "roster.reordered" => {
            let p: RosterReorderedPayload =
                serde_json::from_value(payload).map_err(|_| decode_error("payload"))?;
            Ok(SessionEventBody::RosterReordered { order: p.order })
        }
        "station.worked-set" => {
            let p: StationWorkedSetPayload =
                serde_json::from_value(payload).map_err(|_| decode_error("payload"))?;
            Ok(SessionEventBody::StationWorkedSet {
                check_in_id: p.check_in_id,
            })
        }
        "session.note-set" => {
            let p: SessionNoteSetPayload =
                serde_json::from_value(payload).map_err(|_| decode_error("payload"))?;
            let note = match p.note {
                Some(s) => parse_note(&s).map_err(|_| decode_error("note"))?,
                None => None,
            };
            Ok(SessionEventBody::SessionNoteSet { note })
        }
        "roster.order-mode-set" => {
            let p: RosterOrderModeSetPayload =
                serde_json::from_value(payload).map_err(|_| decode_error("payload"))?;
            // Deliberately TOLERANT where the other enum-bearing kinds are
            // strict: the mode is inert display intent, so an absent or
            // out-of-vocabulary token degrades to the legacy default rather than
            // making a whole session's log undecodable. A wrong ORDER is
            // impossible either way — order lives in `roster.reordered`.
            let mode = p
                .mode
                .as_deref()
                .and_then(|token| RosterOrderMode::try_from(token).ok())
                .unwrap_or_default();
            Ok(SessionEventBody::RosterOrderModeSet { mode })
        }
        "session.closed" => Ok(SessionEventBody::SessionClosed),
        "ncs.stalled" => Ok(SessionEventBody::NcsStalled),
        "ncs.resumed" => Ok(SessionEventBody::NcsResumed),
        "control.handed-off" => {
            let p: ControlHandedOffPayload =
                serde_json::from_value(payload).map_err(|_| decode_error("payload"))?;
            Ok(SessionEventBody::ControlHandedOff {
                new_ncs_account_id: p.new_ncs_account_id,
            })
        }
        "station.blocked" => {
            let p: StationBlockedPayload =
                serde_json::from_value(payload).map_err(|_| decode_error("payload"))?;
            Ok(SessionEventBody::StationBlocked {
                account_id: p.account_id,
            })
        }
        _ => Err(decode_error("kind")),
    }
}

/// Appends one event on a borrowed connection, assigning the next per-session
/// `seq` under the row lock that `UPDATE... WHERE id` takes (monotonic,
/// gapless, 1-indexed, duplicate-free under concurrency). The event's
/// `at` is stored as `created_at` (NOT `now()`). A missing session is
/// `RowNotFound`, never a silent success.
///
/// The transaction-taking core atomic start/close compose in ONE
/// outer transaction with the `net_sessions` INSERT/UPDATE; [`SessionEventLog::append`]
/// is the thin `begin → core → commit` wrapper existing callers keep using. The
/// seq bump + INSERT commit together with the caller's transaction, so a
/// rollback of either reverts the counter (gapless).
pub(crate) async fn append_in_tx(
    conn: &mut sqlx::PgConnection,
    session_id: Uuid,
    body: &SessionEventBody,
    actor_id: Option<Uuid>,
    at_millis: u64,
) -> Result<SessionEvent, sqlx::Error> {
    let at = utc_from_millis(at_millis);

    let bumped = sqlx::query!(
        "UPDATE net_sessions SET last_seq = last_seq + 1, updated_at = $2
         WHERE id = $1
         RETURNING last_seq",
        session_id,
        at,
    )
    .fetch_optional(&mut *conn)
    .await?
    .ok_or(sqlx::Error::RowNotFound)?;
    let seq = bumped.last_seq;

    let payload = to_payload(body);
    sqlx::query!(
        "INSERT INTO session_events (id, session_id, seq, kind, payload, actor, created_at)
         VALUES ($1, $2, $3, $4, $5, $6, $7)",
        Uuid::now_v7(),
        session_id,
        seq,
        body.kind(),
        payload,
        actor_id,
        at,
    )
    .execute(&mut *conn)
    .await?;

    // `last_seq` only ever counts up from 0 (Postgres bigint arithmetic
    // errors on overflow rather than wrapping), so this is always in-range —
    // but a checked conversion states that invariant instead of leaning on an
    // unchecked `as` cast at the seq/u64 boundary.
    let seq = u64::try_from(seq).map_err(|_| decode_error("last_seq"))?;

    Ok(SessionEvent {
        seq,
        actor_id,
        at: at_millis,
        body: body.clone(),
    })
}

/// Reads a session's whole ordered event log on a BORROWED connection — the
/// in-transaction sibling of [`SessionEventLog::events_since`].
/// The version-CAS read MUST run on the same connection inside the append
/// transaction so the `net_sessions` row lock (taken by the guarded live-gate)
/// covers the read → fold → compare → append as one atomic critical section;
/// re-folding the log is acceptable at this scale.
pub(crate) async fn events_since_in_tx(
    conn: &mut sqlx::PgConnection,
    session_id: Uuid,
) -> Result<Vec<SessionEvent>, sqlx::Error> {
    // The query text is byte-identical to [`SessionEventLog::events_since`]
    // (reusing its compiled `.sqlx` cache entry — no new offline query): `seq`
    // is 1-indexed, so a `seq > 0` bound returns the whole log.
    let rows = sqlx::query!(
        "SELECT seq, kind, payload, actor, created_at
             FROM session_events
             WHERE session_id = $1 AND seq > $2
             ORDER BY seq ASC",
        session_id,
        0i64,
    )
    .fetch_all(&mut *conn)
    .await?;

    let mut events = Vec::with_capacity(rows.len());
    for row in rows {
        let seq = u64::try_from(row.seq).map_err(|_| decode_error("seq"))?;
        let body = body_from(&row.kind, row.payload)?;
        events.push(SessionEvent {
            seq,
            actor_id: row.actor,
            at: millis_from_utc(row.created_at),
            body,
        });
    }
    Ok(events)
}

impl SessionEventLog {
    /// Wraps the shared connection pool.
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    /// Appends one event to a session's log in its own transaction (the thin
    /// `begin → core → commit` wrapper over [`append_in_tx`]). Returns the
    /// minted [`SessionEvent`]. The
    /// atomic-composition core is factored out for the start/close paths.
    pub async fn append(
        &self,
        session_id: Uuid,
        body: &SessionEventBody,
        actor_id: Option<Uuid>,
        at_millis: u64,
    ) -> Result<SessionEvent, sqlx::Error> {
        let mut tx = self.pool.begin().await?;
        let event = append_in_tx(&mut tx, session_id, body, actor_id, at_millis).await?;
        tx.commit().await?;
        Ok(event)
    }

    /// Reads a session's events with `seq > since`, ascending — the ordered
    /// input `fold`/`replay` require (they do NOT sort). `since = 0`
    /// returns the whole log; it is the delta seam resume endpoint
    /// consumes.
    pub async fn events_since(
        &self,
        session_id: Uuid,
        since: u64,
    ) -> Result<Vec<SessionEvent>, sqlx::Error> {
        // `since` beyond i64::MAX cannot correspond to any real seq (`seq` is
        // bigint-backed and never exceeds i64::MAX) — reject it explicitly
        // rather than letting an unchecked `as i64` cast wrap negative and
        // silently widen the query to "all events".
        let since_i64 = i64::try_from(since).map_err(|_| decode_error("since"))?;

        let rows = sqlx::query!(
            "SELECT seq, kind, payload, actor, created_at
             FROM session_events
             WHERE session_id = $1 AND seq > $2
             ORDER BY seq ASC",
            session_id,
            since_i64,
        )
        .fetch_all(&self.pool)
        .await?;

        let mut events = Vec::with_capacity(rows.len());
        for row in rows {
            let seq = u64::try_from(row.seq).map_err(|_| decode_error("seq"))?;
            let body = body_from(&row.kind, row.payload)?;
            events.push(SessionEvent {
                seq,
                actor_id: row.actor,
                at: millis_from_utc(row.created_at),
                body,
            });
        }
        Ok(events)
    }

    /// Reads the account's own SELF check-in history for the personal-data
    /// export. Returns every session where the
    /// account checked ITSELF in — `actor = $1 AND source = "self"` — with the
    /// session's snapshotted net title and the check-in fields, oldest first.
    ///
    /// The `actor = $1 AND kind = 'checkin.added'` SQL filter (served by the
    /// `idx_session_events_actor` partial index) narrows to every check-in the
    /// account AUTHORED; the correctness-critical step is the in-Rust
    /// `source == SelfService` discard that follows. For a STAFF-entered add,
    /// `actor` is the OPERATOR who logged it and the checked-in station is only
    /// a bare callsign string with no account link — so filtering on `actor`
    /// alone would leak the account's staff logging of OTHER people's callsigns
    /// into its OWN export (a real privacy bug). The `source` discrimination is
    /// done in Rust (deserialize-then-decide), matching the codebase convention
    /// that every payload read deserializes before inspecting fields — there is
    /// no `payload->>'...'` SQL predicate anywhere in the tree.
    pub async fn self_check_ins(&self, account_id: Uuid) -> Result<Vec<SelfCheckIn>, sqlx::Error> {
        let rows = sqlx::query!(
            // The snapshot's connection SET rides along so a
            // check-in's `via` can be resolved to a label in Rust, through the
            // one shared resolver. Resolving it in SQL would grow a second
            // answer to "what is this connection called" in a language that
            // cannot share the first.
            //
            // ⚠️ RESOLVED AS AT THIS CHECK-IN'S OWN SEQ — NOT `live_connections`
            // and NOT the bare frozen snapshot ("if the
            // net changes frequency after traffic is passed, the worked stations
            // should not change"). The live surfaces overlay every
            // `frequency.changed` because they answer "where is this net NOW";
            // this answers "which way in did THIS check-in come in on", months
            // later. The LAST frequency would tell everyone who checked in
            // before a 14.230 → 14.250 move that they were on 14.250; the
            // PLANNED one (what this read once did) is right only for a
            // net that never moved — a different wrong answer, not a lesser
            // one. A review prescribed `live_connections`; the
            // divergence it saw was real and the prescription would have made
            // it worse.
            //
            // The shape: the shipped query is WRAPPED as `page`, textually
            // unchanged except that it also selects `se.seq`, and a LATERAL
            // outside it aggregates that session's `frequency.changed` rows with
            // `seq <= page.seq`, in seq order. SQL only selects and orders the
            // moves; the as-at fold runs in Rust through the domain's own
            // helpers (`frequency_moves_of`/`connections_as_at`), never as a
            // last-write-wins in SQL. Served by the `UNIQUE (session_id, seq)`
            // constraint's index; no new index.
            r#"SELECT page.session_id AS "session_id!",
                      page.net_title AS "net_title!",
                      page.connections AS "connections?",
                      page.seq AS "seq!",
                      page.kind AS "kind!",
                      page.payload AS "payload!",
                      page.created_at AS "created_at!",
                      moves.moves AS "moves?"
               FROM (SELECT se.session_id,
                            ns.definition_snapshot->>'title' AS net_title,
                            ns.definition_snapshot->'connections' AS connections,
                            se.seq,
                            se.kind,
                            se.payload,
                            se.created_at
                       FROM session_events se
                       JOIN net_sessions ns ON ns.id = se.session_id
                      WHERE se.actor = $1 AND se.kind = 'checkin.added'
                      ORDER BY se.created_at) AS page
               LEFT JOIN LATERAL (
                   SELECT jsonb_agg(jsonb_build_object('seq', fe.seq, 'payload', fe.payload)
                                    ORDER BY fe.seq) AS moves
                     FROM session_events fe
                    WHERE fe.session_id = page.session_id
                      AND fe.kind = 'frequency.changed'
                      AND fe.seq <= page.seq
               ) AS moves ON true
               ORDER BY page.created_at"#,
            account_id,
        )
        .fetch_all(&self.pool)
        .await?;

        let mut out = Vec::new();
        for row in rows {
            let seq = u64::try_from(row.seq).map_err(|_| decode_error("seq"))?;
            let body = body_from(&row.kind, row.payload)?;
            // `via` joins the fields this export carries. It is the
            // surface that already tells the truth about rows the profile widget
            // hides, and a `via` omitted here would be personal data the account
            // holder cannot get a copy of. The `..` that hid it is opened.
            // EXHAUSTIVE `_`-bound destructure, no `..` rest pattern: this is
            // the surface that already
            // tells the truth about rows the profile widget hides, so a new
            // per-check-in field silently missing from a subject's own data
            // export is exactly the failure to make loud. The compile error a
            // new field causes here is the guard working.
            //
            // NOT CARRIED: `check_in_id`/`grid` are already carried by other
            // reads of the same rows and add nothing here (`grid` is the
            // account's own profile value, which the profile export carries
            // directly); `client_event_id` is the caller's idempotency token,
            // not a fact about the station.
            let SessionEventBody::CheckinAdded {
                callsign,
                signal_report,
                staying,
                name,
                location,
                source,
                via,
                // NEVER carried, and the reason is STRUCTURAL rather
                // than editorial. This read returns `source = 'self'` rows only
                // (the discard below), and the participant self add path 403s on
                // the mere PRESENCE of a `relayedBy` — so no row this query can
                // return has one. A future story that makes relay settable on
                // the self path, or that drops the `source` filter, invalidates
                // this reasoning and not merely its conclusion.
                relayed_by: _,
                check_in_id: _,
                grid: _,
                client_event_id: _,
            } = body
            else {
                // Unreachable given the `kind = 'checkin.added'` SQL filter, but
                // stated rather than assumed.
                continue;
            };
            // The correctness-critical discard: a staff-entered add is NOT the
            // account's own check-in history.
            if source != CheckInSource::SelfService {
                continue;
            }
            out.push(SelfCheckIn {
                session_id: row.session_id,
                net_title: row.net_title,
                callsign,
                signal_report,
                staying,
                name,
                location,
                // Against the set as it stood at THIS row's seq.
                via: via_label(&resolve_via(
                    via.as_ref(),
                    &connections_as_at(row.connections, row.moves, seq),
                )),
                checked_in_at_millis: millis_from_utc(row.created_at),
            });
        }
        Ok(out)
    }

    /// One page of the account's OWN check-in history, newest first — the
    /// profile widget's read.
    ///
    /// Ownership is the same two-part composite [`authz::owns_check_in`] states
    /// and [`SessionEventLog::self_check_ins`] applies: `actor = $1` (the
    /// account that CAUSED the entry, an envelope COLUMN — the payload carries
    /// no account link at all) **and** `source = 'self'` (the actor IS the
    /// checked-in station, not an operator who typed someone else's callsign).
    /// Both are required together: `actor` alone returns an NCS's staff logging
    /// of OTHER people's callsigns as if it were their own history, which is the
    /// privacy bug `idx_session_events_actor` was written to prevent.
    ///
    /// # Why `source` is filtered in SQL here and in Rust there
    ///
    /// [`SessionEventLog::self_check_ins`] is unbounded, so discarding rows
    /// after the query costs nothing but a shorter `Vec`. This read is
    /// keyset-paginated, and [`into_page`]'s over-fetch probe decides "is there
    /// another page?" from `rows.len() > limit` — a post-query discard would let
    /// a page come back short, or empty WITH a next cursor, so "load more" would
    /// offer itself while returning nothing. The two predicates are exactly
    /// equivalent: an older payload with no `source` key is SQL `NULL`, and
    /// `NULL = 'self'` is not true, matching the Rust default that folds an
    /// absent `source` to `CheckInSource::Staff`.
    ///
    /// Keyset over `(created_at, id)`, not offset: the log only grows at the
    /// newest end, and the `id` tiebreak is what keeps the walk total when two
    /// check-ins share a millisecond.
    ///
    /// # A check-in whose session predates the connection set
    ///
    /// It is EXCLUDED from this widget, not refused. Refusing killed the whole
    /// page — and, because the walk is keyset, everything older than it too —
    /// over one old row among many. The named 410 still answers on that
    /// session's own surfaces, which is where an operator asks for that log.
    ///
    /// ⚠️ It is therefore INVISIBLE HERE while still present in
    /// [`Self::self_check_ins`], the personal-data export, which reads the same
    /// rows and carries no band or mode to be unable to source. The two surfaces
    /// disagree about whether the check-in exists, and the export is the one
    /// telling the truth. Reconciling them means giving this widget a way to
    /// render a check-in whose net's ways in are unknown — distinct from an
    /// internet-only net, whose band is legitimately `null` — and that is a wire
    /// change no criterion here asked for.
    pub async fn self_check_ins_page(
        &self,
        account_id: Uuid,
        limit: usize,
        cursor: Option<PageCursor>,
    ) -> Result<Page<SelfCheckInEntry>, sqlx::Error> {
        let before_at = cursor.map(|c| utc_from_millis(c.at_millis));
        let before_id = cursor.map(|c| c.id);
        let rows = sqlx::query!(
            // The whole connection SET is selected and `band`/`mode`
            // are derived IN RUST from the entry's own `via`, through the one
            // shared resolver. An earlier read took `->'connections'->0->>'band'`
            // here, which is a fact about the NET rather than about the
            // check-in: on a cross-mode net it told every EchoLink participant
            // they had been on 20m. `AS "connections?"` — the snapshot of a
            // session this version cannot read has none, and the WHERE clause
            // below already excludes those rows.
            //
            // A check-in whose session predates the connection set is EXCLUDED,
            // in SQL, alongside the `source` predicate and for the same reason
            // the module doc gives for that one: this read is keyset-paginated,
            // and a post-query discard would let a page come back short — or
            // empty WITH a next cursor, so "load more" offers itself and returns
            // nothing. Excluded rather than refused: the refusal belongs on that
            // SESSION's own surfaces, where an operator asked for that log. This
            // is a LIST of many sessions, and one unreadable member of it must
            // not cost a reader the whole page and everything older than it. The
            // `jsonb_typeof` guard is what keeps the length call total — an
            // object or a bare `null` under that key would raise, not skip — and
            // `> 0` refuses the empty array too, which is a shape
            // `connection_set_from_wire` refuses and which would otherwise report
            // a confident "this net had no band".
            //
            // The shipped keyset query is WRAPPED as `page` — its
            // WHERE, ORDER BY and LIMIT byte-for-byte, plus `se.seq` — and the
            // LATERAL sits OUTSIDE it, so the moves are aggregated for at most
            // `limit + 1` rows and the staff-entry exclusion, the `source` predicate and
            // the cursor are untouched (the two membership predicates are NOT
            // not edited here). The outer ORDER BY restates the inner one
            // because a join does not promise to preserve its input's order.
            // See `self_check_ins` for why the fold runs in Rust and never as a
            // last-write-wins in SQL.
            r#"SELECT page.id AS "id!",
                      page.session_id AS "session_id!",
                      page.seq AS "seq!",
                      page.created_at AS "created_at!",
                      page.payload AS "payload!",
                      page.net_title AS "net_title!",
                      page.connections AS "connections?",
                      moves.moves AS "moves?"
               FROM (SELECT se.id,
                            se.session_id,
                            se.seq,
                            se.created_at,
                            se.payload,
                            ns.definition_snapshot->>'title' AS net_title,
                            ns.definition_snapshot->'connections' AS connections
                       FROM session_events se
                       JOIN net_sessions ns ON ns.id = se.session_id
                      WHERE se.actor = $1
                        AND se.kind = 'checkin.added'
                        AND se.payload->>'source' = 'self'
                        AND jsonb_typeof(ns.definition_snapshot->'connections') = 'array'
                        AND jsonb_array_length(ns.definition_snapshot->'connections') > 0
                        AND ($2::timestamptz IS NULL OR (se.created_at, se.id) < ($2, $3))
                      ORDER BY se.created_at DESC, se.id DESC
                      LIMIT $4) AS page
               LEFT JOIN LATERAL (
                   SELECT jsonb_agg(jsonb_build_object('seq', fe.seq, 'payload', fe.payload)
                                    ORDER BY fe.seq) AS moves
                     FROM session_events fe
                    WHERE fe.session_id = page.session_id
                      AND fe.kind = 'frequency.changed'
                      AND fe.seq <= page.seq
               ) AS moves ON true
               ORDER BY page.created_at DESC, page.id DESC"#,
            account_id,
            before_at,
            before_id,
            // Over-fetch by one to learn whether a further page exists.
            (limit as i64) + 1,
        )
        .fetch_all(&self.pool)
        .await?;

        let mut entries = Vec::with_capacity(rows.len());
        for row in rows {
            let seq = u64::try_from(row.seq).map_err(|_| decode_error("seq"))?;
            let body = body_from("checkin.added", row.payload)?;
            // The `..` that hid `via` from this read is opened.
            // EXHAUSTIVE `_`-bound destructure, no `..` rest pattern, for the
            // same reason as `self_check_ins` above: the widget's row is a
            // projection and a new field joins it by decision, not by default.
            // Everything below the two carried fields is either already on the
            // row from the SQL envelope (`check_in_id` is `row.id`) or is not
            // this widget's to show — `signal_report`/`name`/`location`/`grid`
            // are the account's own profile data, shown on the profile page
            // itself, and `client_event_id` is a write-path token.
            let SessionEventBody::CheckinAdded {
                callsign,
                via,
                // NEVER carried, for the same STRUCTURAL reason
                // `self_check_ins` above states — this query filters
                // `payload->>'source' = 'self'` in SQL, and the self add path
                // 403s a present `relayedBy`, so no row here can hold one.
                relayed_by: _,
                check_in_id: _,
                signal_report: _,
                staying: _,
                name: _,
                location: _,
                grid: _,
                source: _,
                client_event_id: _,
            } = body
            else {
                // Unreachable given the `kind = 'checkin.added'` SQL filter, but
                // stated rather than assumed.
                continue;
            };
            // As at THIS row's own seq, not `live_connections` and not the bare
            // frozen snapshot — the same decision, for the same reason, as
            // `self_check_ins` above states at length.
            // This row is a historical fact, not a live one.
            let connections = connections_as_at(row.connections, row.moves, seq);
            let display = resolve_via(via.as_ref(), &connections);
            // Band and mode come from the connection THIS check-in arrived on
            // and from nowhere else. An unrecorded, unresolvable or free-text
            // `via` reports neither rather than borrowing position zero's — the
            // substitution is the defect this removes, and the
            // widget already renders an absent band (an internet-only net has
            // none). `via` below is what such a row actually has to say.
            // THREE arms spelled out, no `_` catch-all: a fifth `ViaDisplay`
            // variant must red every consumer, which is the whole reason that
            // type has no `Default` and no `unwrap_or` shape. A wildcard here
            // would silently fold a new fact into "no band, no mode".
            let connection = match display {
                ViaDisplay::Resolved(connection) => Some(connection),
                // Nobody recorded a way in, so there is no band to report and
                // the export fallback is deliberately NOT reached: it is an
                // ADIF-only decision and this is a rendered surface.
                ViaDisplay::NotRecorded => None,
                // A way in this session no longer lists — `via` below says so
                // in words; borrowing position zero's band would not.
                ViaDisplay::Unresolvable => None,
                // The operator's own words for a way the net never listed, so
                // there is no connection to take a band from.
                ViaDisplay::Unlisted(_) => None,
            };
            entries.push(SelfCheckInEntry {
                id: row.id,
                session_id: row.session_id,
                net_title: row.net_title,
                band: connection.and_then(|c| c.band.clone()),
                mode: connection.and_then(|c| c.mode.clone()),
                via: via_label(&display),
                callsign,
                checked_in_at_millis: millis_from_utc(row.created_at),
            });
        }
        Ok(into_page(entries, limit, |e| {
            (e.checked_in_at_millis, e.id)
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use netroll_domain::check_in::{parse_location, parse_name, parse_signal_report};
    use netroll_domain::net::wire::UNRESOLVABLE_VIA_LABEL;
    use netroll_domain::profile::parse_grid;

    fn call(s: &str) -> netroll_domain::callsign::Callsign {
        parse_callsign(s).expect("valid callsign")
    }

    #[test]
    fn checkin_updated_payload_round_trips_with_camelcase_and_omitted_optionals() {
        let body = SessionEventBody::CheckinUpdated {
            check_in_id: Uuid::from_u128(42),
            callsign: call("W1AX"),
            name: parse_name("Maria").expect("valid").filter(|_| true),
            location: None,
            grid: None,
            signal_report: parse_signal_report("599").expect("valid"),
            staying: StayingStatus::StayingForComments,
            precedence: Precedence::Routine,
            traffic: None,
            notes: None,
            public_note: None,
            via: None,
            relayed_by: None,
        };
        let payload = to_payload(&body);
        // camelCase keys; absent `location` OMITTED (not null); staying always present.
        assert_eq!(payload["checkInId"], Uuid::from_u128(42).to_string());
        assert_eq!(payload["callsign"], "W1AX");
        assert_eq!(payload["name"], "Maria");
        assert!(payload.as_object().expect("obj").get("location").is_none());
        assert_eq!(payload["signalReport"], "599");
        assert_eq!(payload["staying"], "staying-for-comments");
        // Round-trips back to the identical body.
        let decoded = body_from("checkin.updated", payload).expect("decodes");
        assert_eq!(decoded, body);
    }

    #[test]
    fn checkin_added_name_and_location_round_trip_with_omit_optional_and_historical_none() {
        // `name`/`location` ride checkin.added, OMITTED when
        // absent (the same omit-optional rule signalReport uses), and
        // an older payload with no keys decodes to None.
        let with_identity = SessionEventBody::CheckinAdded {
            check_in_id: Uuid::from_u128(70),
            callsign: call("W1AW"),
            client_event_id: None,
            signal_report: None,
            staying: StayingStatus::InAndOut,
            name: parse_name("Maria").expect("valid"),
            location: parse_location("Hartford, CT").expect("valid"),
            grid: None,
            source: CheckInSource::Staff,
            via: None,
            relayed_by: None,
        };
        let payload = to_payload(&with_identity);
        assert_eq!(payload["name"], "Maria");
        assert_eq!(payload["location"], "Hartford, CT");
        assert_eq!(
            body_from("checkin.added", payload).expect("decodes"),
            with_identity
        );

        // A None name/location is OMITTED (not null), like the other optionals.
        let callsign_only = SessionEventBody::CheckinAdded {
            check_in_id: Uuid::from_u128(70),
            callsign: call("W1AW"),
            client_event_id: None,
            signal_report: None,
            staying: StayingStatus::InAndOut,
            name: None,
            location: None,
            grid: None,
            source: CheckInSource::Staff,
            via: None,
            relayed_by: None,
        };
        let p = to_payload(&callsign_only);
        assert!(p.as_object().expect("obj").get("name").is_none());
        assert!(p.as_object().expect("obj").get("location").is_none());

        // A HISTORICAL payload (no name/location keys) decodes to None.
        let historical = serde_json::json!({
            "checkInId": Uuid::from_u128(70).to_string(),
            "callsign": "W1AW",
            "staying": "in-and-out",
        });
        match body_from("checkin.added", historical).expect("decodes") {
            SessionEventBody::CheckinAdded { name, location, .. } => {
                assert_eq!(name, None);
                assert_eq!(location, None);
            }
            _ => panic!("expected CheckinAdded"),
        }
    }

    #[test]
    fn checkin_added_grid_round_trips_and_is_omitted_when_absent() {
        // The grid rides `checkin.added` as its canonical
        // string, alongside — never instead of — the free-text location, and is
        // OMITTED (not null) when absent.
        let with_grid = SessionEventBody::CheckinAdded {
            check_in_id: Uuid::from_u128(91),
            callsign: call("W1AW"),
            client_event_id: None,
            signal_report: None,
            staying: StayingStatus::InAndOut,
            name: None,
            location: parse_location("Hartford, CT").expect("valid"),
            grid: Some(parse_grid("FN31pr").expect("valid grid")),
            source: CheckInSource::Staff,
            via: None,
            relayed_by: None,
        };
        let payload = to_payload(&with_grid);
        assert_eq!(payload["grid"], "FN31pr");
        assert_eq!(payload["location"], "Hartford, CT");
        assert_eq!(
            body_from("checkin.added", payload).expect("decodes"),
            with_grid
        );

        let without_grid = SessionEventBody::CheckinAdded {
            check_in_id: Uuid::from_u128(91),
            callsign: call("W1AW"),
            client_event_id: None,
            signal_report: None,
            staying: StayingStatus::InAndOut,
            name: None,
            location: None,
            grid: None,
            source: CheckInSource::Staff,
            via: None,
            relayed_by: None,
        };
        let p = to_payload(&without_grid);
        assert!(
            p.as_object().expect("obj").get("grid").is_none(),
            "an absent grid is OMITTED, never null: {p}"
        );
    }

    #[test]
    fn checkin_updated_grid_round_trips_and_is_omitted_when_absent() {
        // The same omit-optional discipline on the edit
        // payload, which carries the FULL post-edit field set.
        let with_grid = SessionEventBody::CheckinUpdated {
            check_in_id: Uuid::from_u128(92),
            callsign: call("W1AX"),
            name: None,
            location: None,
            grid: Some(parse_grid("fn31").expect("valid grid")),
            signal_report: None,
            staying: StayingStatus::InAndOut,
            precedence: Precedence::Routine,
            traffic: None,
            notes: None,
            public_note: None,
            via: None,
            relayed_by: None,
        };
        let payload = to_payload(&with_grid);
        // The CANONICAL form is stored, not the operator's casing.
        assert_eq!(payload["grid"], "FN31");
        assert_eq!(
            body_from("checkin.updated", payload).expect("decodes"),
            with_grid
        );

        let cleared = SessionEventBody::CheckinUpdated {
            check_in_id: Uuid::from_u128(92),
            callsign: call("W1AX"),
            name: None,
            location: None,
            grid: None,
            signal_report: None,
            staying: StayingStatus::InAndOut,
            precedence: Precedence::Routine,
            traffic: None,
            notes: None,
            public_note: None,
            via: None,
            relayed_by: None,
        };
        let p = to_payload(&cleared);
        assert!(
            p.as_object().expect("obj").get("grid").is_none(),
            "a cleared grid is OMITTED, never null: {p}"
        );
    }

    #[test]
    fn a_pre_9_1_payload_with_no_grid_key_decodes_to_none() {
        // THE ADAPTER TEST FOR HISTORICAL PAYLOADS. These two objects are written
        // by hand precisely because they must contain NO `grid` key at all: they
        // are byte-shaped like every `checkin.added`/`checkin.updated` payload
        // already sitting in `session_events` on the live instance. `#[serde(
        // default)]` is what makes them decode instead of failing `missing field`.
        let historical_add = serde_json::json!({
            "checkInId": Uuid::from_u128(93).to_string(),
            "callsign": "W1AW",
            "staying": "in-and-out",
            "location": "Hartford, CT",
        });
        assert!(
            historical_add
                .as_object()
                .expect("obj")
                .get("grid")
                .is_none(),
            "the fixture itself must carry no grid key"
        );
        match body_from("checkin.added", historical_add).expect("decodes") {
            SessionEventBody::CheckinAdded { grid, location, .. } => {
                assert_eq!(grid, None);
                // The pre-existing free-text location is untouched by the new field.
                assert_eq!(
                    location.map(|l| l.as_str().to_owned()),
                    Some("Hartford, CT".to_owned())
                );
            }
            _ => panic!("expected CheckinAdded"),
        }

        let historical_update = serde_json::json!({
            "checkInId": Uuid::from_u128(93).to_string(),
            "callsign": "W1AW",
            "staying": "in-and-out",
        });
        assert!(
            historical_update
                .as_object()
                .expect("obj")
                .get("grid")
                .is_none(),
            "the fixture itself must carry no grid key"
        );
        match body_from("checkin.updated", historical_update).expect("decodes") {
            SessionEventBody::CheckinUpdated { grid, .. } => assert_eq!(grid, None),
            _ => panic!("expected CheckinUpdated"),
        }

        // Forward-compat: an UNKNOWN key must still decode (proves no
        // `deny_unknown_fields` crept onto either payload struct — an older
        // binary must survive reading a newer payload).
        let from_the_future = serde_json::json!({
            "checkInId": Uuid::from_u128(94).to_string(),
            "callsign": "W1AW",
            "staying": "in-and-out",
            "grid": "FN31",
            "somethingNobodyHasShippedYet": 7,
        });
        match body_from("checkin.added", from_the_future).expect("decodes") {
            SessionEventBody::CheckinAdded { grid, .. } => {
                assert_eq!(grid.map(|g| g.as_str().to_owned()), Some("FN31".to_owned()));
            }
            _ => panic!("expected CheckinAdded"),
        }
    }

    #[test]
    fn checkin_added_source_round_trips_and_a_historical_payload_defaults_to_staff() {
        // A self-sourced add serializes `source: "self"` and
        // round-trips; a HISTORICAL payload (no `source` key) decodes to
        // Staff via the serde default (additive-compat — no migration).
        let self_added = SessionEventBody::CheckinAdded {
            check_in_id: Uuid::from_u128(71),
            callsign: call("N1CCK"),
            client_event_id: None,
            signal_report: None,
            staying: StayingStatus::InAndOut,
            name: None,
            location: None,
            grid: None,
            source: CheckInSource::SelfService,
            via: None,
            relayed_by: None,
        };
        let payload = to_payload(&self_added);
        assert_eq!(payload["source"], "self");
        assert_eq!(
            body_from("checkin.added", payload).expect("decodes"),
            self_added
        );

        let historical = serde_json::json!({
            "checkInId": Uuid::from_u128(72).to_string(),
            "callsign": "W1AW",
            "staying": "in-and-out",
        });
        match body_from("checkin.added", historical).expect("decodes") {
            SessionEventBody::CheckinAdded { source, .. } => {
                assert_eq!(source, CheckInSource::Staff);
            }
            _ => panic!("expected CheckinAdded"),
        }
    }

    #[test]
    fn control_handed_off_payload_round_trips_the_new_ncs_account_id_camelcase() {
        // Both handoff paths mint this one kind; the durable
        // (owner) payload carries the new active NCS's account id.
        let new_ncs = Uuid::from_u128(88);
        let body = SessionEventBody::ControlHandedOff {
            new_ncs_account_id: new_ncs,
        };
        let payload = to_payload(&body);
        assert_eq!(payload["newNcsAccountId"], new_ncs.to_string());
        assert_eq!(payload.as_object().expect("obj").len(), 1);
        assert_eq!(
            body_from("control.handed-off", payload).expect("decodes"),
            body
        );
    }

    #[test]
    fn station_blocked_payload_round_trips_the_account_id_camelcase() {
        // The durable (owner) payload carries the blocked
        // account id under a camelCase key; it round-trips back to the variant.
        let account = Uuid::from_u128(99);
        let body = SessionEventBody::StationBlocked {
            account_id: account,
        };
        let payload = to_payload(&body);
        assert_eq!(payload["accountId"], account.to_string());
        assert_eq!(payload.as_object().expect("obj").len(), 1);
        assert_eq!(
            body_from("station.blocked", payload).expect("decodes"),
            body
        );
    }

    #[test]
    fn ncs_stall_and_resume_payloads_round_trip_as_empty_objects() {
        // The transition IS the fact: stall/resume carry no payload.
        for body in [SessionEventBody::NcsStalled, SessionEventBody::NcsResumed] {
            let payload = to_payload(&body);
            assert_eq!(payload, serde_json::json!({}));
            assert_eq!(body_from(body.kind(), payload).expect("decodes"), body);
        }
    }

    #[test]
    fn checkin_removed_payload_round_trips_as_a_bare_tombstone() {
        let body = SessionEventBody::CheckinRemoved {
            check_in_id: Uuid::from_u128(7),
        };
        let payload = to_payload(&body);
        assert_eq!(payload["checkInId"], Uuid::from_u128(7).to_string());
        assert_eq!(payload.as_object().expect("obj").len(), 1);
        let decoded = body_from("checkin.removed", payload).expect("decodes");
        assert_eq!(decoded, body);
    }

    #[test]
    fn a_historical_updated_payload_without_staying_decodes_to_the_default() {
        // Additive-compat: a payload missing `staying` decodes to in-and-out.
        let payload = serde_json::json!({
            "checkInId": Uuid::from_u128(1).to_string(),
            "callsign": "W1AW",
        });
        let decoded = body_from("checkin.updated", payload).expect("decodes");
        match decoded {
            SessionEventBody::CheckinUpdated {
                staying,
                name,
                location,
                ..
            } => {
                assert_eq!(staying, StayingStatus::InAndOut);
                assert_eq!(name, None);
                assert_eq!(location, None);
            }
            _ => panic!("expected CheckinUpdated"),
        }
    }

    #[test]
    fn updated_location_round_trips_when_present() {
        let body = SessionEventBody::CheckinUpdated {
            check_in_id: Uuid::from_u128(3),
            callsign: call("W1AW"),
            name: None,
            location: parse_location("Hartford, CT").expect("valid"),
            grid: None,
            signal_report: None,
            staying: StayingStatus::InAndOut,
            precedence: Precedence::Routine,
            traffic: None,
            notes: None,
            public_note: None,
            via: None,
            relayed_by: None,
        };
        let decoded = body_from("checkin.updated", to_payload(&body)).expect("decodes");
        assert_eq!(decoded, body);
    }

    #[test]
    fn updated_precedence_and_traffic_round_trip_with_camelcase_and_omitted_traffic() {
        let body = SessionEventBody::CheckinUpdated {
            check_in_id: Uuid::from_u128(9),
            callsign: call("W1AW"),
            name: None,
            location: None,
            grid: None,
            signal_report: None,
            staying: StayingStatus::InAndOut,
            precedence: Precedence::Emergency,
            traffic: parse_traffic_count(Some(3)).expect("valid"),
            notes: None,
            public_note: None,
            via: None,
            relayed_by: None,
        };
        let payload = to_payload(&body);
        // precedence always present as its kebab token; traffic present as a number.
        assert_eq!(payload["precedence"], "emergency");
        assert_eq!(payload["traffic"], 3);
        let decoded = body_from("checkin.updated", payload).expect("decodes");
        assert_eq!(decoded, body);

        // A None traffic is OMITTED (not null), like the other optionals.
        let no_traffic = SessionEventBody::CheckinUpdated {
            check_in_id: Uuid::from_u128(9),
            callsign: call("W1AW"),
            name: None,
            location: None,
            grid: None,
            signal_report: None,
            staying: StayingStatus::InAndOut,
            precedence: Precedence::Priority,
            traffic: None,
            notes: None,
            public_note: None,
            via: None,
            relayed_by: None,
        };
        let p = to_payload(&no_traffic);
        assert!(p.as_object().expect("obj").get("traffic").is_none());
        assert_eq!(p["precedence"], "priority");
        assert_eq!(
            body_from("checkin.updated", p).expect("decodes"),
            no_traffic
        );
    }

    #[test]
    fn a_historical_updated_payload_without_precedence_decodes_to_routine() {
        // Additive-compat: a payload written before those fields existed
        // decodes to the Routine default / no traffic.
        let payload = serde_json::json!({
            "checkInId": Uuid::from_u128(1).to_string(),
            "callsign": "W1AW",
            "staying": "in-and-out",
        });
        let decoded = body_from("checkin.updated", payload).expect("decodes");
        match decoded {
            SessionEventBody::CheckinUpdated {
                precedence,
                traffic,
                ..
            } => {
                assert_eq!(precedence, Precedence::Routine);
                assert_eq!(traffic, None);
            }
            _ => panic!("expected CheckinUpdated"),
        }
    }

    #[test]
    fn checkin_updated_notes_round_trip_with_omit_optional_and_historical_none() {
        // `notes` rides checkin.updated, OMITTED when absent.
        let with_note = SessionEventBody::CheckinUpdated {
            check_in_id: Uuid::from_u128(5),
            callsign: call("W1AW"),
            name: None,
            location: None,
            grid: None,
            signal_report: None,
            staying: StayingStatus::InAndOut,
            precedence: Precedence::Routine,
            traffic: None,
            notes: netroll_domain::check_in::parse_note("handling traffic").expect("valid"),
            public_note: None,
            via: None,
            relayed_by: None,
        };
        let payload = to_payload(&with_note);
        assert_eq!(payload["notes"], "handling traffic");
        assert_eq!(
            body_from("checkin.updated", payload).expect("decodes"),
            with_note
        );

        // A None note is OMITTED (not null), like the other optionals.
        let no_note = SessionEventBody::CheckinUpdated {
            check_in_id: Uuid::from_u128(5),
            callsign: call("W1AW"),
            name: None,
            location: None,
            grid: None,
            signal_report: None,
            staying: StayingStatus::InAndOut,
            precedence: Precedence::Routine,
            traffic: None,
            notes: None,
            public_note: None,
            via: None,
            relayed_by: None,
        };
        let p = to_payload(&no_note);
        assert!(p.as_object().expect("obj").get("notes").is_none());

        // A HISTORICAL payload with no `notes` key decodes to None.
        let historical = serde_json::json!({
            "checkInId": Uuid::from_u128(5).to_string(),
            "callsign": "W1AW",
            "staying": "in-and-out",
            "precedence": "routine",
        });
        match body_from("checkin.updated", historical).expect("decodes") {
            SessionEventBody::CheckinUpdated { notes, .. } => assert_eq!(notes, None),
            _ => panic!("expected CheckinUpdated"),
        }
    }

    #[test]
    fn checkin_updated_public_note_round_trips_and_a_historical_payload_decodes_to_none() {
        // The persisted key `notes` STAYS and now MEANS the
        // staff note; `publicNote` is a new optional key beside it. Renaming
        // `notes` to `staffNote` would have forced a decode alias on every
        // historical event or a backfill of an append-only log — and got wrong,
        // it would BLANK every note written before the split, which is
        // inverted.
        let both = SessionEventBody::CheckinUpdated {
            check_in_id: Uuid::from_u128(5),
            callsign: call("W1AW"),
            name: None,
            location: None,
            grid: None,
            signal_report: None,
            staying: StayingStatus::InAndOut,
            precedence: Precedence::Routine,
            traffic: None,
            notes: netroll_domain::check_in::parse_note("STAFF: sounded rough").expect("valid"),
            public_note: netroll_domain::check_in::parse_note("PUBLIC: relaying").expect("valid"),
            via: None,
            relayed_by: None,
        };
        let payload = to_payload(&both);
        assert_eq!(payload["notes"], "STAFF: sounded rough");
        assert_eq!(payload["publicNote"], "PUBLIC: relaying");
        assert_eq!(
            body_from("checkin.updated", payload).expect("decodes"),
            both
        );

        // An absent public note is OMITTED, not null — the shipped convention.
        let staff_only = SessionEventBody::CheckinUpdated {
            check_in_id: Uuid::from_u128(5),
            callsign: call("W1AW"),
            name: None,
            location: None,
            grid: None,
            signal_report: None,
            staying: StayingStatus::InAndOut,
            precedence: Precedence::Routine,
            traffic: None,
            notes: netroll_domain::check_in::parse_note("staff only").expect("valid"),
            public_note: None,
            via: None,
            relayed_by: None,
        };
        let p = to_payload(&staff_only);
        assert!(p.as_object().expect("obj").get("publicNote").is_none());

        // The migration outcome by construction: a payload written
        // BEFORE the split carries `notes` and no `publicNote` key. It decodes
        // with the prose intact in the STAFF field and None in the public one.
        let historical = serde_json::json!({
            "checkInId": Uuid::from_u128(5).to_string(),
            "callsign": "W1AW",
            "staying": "in-and-out",
            "precedence": "routine",
            "notes": "written months ago, under the old assumption",
        });
        match body_from("checkin.updated", historical).expect("decodes") {
            SessionEventBody::CheckinUpdated {
                notes, public_note, ..
            } => {
                assert_eq!(
                    notes.as_ref().map(|n| n.as_str()),
                    Some("written months ago, under the old assumption")
                );
                assert_eq!(public_note, None);
            }
            _ => panic!("expected CheckinUpdated"),
        }
    }

    #[test]
    fn a_public_note_failing_the_shared_prose_guard_is_a_decode_error_not_a_silent_drop() {
        // The public note is a `Note` under the SAME
        // `parse_note` the staff note uses — it is the first free-prose field
        // this project renders on an unauthenticated page, so the control/bidi
        // rejection is not optional. The READ path re-parses, exactly as `notes`
        // does.
        let hostile = serde_json::json!({
            "checkInId": Uuid::from_u128(5).to_string(),
            "callsign": "W1AW",
            "staying": "in-and-out",
            "precedence": "routine",
            "publicNote": "look\u{202e}elsewhere",
        });
        assert!(body_from("checkin.updated", hostile).is_err());
    }

    #[test]
    fn a_via_kind_this_build_cannot_read_costs_one_label_and_not_the_whole_log() {
        // `ViaWire` is `kind`-discriminated,
        // so an unknown token used to fail the WHOLE payload deserialize — which
        // took the check-in-history page, the personal-data export and the
        // session's WS replay down with it, over one row a newer deploy wrote.
        let from_the_future = serde_json::json!({
            "checkInId": Uuid::from_u128(5).to_string(),
            "callsign": "W1AW",
            "staying": "in-and-out",
            "via": { "kind": "carrier-pigeon", "loft": "Hartford" },
        });
        let body = body_from("checkin.added", from_the_future).expect("the rest of the row reads");
        let SessionEventBody::CheckinAdded { callsign, via, .. } = body else {
            panic!("a checkin.added decodes to CheckinAdded");
        };
        assert_eq!(callsign.as_str(), "W1AW");
        // NOT `None`: that would say "nobody recorded a way in" about a row that
        // plainly recorded one. It resolves to `Unresolvable`, which says so.
        assert_ne!(via, None, "an unreadable via must not collapse into absent");
        assert_eq!(
            via_label(&resolve_via(via.as_ref(), &[])),
            Some(UNRESOLVABLE_VIA_LABEL.to_owned())
        );
    }

    #[test]
    fn a_stored_free_text_via_is_re_parsed_and_never_reaches_a_surface_unchecked() {
        // `via` has the widest render surface of any re-parsed field — it reaches
        // account-less viewers on the public roster — and it ships
        // as the ONE stored string that was not re-validated on read, while
        // `name`/`location`/`grid`/`signalReport`/`notes` all were.
        let hostile = serde_json::json!({
            "checkInId": Uuid::from_u128(5).to_string(),
            "callsign": "W1AW",
            "staying": "in-and-out",
            "via": { "kind": "unlisted", "text": "look\u{202e}elsewhere" },
        });
        let body = body_from("checkin.added", hostile).expect("the rest of the row reads");
        let SessionEventBody::CheckinAdded { via, .. } = body else {
            panic!("a checkin.added decodes to CheckinAdded");
        };
        let rendered = via_label(&resolve_via(via.as_ref(), &[]));
        assert_eq!(rendered, Some(UNRESOLVABLE_VIA_LABEL.to_owned()));
        assert!(
            !rendered.expect("rendered").contains('\u{202e}'),
            "the bidi override never reaches a rendered surface"
        );
    }

    #[test]
    fn a_readable_free_text_via_still_round_trips_byte_for_byte() {
        let body = SessionEventBody::CheckinAdded {
            check_in_id: Uuid::from_u128(5),
            callsign: call("W1AW"),
            client_event_id: None,
            signal_report: None,
            staying: StayingStatus::default(),
            name: None,
            location: None,
            grid: None,
            source: CheckInSource::SelfService,
            via: Some(Via::Unlisted("Bob's hotspot".to_owned())),
            relayed_by: None,
        };
        let payload = to_payload(&body);
        // The stored shape is UNCHANGED by the tolerant read type: a bare
        // `kind`-discriminated object.
        assert_eq!(payload["via"]["kind"], "unlisted");
        assert_eq!(payload["via"]["text"], "Bob's hotspot");
        assert_eq!(body_from("checkin.added", payload).expect("decodes"), body);
    }

    #[test]
    fn one_unreadable_stored_connection_costs_one_label_and_not_the_whole_set() {
        // The all-or-nothing read made ONE
        // malformed element empty the set, so every `via` on that session read
        // "no longer lists" — indistinguishable from a genuinely deleted way in.
        let good = Uuid::from_u128(7);
        let stored = serde_json::json!([
            { "id": good.to_string(), "position": 0, "kind": "echolink", "node": "12345" },
            { "this": "is not a connection" },
        ]);
        let connections = connections_of(Some(stored));
        assert_eq!(connections.len(), 1, "the readable element survives");
        assert_eq!(
            via_label(&resolve_via(Some(&Via::Connection(good)), &connections)),
            Some("EchoLink — 12345".to_owned()),
            "and a via naming it still resolves to its label"
        );
    }

    #[test]
    fn station_worked_set_payload_round_trips_with_target_and_null() {
        // CheckInId present …
        let set = SessionEventBody::StationWorkedSet {
            check_in_id: Some(Uuid::from_u128(42)),
        };
        let payload = to_payload(&set);
        assert_eq!(payload["checkInId"], Uuid::from_u128(42).to_string());
        assert_eq!(
            body_from("station.worked-set", payload).expect("decodes"),
            set
        );

        // … and the cursor-clearing null case (present key valued null).
        let clear = SessionEventBody::StationWorkedSet { check_in_id: None };
        let payload = to_payload(&clear);
        assert!(payload["checkInId"].is_null());
        assert_eq!(
            body_from("station.worked-set", payload).expect("decodes"),
            clear
        );
    }

    #[test]
    fn session_note_set_payload_round_trips_with_note_and_cleared() {
        // A present net-level note …
        let set = SessionEventBody::SessionNoteSet {
            note: netroll_domain::check_in::parse_note("Net closing in 5").expect("valid"),
        };
        let payload = to_payload(&set);
        assert_eq!(payload["note"], "Net closing in 5");
        assert_eq!(
            body_from("session.note-set", payload).expect("decodes"),
            set
        );

        // … and the cleared case (note OMITTED when None).
        let clear = SessionEventBody::SessionNoteSet { note: None };
        let payload = to_payload(&clear);
        assert!(payload.as_object().expect("obj").get("note").is_none());
        assert_eq!(
            body_from("session.note-set", payload).expect("decodes"),
            clear
        );
    }

    #[test]
    fn roster_order_mode_set_payload_round_trips_and_tolerates_an_unknown_token() {
        // The session-scoped ordering mode rides a kebab token.
        let sink = SessionEventBody::RosterOrderModeSet {
            mode: RosterOrderMode::WorkedSink,
        };
        let payload = to_payload(&sink);
        assert_eq!(payload["mode"], "worked-sink");
        assert_eq!(
            body_from("roster.order-mode-set", payload).expect("decodes"),
            sink
        );

        let manual = SessionEventBody::RosterOrderModeSet {
            mode: RosterOrderMode::Manual,
        };
        let payload = to_payload(&manual);
        assert_eq!(payload["mode"], "manual");
        assert_eq!(
            body_from("roster.order-mode-set", payload).expect("decodes"),
            manual
        );

        // An unknown or absent token decodes to the DEFAULT rather than failing:
        // the mode is inert display state, and a whole session must never become
        // unreadable because one stored token is out of vocabulary.
        assert_eq!(
            body_from(
                "roster.order-mode-set",
                serde_json::json!({ "mode": "shuffle" })
            )
            .expect("decodes"),
            manual
        );
        assert_eq!(
            body_from("roster.order-mode-set", serde_json::json!({})).expect("decodes"),
            manual
        );
    }

    #[test]
    fn roster_reordered_payload_round_trips_the_order_array() {
        let body = SessionEventBody::RosterReordered {
            order: vec![Uuid::from_u128(3), Uuid::from_u128(1), Uuid::from_u128(2)],
        };
        let payload = to_payload(&body);
        assert_eq!(
            payload["order"][0],
            Uuid::from_u128(3).to_string(),
            "the order array preserves the given sequence"
        );
        let decoded = body_from("roster.reordered", payload).expect("decodes");
        assert_eq!(decoded, body);
    }

    // --- The as-at overlay the two history reads share -----------

    fn stored_hf(id: Uuid, planned_hz: i64) -> serde_json::Value {
        serde_json::json!([{
            "id": id,
            "position": 0,
            "kind": "hf",
            "plannedFrequencyHz": planned_hz,
            "band": "20m",
            "mode": "ssb",
            "repeaterOffsetHz": null,
            "toneMode": null,
            "toneValue": null,
            "node": null,
            "reflector": null,
            "network": null,
            "talkgroup": null,
            "label": null,
            "detail": null
        }])
    }

    fn stored_move(seq: i64, connection_id: Uuid, hz: i64) -> serde_json::Value {
        serde_json::json!({
            "seq": seq,
            "payload": { "connectionId": connection_id, "operatingFrequencyHz": hz }
        })
    }

    #[test]
    fn the_as_at_set_applies_only_the_moves_the_lateral_selected_and_no_moves_means_planned() {
        let hf = Uuid::from_u128(0x17_01);
        let moves = serde_json::json!([
            stored_move(3, hf, 14_240_000),
            stored_move(5, hf, 14_250_000),
        ]);
        // The lateral already filtered `seq <= page.seq`; the Rust fold cuts
        // off at the same seq, so a stricter SQL filter changes nothing and a
        // looser one (the M3 mutation) is caught by the domain helper.
        let at_four = connections_as_at(Some(stored_hf(hf, 14_230_000)), Some(moves), 4);
        assert_eq!(at_four[0].planned_frequency_hz, Some(14_240_000));
        let never_moved = connections_as_at(Some(stored_hf(hf, 14_230_000)), None, 4);
        assert_eq!(never_moved[0].planned_frequency_hz, Some(14_230_000));
    }

    #[test]
    fn one_unreadable_frequency_move_is_skipped_and_the_readable_ones_still_apply() {
        // The `connections_of` posture, one column over: an older move (no
        // `connectionId`, refused on decode) or a malformed element costs
        // that ONE move, never the row and never the page.
        let hf = Uuid::from_u128(0x17_02);
        let moves = serde_json::json!([
            { "seq": 2, "payload": { "operatingFrequencyHz": 7_000_000 } },
            "not even an object",
            // A seq no durable log can assign: skipped (and warned) like its
            // siblings, never a panic or a 500.
            stored_move(-1, hf, 7_100_000),
            stored_move(3, hf, 14_250_000),
        ]);
        let run = frequency_moves_of(Some(moves));
        assert_eq!(run.len(), 1);
        assert_eq!(run[0].seq, 3);
        assert_eq!(run[0].operating_frequency_hz, 14_250_000);

        // And a run that is not an array at all degrades to "no moves", so the
        // via still resolves — against the planned frequency — rather than 500.
        assert!(frequency_moves_of(Some(serde_json::json!({ "seq": 3 }))).is_empty());
    }
}
