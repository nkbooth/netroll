// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
//! The WebSocket wire contract.
//!
//! Every server-to-client frame is a camelCase JSON text frame discriminated by
//! `type`: `snapshot` once on a fresh connect, then one `event` per delta. The
//! stream is structurally read-only, so there is no client-message type here.

use netroll_adapters::pg::session_events::to_payload;
use netroll_domain::event::{SessionEvent, SessionEventBody};
use netroll_domain::net::wire::via_of;
use serde::Serialize;
use serde_json::json;
use uuid::Uuid;

use crate::http::net_sessions::{PublicSessionView, SessionSummaryBody};
use crate::http::rfc3339;
use crate::ws::hub::LockDelta;

/// One appended event delta: the SHARED element serialized by BOTH the WS
/// `event` frame and the HTTP catch-up endpoint. Flattened into the WS frame
/// under a `type` discriminator, serialized bare as an array element over HTTP,
/// where the framing already delimits.
///
/// ONE element serializer is what guarantees the client folds a single event
/// shape whether a delta arrives live or through catch-up. `actorId` is omitted,
/// not null, when the event has no actor; `payload` is byte-identical to the
/// stored `session_events.payload`.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct WireEvent {
    /// Per-session monotonic sequence number.
    seq: u64,
    /// Stable `noun.verb` kind token (`SessionEventBody::kind`).
    kind: &'static str,
    /// The account that caused the event; omitted when absent.
    #[serde(skip_serializing_if = "Option::is_none")]
    actor_id: Option<Uuid>,
    /// RFC 3339 UTC timestamp (the event's `at`).
    at: String,
    /// The variant's own fields, camelCase — byte-identical to the stored
    /// `session_events.payload` for this kind.
    payload: serde_json::Value,
}

impl WireEvent {
    /// Builds the shared wire element from a domain [`SessionEvent`], reusing
    /// the DB payload serializer so the wire payload matches the stored JSONB
    /// exactly.
    pub(crate) fn from_event(event: &SessionEvent) -> Self {
        WireEvent {
            seq: event.seq,
            kind: event.body.kind(),
            actor_id: event.actor_id,
            at: rfc3339(event.at),
            payload: to_payload(&event.body),
        }
    }

    /// Builds the REDACTED public wire element for the account-less catch-up
    /// endpoint and the public WS deltas. It forces `actorId` out and projects
    /// an ALLOWLIST of public-safe payload keys per kind through the exhaustive
    /// [`public_payload`] match. An allowlist rather than a denylist, so a FUTURE
    /// variant cannot leak an operator or staff field by default: it fails to
    /// compile until its public projection is declared.
    pub(crate) fn from_event_public(event: &SessionEvent) -> Self {
        WireEvent {
            seq: event.seq,
            kind: event.body.kind(),
            actor_id: None,
            at: rfc3339(event.at),
            payload: public_payload(&event.body),
        }
    }
}

/// The PUBLIC-SAFE payload projection for each event kind: an EXHAUSTIVE
/// allowlist match, so a new variant will not compile until its public
/// projection is declared and no operator, internal or staff field can leak by
/// default.
///
/// Public radio data — operating frequency, callsign, the check-in row id,
/// `staying`, `precedence`, `traffic` and the public note — is kept. Everything
/// else, including `clientEventId`, the definition ids, `name`, `location`,
/// `signalReport`, the STAFF note, corrections and operator ids, is omitted by
/// construction: it is simply never projected.
fn public_payload(body: &SessionEventBody) -> serde_json::Value {
    match body {
        // `session.started` and `frequency.changed` were one or-pattern arm
        // while they shared a payload field; that field is gone from
        // `session.started` entirely.
        //
        // `session.started` projects `{}`, decided here rather than inherited:
        // what remains of its payload is an internal join key and a counter,
        // neither of which is public radio data. A public subscriber that sees
        // this delta refetches the snapshot, where the connection set lives.
        SessionEventBody::SessionStarted { .. } => json!({}),
        // `frequency.changed` now carries WHICH connection moved, and a public
        // viewer needs both halves: a net with three ways to reach it has three
        // frequencies, and a bare number names none of them. The connection id
        // is a snapshot-local identifier the public view already publishes, not
        // an account or operator identity.
        SessionEventBody::FrequencyChanged {
            connection_id,
            operating_frequency_hz,
        } => json!({
            "connectionId": connection_id,
            "operatingFrequencyHz": operating_frequency_hz,
        }),
        // `source` IS public provenance, like the callsign and the worked
        // cursor, so the account-less roster can render the Self badge. It is a
        // two-value flag, not an identity: `addedBy` and `actorId` stay redacted.
        //
        // `staying` and `via` are the only widened fields this arm carries,
        // because they are the only ones SET AT ADD. Precedence, traffic and
        // both notes are edit-only, so the add event does not carry them at all
        // and there is nothing here to project. That is not licence to widen
        // those three: when one gains an add-time value it needs its own
        // decision, not this one by analogy.
        //
        // ⚠️ The allowlist is exhaustive over event VARIANTS, not over a
        // variant's FIELDS. When this arm ended in `..`, a newly added field
        // compiled GREEN and was SILENTLY redacted. The EXHAUSTIVE `_`-bound
        // destructure is what makes that decision a compile error the author has
        // to answer; the behaviour tests below, not rustc, are the backstop.
        //
        // NOT PROJECTED, one reason each:
        // - `client_event_id` — the caller's own idempotency token, echoed to
        // nobody but the caller's socket.
        // - `signal_report` — staff radio data, never public.
        // - `name`/`location`/`grid` — operator-identifying; `grid` is
        // additionally a MORE precise location than the free-text one already
        // withheld.
        SessionEventBody::CheckinAdded {
            check_in_id,
            callsign,
            source,
            staying,
            via,
            // An explicit refusal, not an omission. `via` names a connection
            // the OWNER published, which this view already carries in
            // `connections`; `relayed_by` names a THIRD-PARTY STATION that never
            // checked in and appears nowhere else on the page, which puts it
            // with `added_by` and not with `via`. The observer payoff is nil:
            // someone watching a public roster wants to know how to reach the
            // net, not who passed the traffic. A leak here is UNRECOVERABLE.
            relayed_by: _,
            client_event_id: _,
            signal_report: _,
            name: _,
            location: _,
            grid: _,
        } => {
            let mut payload = serde_json::Map::new();
            payload.insert("checkInId".into(), json!(check_in_id));
            payload.insert("callsign".into(), json!(callsign.as_str()));
            payload.insert("source".into(), json!(source.as_str()));
            payload.insert("staying".into(), json!(staying.as_str()));
            // Omitted, not null, when absent — hand-built for the same
            // reason the update arm below is.
            if let Some(via) = via {
                payload.insert("via".into(), json!(via_of(via)));
            }
            serde_json::Value::Object(payload)
        }
        // No `source`: provenance is set once, at add. It DOES carry the four
        // widened fields, so a mid-net precedence change reaches an observer
        // live rather than only on the next snapshot.
        //
        // EXHAUSTIVE `_`-bound destructure for the same reason as the add arm,
        // and the withheld fields for the same reasons: `signal_report` is staff
        // radio data, `name`/`location`/`grid` are operator-identifying, and
        // `notes` is the STAFF note — the public note is the one that crosses,
        // which is the whole point of the split.
        SessionEventBody::CheckinUpdated {
            check_in_id,
            callsign,
            staying,
            precedence,
            traffic,
            public_note,
            via,
            // The SAME refusal as the add arm, for the same reasons. Named
            // rather than left to `..`, which is the point of the exhaustive
            // destructure.
            relayed_by: _,
            name: _,
            location: _,
            grid: _,
            signal_report: _,
            notes: _,
        } => {
            // Built key-by-key rather than as one `json!` literal because the
            // omit-optional rule applies here too and a literal cannot express
            // it: `"traffic": None` serializes as an explicit `null`. The owner
            // wire gets this from `CheckinUpdatedPayload`'s
            // `skip_serializing_if` and the HTTP snapshot from
            // `PublicRosterEntry`'s; this arm is hand-built, so it says so.
            let mut payload = serde_json::Map::new();
            payload.insert("checkInId".into(), json!(check_in_id));
            payload.insert("callsign".into(), json!(callsign.as_str()));
            payload.insert("staying".into(), json!(staying.as_str()));
            payload.insert("precedence".into(), json!(precedence.as_str()));
            if let Some(count) = traffic {
                payload.insert("traffic".into(), json!(count.get() as i64));
            }
            if let Some(note) = public_note {
                payload.insert("publicNote".into(), json!(note.as_str()));
            }
            // The corrected way in reaches an observer live rather
            // than only on the next snapshot. STRUCTURED — the browser holds the
            // session's connections and resolves the label itself.
            if let Some(via) = via {
                payload.insert("via".into(), json!(via_of(via)));
            }
            serde_json::Value::Object(payload)
        }
        SessionEventBody::CheckinRemoved { check_in_id } => json!({ "checkInId": check_in_id }),
        // The reorder's public projection IS the order list: list position is
        // public radio data. This arm projects the permutation only — a
        // `roster.reordered` frame carries no labels because it carries no
        // per-station data at all, not because labels are withheld. The
        // precedence labels reach observers on the check-in arms instead.
        SessionEventBody::RosterReordered { order } => json!({ "order": order }),
        // The worked-station cursor position IS public radio data: project ONLY
        // `checkInId`, nullable, never an actor or any other field.
        SessionEventBody::StationWorkedSet { check_in_id } => {
            json!({ "checkInId": check_in_id })
        }
        // The net-note TEXT is operator-only/staff-private —
        // its public projection is empty. Only the fact that a note changed would
        // ever be observable, and even that is withheld to `{}`.
        SessionEventBody::SessionNoteSet { .. } => json!({}),
        // The token crosses because the public page's your-turn selector reads
        // the mode: under worked-sink "next up" is the top of the unworked
        // group, not the row after the cursor. A delta that dropped the token
        // would overwrite the correct snapshot value with the frontend's
        // `manual` default on the very next toggle. The actor is still withheld
        // by the envelope.
        SessionEventBody::RosterOrderModeSet { mode } => json!({ "mode": mode.as_str() }),
        SessionEventBody::SessionClosed => json!({}),
        // `ncs.stalled`/`ncs.resumed` ARE public events — a paused
        // net is publicly observable (the roster visibly freezes) — but the fold
        // derives `controlState` from the KIND alone, so the public payload is
        // empty (no operator id, no stall instant leaks).
        SessionEventBody::NcsStalled | SessionEventBody::NcsResumed => json!({}),
        // `control.handed-off`'s `newNcsAccountId` is an OPERATOR
        // id — redacted to `{}` on the public wire. The public just needs "control
        // is active again," which the kind (and the folded controlState) conveys.
        SessionEventBody::ControlHandedOff { .. } => json!({}),
        // `station.blocked`'s `accountId` is a
        // participant/operator id — redacted to `{}` on the public wire. The public
        // sees only the accompanying `checkin.removed` roster effect, never WHO was
        // blocked.
        SessionEventBody::StationBlocked { .. } => json!({}),
    }
}

/// A server-to-client WebSocket frame.
///
/// `#[serde(tag = "type")]` emits the discriminator; the variant names become
/// `"snapshot"`/`"event"`. The `Event` variant flattens the shared
/// [`WireEvent`] element (internally-tagged newtype variant), so the frame is
/// `{ "type": "event", seq, kind, actorId?, at, payload }` — byte-identical to
/// before the extraction.
#[derive(Serialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub(crate) enum ServerMessage {
    /// The folded session summary, byte-shape-identical to
    /// `GET /api/net-sessions/{id}` — sent exactly once, first, on a fresh
    /// connect (`since` absent/0). The client's resume cursor is
    /// `session.latestSeq`.
    Snapshot {
        /// The folded summary (reused verbatim from the REST surface).
        /// Boxed to keep the enum small — the summary dwarfs the `Event` variant
        /// (`clippy::large_enum_variant`, the `StartOutcome::Started` precedent);
        /// `Box` serializes transparently, so the wire shape is unchanged.
        session: Box<SessionSummaryBody>,
    },
    /// One appended event delta, in ascending `seq` — the shared [`WireEvent`]
    /// element under the `event` discriminator.
    Event(WireEvent),
    /// An ephemeral soft-lock advisory delta — a NON-event frame
    /// with NO `seq`, NEVER folded into `SessionState`, that updates a separate
    /// ephemeral store slice. `holderCallsign`/`expiresAt` are `null` on release
    /// (the entry is free). Owner consoles ONLY — the public WS never emits it.
    #[serde(rename_all = "camelCase")]
    Lock {
        /// The entry whose lock changed.
        check_in_id: Uuid,
        /// The holder's callsign while held; `null` on release.
        holder_callsign: Option<String>,
        /// The lease expiry as an RFC 3339 string while held; `null` on release.
        expires_at: Option<String>,
    },
}

impl ServerMessage {
    /// Builds an `event` frame from a domain [`SessionEvent`], reusing the
    /// shared [`WireEvent`] element so the streamed frame body matches the HTTP
    /// catch-up element (and the stored JSONB payload) exactly.
    pub(crate) fn event(event: &SessionEvent) -> Self {
        ServerMessage::Event(WireEvent::from_event(event))
    }

    /// Builds a `lock` frame from a hub [`LockDelta`]. The
    /// `expiresAt` millis are rendered as an RFC 3339 string, matching every
    /// other timestamp on the wire.
    pub(crate) fn lock(delta: &LockDelta) -> Self {
        ServerMessage::Lock {
            check_in_id: delta.check_in_id,
            holder_callsign: delta.holder_callsign.clone(),
            expires_at: delta.expires_at_millis.map(rfc3339),
        }
    }
}

/// A server-to-client frame on the PUBLIC, account-less WebSocket.
/// Byte-shape-identical framing to [`ServerMessage`], so the frontend folds the
/// public stream through the same code path with the operator and internal ids
/// stripped.
#[derive(Serialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub(crate) enum PublicServerMessage {
    /// The folded REDACTED public view, byte-shape-identical to
    /// `GET /api/net-sessions/{id}/live`. Boxed to keep the enum small (the
    /// `ServerMessage::Snapshot` precedent).
    Snapshot {
        /// The redacted public view (reused verbatim from the public REST read).
        session: Box<PublicSessionView>,
    },
    /// One appended event delta, REDACTED — no `actorId`, no internal ids.
    Event(WireEvent),
}

impl PublicServerMessage {
    /// Builds a redacted `event` frame from a domain [`SessionEvent`].
    pub(crate) fn event(event: &SessionEvent) -> Self {
        PublicServerMessage::Event(WireEvent::from_event_public(event))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use netroll_adapters::pg::net_sessions::{DefinitionSnapshot, NetSessionRow};
    use netroll_domain::callsign::parse_callsign;
    use netroll_domain::check_in::{CheckInSource, StayingStatus, parse_signal_report};
    use netroll_domain::event::SessionEventBody;
    use netroll_domain::fold::{SessionLifecycle, replay};
    use netroll_domain::net::connection::Via;

    fn checkin_event() -> SessionEvent {
        SessionEvent {
            seq: 43,
            actor_id: Some(Uuid::from_u128(100)),
            at: 1_700_000_000_000,
            body: SessionEventBody::CheckinAdded {
                check_in_id: Uuid::from_u128(42),
                callsign: parse_callsign("W1AW").expect("valid callsign"),
                client_event_id: Some(Uuid::from_u128(9)),
                signal_report: parse_signal_report("599").expect("valid report"),
                staying: StayingStatus::StayingForComments,
                name: None,
                location: None,
                grid: None,
                source: CheckInSource::Staff,
                via: None,
                relayed_by: None,
            },
        }
    }

    #[test]
    fn wire_event_element_shape_matches_db_payload_and_omits_absent_actor() {
        // The HTTP catch-up endpoint serializes each missed event
        // as this bare element — byte-shape-identical to the WS `event` frame
        // body minus the `type` discriminator. One shared serializer, two
        // consumers: the WS frame and the HTTP array element.
        let event = checkin_event();
        let v = serde_json::to_value(WireEvent::from_event(&event)).expect("serializes");
        assert_eq!(v["seq"], 43);
        assert_eq!(v["kind"], "checkin.added");
        assert!(v["at"].is_string(), "at is an RFC 3339 string");
        assert_eq!(v["actorId"], Uuid::from_u128(100).to_string());
        // Wire/DB parity: the element payload equals the stored JSONB payload.
        assert_eq!(v["payload"], to_payload(&event.body));
        // A bare element carries NO `type` discriminator (HTTP framing delimits).
        assert!(
            v.as_object().expect("object").get("type").is_none(),
            "the HTTP element has no type discriminator"
        );

        // actorId omitted (not null) when absent; closed payload is empty.
        let closed = SessionEvent {
            seq: 2,
            actor_id: None,
            at: 1_700_000_000_000,
            body: SessionEventBody::SessionClosed,
        };
        let cv = serde_json::to_value(WireEvent::from_event(&closed)).expect("serializes");
        assert!(
            cv.as_object().expect("object").get("actorId").is_none(),
            "actorId is omitted, not null, when the event has no actor"
        );
        assert_eq!(cv["payload"], serde_json::json!({}));
    }

    #[test]
    fn public_wire_event_strips_actor_and_internal_ids_from_the_serialized_bytes() {
        // The redacted serializer forces actorId out and strips clientEventId
        // and the definition ids from the payload. Asserted on the raw
        // serialized bytes, not just the struct.
        let checkin = checkin_event(); // has actor 100, clientEventId 9
        let v = serde_json::to_value(WireEvent::from_event_public(&checkin)).expect("serializes");
        assert_eq!(v["kind"], "checkin.added");
        assert_eq!(v["payload"]["callsign"], "W1AW");
        let raw = serde_json::to_string(&WireEvent::from_event_public(&checkin)).expect("string");
        assert!(
            v.as_object().expect("object").get("actorId").is_none(),
            "the public element carries no actorId"
        );
        assert!(
            v["payload"]
                .as_object()
                .expect("object")
                .get("clientEventId")
                .is_none(),
            "the public checkin payload drops clientEventId"
        );
        assert!(
            !raw.contains("actorId") && !raw.contains("clientEventId"),
            "neither operator id key appears in the raw bytes: {raw}"
        );

        // A session.started's payload internal net ids are stripped too.
        let started = SessionEvent {
            seq: 1,
            actor_id: Some(Uuid::from_u128(100)),
            at: 1_700_000_000_000,
            body: SessionEventBody::SessionStarted {
                definition_id: Uuid::from_u128(7),
                definition_version: 3,
            },
        };
        let raw_started =
            serde_json::to_string(&WireEvent::from_event_public(&started)).expect("string");
        assert!(
            !raw_started.contains("definitionId") && !raw_started.contains("definitionVersion"),
            "the public session.started frame strips the internal net ids: {raw_started}"
        );
        assert!(
            !raw_started.contains(&Uuid::from_u128(7).to_string()),
            "the definition id value never appears in the public frame"
        );
        // `session.started`'s public projection is empty. The frequency it used
        // to carry belonged to the session; frequencies belong to connections,
        // which ride the snapshot.
        let v_started =
            serde_json::to_value(WireEvent::from_event_public(&started)).expect("value");
        assert_eq!(v_started["payload"], serde_json::json!({}));

        // The OTHER half of the split arm keeps a projection, and it now names
        // the connection that moved — without which a three-way net's delta
        // says which frequency changed but not what is on it.
        let moved = SessionEvent {
            seq: 2,
            actor_id: Some(Uuid::from_u128(100)),
            at: 1_700_000_001_000,
            body: SessionEventBody::FrequencyChanged {
                connection_id: Uuid::from_u128(9),
                operating_frequency_hz: 7_200_000,
            },
        };
        let v_moved = serde_json::to_value(WireEvent::from_event_public(&moved)).expect("value");
        assert_eq!(
            v_moved["payload"]["connectionId"],
            Uuid::from_u128(9).to_string()
        );
        assert_eq!(v_moved["payload"]["operatingFrequencyHz"], 7_200_000);
    }

    #[test]
    fn lock_frame_serializes_type_check_in_id_holder_and_expiry() {
        let delta = LockDelta {
            check_in_id: Uuid::from_u128(42),
            holder_callsign: Some("W1AW".to_owned()),
            expires_at_millis: Some(1_700_000_015_000),
        };
        let v = serde_json::to_value(ServerMessage::lock(&delta)).expect("serializes");
        assert_eq!(v["type"], "lock");
        assert_eq!(v["checkInId"], Uuid::from_u128(42).to_string());
        assert_eq!(v["holderCallsign"], "W1AW");
        assert!(
            v["expiresAt"].is_string(),
            "expiresAt is an RFC 3339 string"
        );
    }

    #[test]
    fn a_release_lock_frame_carries_null_holder_and_expiry() {
        let delta = LockDelta {
            check_in_id: Uuid::from_u128(42),
            holder_callsign: None,
            expires_at_millis: None,
        };
        let v = serde_json::to_value(ServerMessage::lock(&delta)).expect("serializes");
        assert_eq!(v["type"], "lock");
        // Present keys valued null (not omitted) — the frontend reads them as
        // "released" (holderCallsign === null ⇒ drop the lock slice entry).
        assert!(v["holderCallsign"].is_null());
        assert!(v["expiresAt"].is_null());
    }

    #[test]
    fn public_checkin_added_keeps_callsign_but_strips_the_prefilled_name_and_location() {
        // Widening the OWNER checkin.added payload with name/location must NOT
        // leak them onto the public arm: the exhaustive allowlist projects only
        // public radio data and drops the new fields by construction.
        let added = SessionEvent {
            seq: 4,
            actor_id: Some(Uuid::from_u128(100)),
            at: 1_700_000_000_000,
            body: SessionEventBody::CheckinAdded {
                check_in_id: Uuid::from_u128(42),
                callsign: parse_callsign("W1AW").expect("valid"),
                client_event_id: Some(Uuid::from_u128(9)),
                signal_report: None,
                staying: StayingStatus::StayingForComments,
                name: netroll_domain::check_in::parse_name("Maria").expect("valid"),
                location: netroll_domain::check_in::parse_location("Hartford, CT").expect("valid"),
                grid: None,
                source: CheckInSource::SelfService,
                via: None,
                relayed_by: None,
            },
        };
        let v = serde_json::to_value(WireEvent::from_event_public(&added)).expect("value");
        assert_eq!(v["kind"], "checkin.added");
        assert_eq!(v["payload"]["callsign"], "W1AW");
        assert_eq!(v["payload"]["checkInId"], Uuid::from_u128(42).to_string());
        // `source` IS public provenance, so the account-less roster can render
        // the Self badge.
        assert_eq!(v["payload"]["source"], "self");
        // `staying` joins the public add arm. Seeded NON-DEFAULT so the
        // assertion discriminates a projected value from the reducer's
        // absent-key fallback. It is the only one of the four widened fields the
        // add event carries; the rest are edit-only.
        assert_eq!(v["payload"]["staying"], "staying-for-comments");
        assert_eq!(
            v["payload"].as_object().expect("obj").len(),
            4,
            "the public checkin.added carries checkInId + callsign + source + staying"
        );
        let raw = serde_json::to_string(&WireEvent::from_event_public(&added)).expect("string");
        assert!(
            !raw.contains("Maria") && !raw.contains("Hartford") && !raw.contains("actorId"),
            "no prefilled identity or operator id leaks on the public add: {raw}"
        );

        // The OWNER serializer DOES carry the widened payload (wire/DB parity).
        let owner = serde_json::to_value(WireEvent::from_event(&added)).expect("value");
        assert_eq!(owner["payload"]["name"], "Maria");
        assert_eq!(owner["payload"]["location"], "Hartford, CT");
    }

    #[test]
    fn the_owner_delta_carries_the_grid_and_the_public_delta_does_not() {
        // The OWNER half follows for free from wire/DB parity; the PUBLIC half
        // is what this test really guards, pinning the allowlist against a
        // future well-meaning widening. A grid is a MORE precise location than
        // the free-text field already withheld.
        let added = SessionEvent {
            seq: 6,
            actor_id: Some(Uuid::from_u128(100)),
            at: 1_700_000_000_000,
            body: SessionEventBody::CheckinAdded {
                check_in_id: Uuid::from_u128(42),
                callsign: parse_callsign("W1AW").expect("valid"),
                client_event_id: None,
                signal_report: None,
                staying: StayingStatus::InAndOut,
                name: None,
                location: None,
                grid: Some(netroll_domain::profile::parse_grid("FN31pr").expect("valid grid")),
                source: CheckInSource::Staff,
                via: None,
                relayed_by: None,
            },
        };
        let owner = serde_json::to_value(WireEvent::from_event(&added)).expect("value");
        assert_eq!(
            owner["payload"]["grid"], "FN31pr",
            "the fixture genuinely carries a grid on the owner delta"
        );

        let public = serde_json::to_value(WireEvent::from_event_public(&added)).expect("value");
        let mut keys: Vec<&str> = public["payload"]
            .as_object()
            .expect("obj")
            .keys()
            .map(String::as_str)
            .collect();
        keys.sort_unstable();
        assert_eq!(keys, vec!["callsign", "checkInId", "source", "staying"]);
        let raw = serde_json::to_string(&WireEvent::from_event_public(&added)).expect("string");
        assert!(!raw.contains("FN31pr"), "the grid never leaks: {raw}");
    }

    #[test]
    fn public_checkin_updated_keeps_the_observer_fields_but_strips_every_staff_field() {
        // The redacted public delta must never carry
        // name/location/signalReport/actorId. Four fields that were once absent
        // here — `staying`, `precedence`, `traffic` and the public note — now
        // cross deliberately; every field that is STILL redacted is re-asserted
        // below in the same test, so no assertion was merely removed.
        let updated = SessionEvent {
            seq: 5,
            actor_id: Some(Uuid::from_u128(100)),
            at: 1_700_000_000_000,
            body: SessionEventBody::CheckinUpdated {
                check_in_id: Uuid::from_u128(42),
                callsign: parse_callsign("W1AX").expect("valid"),
                name: netroll_domain::check_in::parse_name("Maria").expect("valid"),
                location: netroll_domain::check_in::parse_location("Hartford, CT").expect("valid"),
                grid: None,
                signal_report: parse_signal_report("599").expect("valid"),
                staying: StayingStatus::StayingForComments,
                precedence: netroll_domain::check_in::Precedence::Emergency,
                traffic: netroll_domain::check_in::parse_traffic_count(Some(3)).expect("valid"),
                notes: netroll_domain::check_in::parse_note("staff-only round note")
                    .expect("valid"),
                public_note: netroll_domain::check_in::parse_note("public round note")
                    .expect("valid"),
                via: None,
                relayed_by: None,
            },
        };
        let raw = serde_json::to_string(&WireEvent::from_event_public(&updated)).expect("string");
        let v = serde_json::to_value(WireEvent::from_event_public(&updated)).expect("value");
        assert_eq!(v["kind"], "checkin.updated");
        assert_eq!(v["payload"]["callsign"], "W1AX");
        assert_eq!(v["payload"]["checkInId"], Uuid::from_u128(42).to_string());

        // NOW CARRIED, with the value the operator set — not a
        // default the observer's reducer would have supplied.
        assert_eq!(v["payload"]["staying"], "staying-for-comments");
        assert_eq!(v["payload"]["precedence"], "emergency");
        assert_eq!(v["payload"]["traffic"], 3);
        assert_eq!(v["payload"]["publicNote"], "public round note");

        // STILL REDACTED, re-asserted here in the same test. The STAFF note is on
        // this list and the public note is not — that split is the whole reason
        // the field was split.
        assert!(
            !raw.contains("actorId")
                && !raw.contains("Maria")
                && !raw.contains("Hartford")
                && !raw.contains("signalReport")
                && !raw.contains("\"notes\"")
                && !raw.contains("staff-only round note"),
            "no staff/operator field leaks in the public updated delta: {raw}"
        );
    }

    #[test]
    fn the_public_updated_delta_omits_the_absent_optionals_rather_than_nulling_them() {
        // "Omitted, not null, when absent" governs every optional on every
        // wire. The OWNER delta gets it from `CheckinUpdatedPayload`'s
        // `skip_serializing_if` and the HTTP snapshot from `PublicRosterEntry`,
        // but this arm builds its JSON by hand, so nothing structural enforces
        // it. Without this test a `None` serializes as an explicit `null` and
        // the snapshot and the delta disagree about the same two keys, making
        // `sessionWire.ts`'s `readonly publicNote?: string` a lie.
        let updated = SessionEvent {
            seq: 7,
            actor_id: Some(Uuid::from_u128(100)),
            at: 1_700_000_000_000,
            body: SessionEventBody::CheckinUpdated {
                check_in_id: Uuid::from_u128(42),
                callsign: parse_callsign("W1AX").expect("valid"),
                name: None,
                location: None,
                grid: None,
                signal_report: None,
                staying: StayingStatus::InAndOut,
                precedence: netroll_domain::check_in::Precedence::Routine,
                traffic: None,
                notes: None,
                public_note: None,
                via: None,
                relayed_by: None,
            },
        };
        let v = serde_json::to_value(WireEvent::from_event_public(&updated)).expect("value");
        let payload = v["payload"].as_object().expect("payload object");
        assert!(
            !payload.contains_key("traffic"),
            "an absent traffic count is OMITTED, never a null key: {payload:?}"
        );
        assert!(
            !payload.contains_key("publicNote"),
            "an absent public note is OMITTED, never a null key: {payload:?}"
        );
        let mut keys: Vec<&str> = payload.keys().map(String::as_str).collect();
        keys.sort_unstable();
        assert_eq!(
            keys,
            vec!["callsign", "checkInId", "precedence", "staying"],
            "the always-projected four, and nothing null-shaped beside them"
        );

        // The PRESENT case still carries both, so this test cannot pass by the
        // arm simply dropping the fields.
        let present = SessionEvent {
            body: SessionEventBody::CheckinUpdated {
                check_in_id: Uuid::from_u128(42),
                callsign: parse_callsign("W1AX").expect("valid"),
                name: None,
                location: None,
                grid: None,
                signal_report: None,
                staying: StayingStatus::InAndOut,
                precedence: netroll_domain::check_in::Precedence::Routine,
                traffic: netroll_domain::check_in::parse_traffic_count(Some(2)).expect("valid"),
                notes: None,
                public_note: netroll_domain::check_in::parse_note("public round note")
                    .expect("valid"),
                via: None,
                relayed_by: None,
            },
            ..updated
        };
        let pv = serde_json::to_value(WireEvent::from_event_public(&present)).expect("value");
        assert_eq!(pv["payload"]["traffic"], 2);
        assert_eq!(pv["payload"]["publicNote"], "public round note");
    }

    #[test]
    fn public_roster_order_mode_set_carries_the_mode_and_nothing_else() {
        // The public page's your-turn selector READS the mode, so a public delta
        // that drops it overwrites the correct snapshot value with the
        // frontend's `manual` default. The token crosses; the actor does not.
        // `sessionReducer.test.ts` folds THIS payload.
        let event = SessionEvent {
            seq: 12,
            actor_id: Some(Uuid::from_u128(100)),
            at: 1_700_000_000_000,
            body: SessionEventBody::RosterOrderModeSet {
                mode: netroll_domain::fold::RosterOrderMode::WorkedSink,
            },
        };
        let v = serde_json::to_value(WireEvent::from_event_public(&event)).expect("value");
        assert_eq!(v["kind"], "roster.order-mode-set");
        assert_eq!(v["payload"]["mode"], "worked-sink");
        assert_eq!(v["payload"].as_object().expect("obj").len(), 1);
        assert!(v["actorId"].is_null());
    }

    #[test]
    fn public_station_worked_set_carries_only_the_check_in_id() {
        // The cursor position IS public radio data, so the public payload is
        // `{ checkInId }` ONLY — no other field, and no actorId.
        let worked = SessionEvent {
            seq: 8,
            actor_id: Some(Uuid::from_u128(100)),
            at: 1_700_000_000_000,
            body: SessionEventBody::StationWorkedSet {
                check_in_id: Some(Uuid::from_u128(42)),
            },
        };
        let v = serde_json::to_value(WireEvent::from_event_public(&worked)).expect("value");
        assert_eq!(v["kind"], "station.worked-set");
        assert_eq!(v["payload"]["checkInId"], Uuid::from_u128(42).to_string());
        assert_eq!(
            v["payload"].as_object().expect("obj").len(),
            1,
            "the public worked-set carries only checkInId"
        );
        let raw = serde_json::to_string(&WireEvent::from_event_public(&worked)).expect("string");
        assert!(
            !raw.contains("actorId"),
            "no operator id on the worked-set: {raw}"
        );

        // The cursor-clearing (None) case still projects checkInId (valued null).
        let cleared = SessionEvent {
            seq: 9,
            actor_id: Some(Uuid::from_u128(100)),
            at: 1_700_000_000_000,
            body: SessionEventBody::StationWorkedSet { check_in_id: None },
        };
        let cv = serde_json::to_value(WireEvent::from_event_public(&cleared)).expect("value");
        assert!(cv["payload"]["checkInId"].is_null());
    }

    #[test]
    fn public_session_note_set_is_an_empty_payload() {
        // The net-note TEXT is operator-only/staff-private —
        // its public projection is `{}` (empty), never the note itself.
        let note = SessionEvent {
            seq: 10,
            actor_id: Some(Uuid::from_u128(100)),
            at: 1_700_000_000_000,
            body: SessionEventBody::SessionNoteSet {
                note: netroll_domain::check_in::parse_note("Net closing in 5").expect("valid"),
            },
        };
        let v = serde_json::to_value(WireEvent::from_event_public(&note)).expect("value");
        assert_eq!(v["kind"], "session.note-set");
        assert_eq!(
            v["payload"].as_object().expect("obj").len(),
            0,
            "the public note-set carries an empty payload"
        );
        let raw = serde_json::to_string(&WireEvent::from_event_public(&note)).expect("string");
        // The note TEXT never crosses the public wire; the payload is empty. (The
        // `kind` token "session.note-set" legitimately contains "note" — assert on
        // the payload emptiness + the absent text, not a naive substring.)
        assert!(
            !raw.contains("Net closing in 5"),
            "the note text never crosses the public wire: {raw}"
        );
        assert!(v["payload"].as_object().expect("obj").is_empty());
    }

    #[test]
    fn public_control_events_carry_no_operator_id() {
        // `ncs.stalled`/`ncs.resumed` are public with an empty payload, because
        // the fold derives controlState from the KIND; `control.handed-off`'s
        // `newNcsAccountId` is an operator id, redacted to `{}`.
        let new_ncs = Uuid::from_u128(555);
        for (body, kind) in [
            (SessionEventBody::NcsStalled, "ncs.stalled"),
            (SessionEventBody::NcsResumed, "ncs.resumed"),
            (
                SessionEventBody::ControlHandedOff {
                    new_ncs_account_id: new_ncs,
                },
                "control.handed-off",
            ),
        ] {
            let event = SessionEvent {
                seq: 12,
                actor_id: Some(Uuid::from_u128(100)),
                at: 1_700_000_000_000,
                body,
            };
            let v = serde_json::to_value(WireEvent::from_event_public(&event)).expect("value");
            assert_eq!(v["kind"], kind);
            assert!(
                v["payload"].as_object().expect("obj").is_empty(),
                "public control payload is empty for {kind}"
            );
            // Neither the new-NCS operator id nor the envelope actor id leaks.
            let raw = serde_json::to_string(&WireEvent::from_event_public(&event)).expect("string");
            assert!(!raw.contains(&new_ncs.to_string()));
            assert!(!raw.contains(&Uuid::from_u128(100).to_string()));
        }
    }

    #[test]
    fn public_station_blocked_redacts_the_account_id() {
        // `station.blocked` carries an account id that must NEVER cross the
        // public wire, so its projection is empty and the account-less viewer
        // learns only the accompanying `checkin.removed` roster effect.
        let blocked_account = Uuid::from_u128(777);
        let event = SessionEvent {
            seq: 15,
            actor_id: Some(Uuid::from_u128(100)),
            at: 1_700_000_000_000,
            body: SessionEventBody::StationBlocked {
                account_id: blocked_account,
            },
        };
        let v = serde_json::to_value(WireEvent::from_event_public(&event)).expect("value");
        assert_eq!(v["kind"], "station.blocked");
        assert!(
            v["payload"].as_object().expect("obj").is_empty(),
            "public station.blocked payload must be empty"
        );
        // Neither the blocked account id nor the envelope actor id leaks.
        let raw = serde_json::to_string(&WireEvent::from_event_public(&event)).expect("string");
        assert!(!raw.contains(&blocked_account.to_string()));
        assert!(!raw.contains(&Uuid::from_u128(100).to_string()));
    }

    #[test]
    fn owner_worked_set_and_note_set_carry_the_full_payload() {
        // The OWNER serializer carries the worked-set target + net note verbatim
        // via the adapter serializer (wire/DB parity).
        let worked = SessionEvent {
            seq: 8,
            actor_id: Some(Uuid::from_u128(100)),
            at: 1_700_000_000_000,
            body: SessionEventBody::StationWorkedSet {
                check_in_id: Some(Uuid::from_u128(42)),
            },
        };
        let v = serde_json::to_value(WireEvent::from_event(&worked)).expect("value");
        assert_eq!(v["payload"], to_payload(&worked.body));
        assert_eq!(v["payload"]["checkInId"], Uuid::from_u128(42).to_string());

        let note = SessionEvent {
            seq: 9,
            actor_id: Some(Uuid::from_u128(100)),
            at: 1_700_000_000_000,
            body: SessionEventBody::SessionNoteSet {
                note: netroll_domain::check_in::parse_note("Net closing in 5").expect("valid"),
            },
        };
        let nv = serde_json::to_value(WireEvent::from_event(&note)).expect("value");
        assert_eq!(nv["payload"]["note"], "Net closing in 5");
    }

    #[test]
    fn public_roster_reordered_carries_only_the_order_list() {
        // The reorder's public projection IS the order list, since list position
        // is public radio data; it carries no precedence labels and no actorId.
        // A `roster.reordered` frame carries a permutation of ids and nothing
        // per-station at all, so there is no label here to withhold or widen.
        let reordered = SessionEvent {
            seq: 7,
            actor_id: Some(Uuid::from_u128(100)),
            at: 1_700_000_000_000,
            body: SessionEventBody::RosterReordered {
                order: vec![Uuid::from_u128(42), Uuid::from_u128(43)],
            },
        };
        let v = serde_json::to_value(WireEvent::from_event_public(&reordered)).expect("value");
        assert_eq!(v["kind"], "roster.reordered");
        assert_eq!(v["payload"]["order"][0], Uuid::from_u128(42).to_string());
        assert_eq!(v["payload"]["order"][1], Uuid::from_u128(43).to_string());
        assert_eq!(
            v["payload"].as_object().expect("obj").len(),
            1,
            "the public reorder carries only the order list"
        );
        let raw = serde_json::to_string(&WireEvent::from_event_public(&reordered)).expect("string");
        assert!(
            !raw.contains("actorId"),
            "no operator id on the reorder: {raw}"
        );
    }

    #[test]
    fn owner_roster_reordered_carries_the_full_order_payload() {
        // The OWNER serializer (from_event) carries the reorder payload verbatim
        // via the adapter serializer (wire/DB parity).
        let reordered = SessionEvent {
            seq: 7,
            actor_id: Some(Uuid::from_u128(100)),
            at: 1_700_000_000_000,
            body: SessionEventBody::RosterReordered {
                order: vec![Uuid::from_u128(43), Uuid::from_u128(42)],
            },
        };
        let v = serde_json::to_value(WireEvent::from_event(&reordered)).expect("value");
        assert_eq!(v["kind"], "roster.reordered");
        assert_eq!(v["payload"], to_payload(&reordered.body));
        assert_eq!(v["payload"]["order"][0], Uuid::from_u128(43).to_string());
    }

    #[test]
    fn public_checkin_removed_is_a_bare_checkin_id_tombstone() {
        let removed = SessionEvent {
            seq: 6,
            actor_id: Some(Uuid::from_u128(100)),
            at: 1_700_000_000_000,
            body: SessionEventBody::CheckinRemoved {
                check_in_id: Uuid::from_u128(42),
            },
        };
        let v = serde_json::to_value(WireEvent::from_event_public(&removed)).expect("value");
        assert_eq!(v["kind"], "checkin.removed");
        assert_eq!(v["payload"]["checkInId"], Uuid::from_u128(42).to_string());
        assert_eq!(
            v["payload"].as_object().expect("obj").len(),
            1,
            "the tombstone carries only checkInId"
        );
        let raw = serde_json::to_string(&WireEvent::from_event_public(&removed)).expect("string");
        assert!(
            !raw.contains("actorId"),
            "no operator id on the tombstone: {raw}"
        );
    }

    #[test]
    fn event_frame_is_type_discriminated_camelcase_with_db_identical_payload() {
        let event = checkin_event();
        let v = serde_json::to_value(ServerMessage::event(&event)).expect("serializes");

        assert_eq!(v["type"], "event");
        assert_eq!(v["seq"], 43);
        assert_eq!(v["kind"], "checkin.added");
        assert!(v["at"].is_string(), "at is an RFC 3339 string");
        assert_eq!(v["actorId"], Uuid::from_u128(100).to_string());
        // Wire/DB parity: the streamed payload equals the stored JSONB payload.
        assert_eq!(v["payload"], to_payload(&event.body));
        assert_eq!(v["payload"]["checkInId"], Uuid::from_u128(42).to_string());
        assert_eq!(
            v["payload"]["clientEventId"],
            Uuid::from_u128(9).to_string()
        );
    }

    #[test]
    fn actor_id_is_omitted_not_null_when_absent_and_closed_payload_is_empty() {
        let event = SessionEvent {
            seq: 2,
            actor_id: None,
            at: 1_700_000_000_000,
            body: SessionEventBody::SessionClosed,
        };
        let v = serde_json::to_value(ServerMessage::event(&event)).expect("serializes");
        assert_eq!(v["type"], "event");
        assert!(
            v.as_object().expect("object").get("actorId").is_none(),
            "actorId is omitted, not null, when the event has no actor"
        );
        assert_eq!(v["payload"], serde_json::json!({}));
    }

    #[test]
    fn http_element_is_byte_identical_to_ws_frame_body_minus_the_type_tag() {
        // "Byte-shape-identical" is proven by direct comparison of the two REAL
        // serialization call sites for the SAME event, not just by both routing
        // through `WireEvent` in the type system. A `#[serde]` tweak applied to
        // one call site and not the other fails here.
        let event = checkin_event();

        // The HTTP catch-up array element (bare `WireEvent`).
        let http_element = serde_json::to_value(WireEvent::from_event(&event)).expect("serializes");

        // The WS `event` frame body, with the `type` discriminator stripped.
        let mut ws_frame = serde_json::to_value(ServerMessage::event(&event)).expect("serializes");
        let ws_body = ws_frame.as_object_mut().expect("object");
        assert_eq!(
            ws_body.remove("type"),
            Some(serde_json::json!("event")),
            "the WS frame carries the type discriminator the HTTP element lacks"
        );

        let ws_body_value = serde_json::Value::Object(ws_body.clone());
        assert_eq!(
            http_element, ws_body_value,
            "the HTTP catch-up element and the WS event frame body (minus `type`) \
             must be byte-identical — same fields, same values, same omission rules"
        );
    }

    #[test]
    fn snapshot_wraps_the_identical_summary_body_under_a_type_tag() {
        let row = NetSessionRow {
            id: Uuid::from_u128(1),
            definition_id: Uuid::from_u128(7),
            definition_version: 3,
            definition_snapshot: DefinitionSnapshot {
                title: "Sunday Traffic Net".into(),
                description: None,
                connections: vec![netroll_domain::net::wire::NetConnectionWire {
                    id: Uuid::from_u128(0x16_04),
                    position: 0,
                    kind: "hf".into(),
                    planned_frequency_hz: Some(14_230_000),
                    band: Some("20m".into()),
                    mode: Some("ssb".into()),
                    repeater_offset_hz: None,
                    tone_mode: None,
                    tone_value: None,
                    node: None,
                    reflector: None,
                    network: None,
                    talkgroup: None,
                    label: None,
                    detail: None,
                }],
                net_category: "traffic".into(),
                net_type: "open".into(),
                country: None,
                state: None,
                grid: None,
            },
            lifecycle: SessionLifecycle::Live,
            started_at_millis: Some(1_700_000_000_000),
            closed_at_millis: None,
            control_state: netroll_domain::fold::ControlState::Active,
            active_ncs_account_id: None,
            stalled_at_millis: None,
            last_seq: 1,
            created_at_millis: 1_700_000_000_000,
            updated_at_millis: 1_700_000_000_000,
        };
        let folded = replay(
            &[SessionEvent {
                seq: 1,
                actor_id: Some(Uuid::from_u128(100)),
                at: 1_700_000_000_000,
                body: SessionEventBody::SessionStarted {
                    definition_id: Uuid::from_u128(7),
                    definition_version: 3,
                },
            }],
            0,
        );
        let summary = crate::http::net_sessions::build_summary(
            &row,
            &folded,
            netroll_domain::authz::Role::Owner,
        );
        let summary_value = serde_json::to_value(&summary).expect("summary serializes");

        let msg = ServerMessage::Snapshot {
            session: Box::new(summary),
        };
        let v = serde_json::to_value(msg).expect("message serializes");

        assert_eq!(v["type"], "snapshot");
        // The snapshot's `session` is byte-shape-identical to the REST summary.
        assert_eq!(v["session"], summary_value);
        assert_eq!(v["session"]["latestSeq"], 1);
        assert_eq!(v["session"]["lifecycle"], "live");
        // The staff snapshot carries the viewer's own resolved role.
        assert_eq!(v["session"]["viewerRole"], "owner");
    }

    // --- `via` across BOTH sockets ------------------------------

    fn checkin_with_via(via: Via) -> SessionEvent {
        let mut event = checkin_event();
        if let SessionEventBody::CheckinAdded { via: slot, .. } = &mut event.body {
            *slot = Some(via);
        }
        event
    }

    fn updated_with_via(via: Option<Via>) -> SessionEvent {
        SessionEvent {
            seq: 44,
            actor_id: Some(Uuid::from_u128(100)),
            at: 1_700_000_000_000,
            body: SessionEventBody::CheckinUpdated {
                check_in_id: Uuid::from_u128(42),
                callsign: parse_callsign("W1AW").expect("valid callsign"),
                name: None,
                location: None,
                grid: None,
                signal_report: None,
                staying: StayingStatus::InAndOut,
                precedence: netroll_domain::check_in::Precedence::Routine,
                traffic: None,
                notes: None,
                public_note: None,
                via,
                relayed_by: None,
            },
        }
    }

    #[test]
    fn the_public_added_delta_projects_via_which_the_compiler_would_not_have_forced() {
        // ⚠️ The public WS allowlist is exhaustive over event VARIANTS, not
        // over a variant's FIELDS, so THIS TEST is the guard, not rustc: a field
        // added to `checkin.added` once compiled GREEN and was silently
        // redacted.
        //
        // Asserted on the PUBLIC socket. An earlier mutation proof came back
        // green because it asserted on the OWNER one while the behaviour lived
        // in `public_payload`.
        let connection_id = Uuid::from_u128(0x16_05);
        let v = serde_json::to_value(WireEvent::from_event_public(&checkin_with_via(
            Via::Connection(connection_id),
        )))
        .expect("serializes");
        assert_eq!(v["payload"]["via"]["kind"], "connection");
        assert_eq!(
            v["payload"]["via"]["connectionId"],
            connection_id.to_string()
        );
    }

    #[test]
    fn the_public_updated_delta_projects_a_corrected_via() {
        let v = serde_json::to_value(WireEvent::from_event_public(&updated_with_via(Some(
            Via::Unlisted("a phone patch".to_owned()),
        ))))
        .expect("serializes");
        assert_eq!(v["payload"]["via"]["kind"], "unlisted");
        assert_eq!(v["payload"]["via"]["text"], "a phone patch");
    }

    #[test]
    fn a_public_delta_with_no_via_omits_the_key_rather_than_nulling_it() {
        // `serde_json`'s index returns `Null` for a MISSING key and for an
        // explicit null alike, so the absence assertion goes through
        // `contains_key` on the payload object.
        for event in [checkin_event(), updated_with_via(None)] {
            let v = serde_json::to_value(WireEvent::from_event_public(&event)).expect("serializes");
            let payload = v["payload"].as_object().expect("a payload object");
            assert!(
                payload.contains_key("checkInId"),
                "the payload serialized its always-present keys, so absence below means absence"
            );
            assert!(!payload.contains_key("via"));
        }
    }

    #[test]
    fn the_owner_delta_carries_via_verbatim_and_that_is_asserted_not_inherited() {
        // `WireEvent::from_event` serializes the STORED payload through
        // `to_payload`, so a new field auto-carries with no human decision. That
        // is the right answer here — but a field that rides in unexamined is how
        // the next one rides in unexamined too.
        let connection_id = Uuid::from_u128(0x16_05);
        let v = serde_json::to_value(WireEvent::from_event(&checkin_with_via(Via::Connection(
            connection_id,
        ))))
        .expect("serializes");
        assert_eq!(
            v["payload"]["via"]["connectionId"],
            connection_id.to_string()
        );
        assert_eq!(v["payload"]["via"]["kind"], "connection");
    }

    #[test]
    fn the_public_socket_still_redacts_everything_it_redacted_before_via_joined() {
        let v = serde_json::to_value(WireEvent::from_event_public(&checkin_with_via(
            Via::Connection(Uuid::from_u128(0x16_05)),
        )))
        .expect("serializes");
        let payload = v["payload"].as_object().expect("a payload object");
        for redacted in ["name", "location", "signalReport", "notes", "clientEventId"] {
            assert!(
                !payload.contains_key(redacted),
                "widening one field must not widen another: {redacted}"
            );
        }
    }

    // --- The relaying station is REFUSED on the public wire ------

    /// The relaying station used across these assertions. Deliberately a
    /// callsign whose bytes appear nowhere else in the fixture, so a
    /// substring search over the whole serialized frame is a real leak test
    /// rather than a coincidence.
    const RELAY_CALL: &str = "K7ZZQ";

    fn relayer() -> netroll_domain::callsign::Callsign {
        parse_callsign(RELAY_CALL).expect("valid callsign")
    }

    fn checkin_relayed() -> SessionEvent {
        let mut event = checkin_event();
        if let SessionEventBody::CheckinAdded {
            relayed_by: slot, ..
        } = &mut event.body
        {
            *slot = Some(relayer());
        }
        event
    }

    fn updated_relayed() -> SessionEvent {
        let mut event = updated_with_via(None);
        if let SessionEventBody::CheckinUpdated {
            relayed_by: slot, ..
        } = &mut event.body
        {
            *slot = Some(relayer());
        }
        event
    }

    #[test]
    fn the_public_added_delta_carries_neither_the_relay_key_nor_the_callsigns_bytes() {
        // Asserted AT THE WIRE BYTES and not at the type: the allowlist is
        // exhaustive over event VARIANTS, not over a variant's FIELDS, so
        // nothing but this makes the refusal true. On the PUBLIC socket, because
        // an earlier proof came back green asserting on the owner one.
        //
        // The bytes matter as much as the key: an author who projects the
        // callsign under another name leaks it just as completely.
        let frame = serde_json::to_string(&WireEvent::from_event_public(&checkin_relayed()))
            .expect("serializes");
        assert!(
            frame.contains("W1AW"),
            "the fixture genuinely serializes a check-in, so the next two assertions are not vacuous"
        );
        assert!(
            !frame.contains("relayedBy"),
            "a third-party station's callsign must not reach an unauthenticated page: {frame}"
        );
        assert!(
            !frame.contains(RELAY_CALL),
            "and not under any other key either: {frame}"
        );
    }

    #[test]
    fn the_public_updated_delta_carries_neither_the_relay_key_nor_the_callsigns_bytes() {
        let frame = serde_json::to_string(&WireEvent::from_event_public(&updated_relayed()))
            .expect("serializes");
        assert!(
            frame.contains("W1AW"),
            "the fixture genuinely serializes an edit"
        );
        assert!(!frame.contains("relayedBy"), "{frame}");
        assert!(!frame.contains(RELAY_CALL), "{frame}");
    }

    #[test]
    fn the_owner_deltas_do_carry_the_relaying_station_on_both_frames() {
        // `relayed_by` is set AT ADD and editable, so it joins BOTH owner
        // frames. Asserted here so the public refusal above cannot be satisfied
        // by the field simply never being projected anywhere.
        let added =
            serde_json::to_value(WireEvent::from_event(&checkin_relayed())).expect("serializes");
        assert_eq!(added["payload"]["relayedBy"], RELAY_CALL);
        let updated =
            serde_json::to_value(WireEvent::from_event(&updated_relayed())).expect("serializes");
        assert_eq!(updated["payload"]["relayedBy"], RELAY_CALL);
    }

    #[test]
    fn an_unrelayed_owner_delta_omits_the_key_rather_than_nulling_it() {
        // The omit-optional rule, which the owner frames get from
        // `skip_serializing_if`, and which is what keeps a relay-less event
        // byte-indistinguishable from one written before the field existed.
        let added =
            serde_json::to_value(WireEvent::from_event(&checkin_event())).expect("serializes");
        assert!(
            added["payload"]
                .as_object()
                .expect("obj")
                .get("relayedBy")
                .is_none(),
            "omitted, not null: {added}"
        );
        let updated = serde_json::to_value(WireEvent::from_event(&updated_with_via(None)))
            .expect("serializes");
        assert!(
            updated["payload"]
                .as_object()
                .expect("obj")
                .get("relayedBy")
                .is_none(),
            "omitted, not null: {updated}"
        );
    }
}
