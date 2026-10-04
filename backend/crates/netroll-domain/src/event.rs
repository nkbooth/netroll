// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
//! The session event log's entry type: an envelope plus a typed payload.
//!
//! `seq` and `at` are GIVEN, never generated here — the durable log assigns
//! `seq` inside its write transaction and the injected clock supplies `at`.
//! Serde-free by design: the camelCase wire seam lives at the adapter boundary.

use uuid::Uuid;

use crate::callsign::Callsign;
use crate::check_in::{
    CheckInSource, Location, Name, Note, Precedence, SignalReport, StayingStatus, TrafficCount,
};
use crate::net::connection::Via;
use crate::profile::Grid;

/// One ordered entry in a single session's append-only event log.
///
/// Maps 1:1 to the persisted `session_events` row. `session_id` is a row-scope
/// concern of the adapter, not carried here, because the fold is already scoped
/// to one session's log.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionEvent {
    /// Per-session monotonic sequence number the durable log assigns at append.
    /// The fold uses it as the idempotency guard and as `last_seq`.
    ///
    /// **`seq` is 1-indexed and the durable log MUST honour that: the first
    /// event in a session's log is `seq = 1`, never `0`.**
    /// [`crate::fold::SessionState::last_seq`] defaults to `0` to mean "no
    /// events applied yet", so a `seq == 0` event is indistinguishable from that
    /// sentinel and the idempotency guard silently drops it with no error. The
    /// fold is total and panic-free, so it must not defend against this; the
    /// log's `seq` assignment is the sole enforcement point.
    pub seq: u64,
    /// The account that caused the event, if any (system-originated events
    /// carry `None`).
    pub actor_id: Option<Uuid>,
    /// When the event occurred, epoch milliseconds — the domain's time currency,
    /// drawn from the injected clock at command time.
    pub at: u64,
    /// The typed payload; its variant determines [`SessionEventBody::kind`].
    pub body: SessionEventBody,
}

/// The typed payload of a [`SessionEvent`], one variant per session transition.
///
/// Additive-only: a new transition gets a NEW variant and a NEW `kind`, never a
/// repurposed field, because every payload already written must keep decoding.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionEventBody {
    /// The session went live, snapshotting its definition provenance
    /// (`definition_id` + `definition_version`, the stamp).
    ///
    /// **There is no session-level operating frequency in this payload.** A net
    /// has a SET of ways to reach it, each with its own frequency or none at
    /// all, and an internet-only net has none to give at start. The frozen
    /// `definition_snapshot` carries the connection set; a mid-session move is
    /// [`Self::FrequencyChanged`], which names the connection it moves.
    ///
    /// **A payload carrying the retired `operatingFrequencyHz` is REFUSED, not
    /// translated.** It names no connection, and there is none in its session's
    /// snapshot to attribute it to, because a required `connections` key makes
    /// every such snapshot fail to decode first. No legacy field is minted, and
    /// the refusal reaches the operator as `/errors/unreplayable-log`, not a 500.
    SessionStarted {
        /// The definition this session was spawned from.
        definition_id: Uuid,
        /// The definition version snapshotted at start (provenance).
        definition_version: i32,
    },
    /// One connection's operating frequency was changed mid-session.
    ///
    /// The event NAMES the connection it changes, so the fold never has to
    /// infer which entry of the snapshot's connection set it moves. See
    /// [`Self::SessionStarted`] for the historical reading of the shape that
    /// named none.
    FrequencyChanged {
        /// The snapshot connection whose frequency moved.
        connection_id: Uuid,
        /// The new operating frequency, exact Hz.
        operating_frequency_hz: i64,
    },
    /// A station was added to the roster. Idempotent by `check_in_id` in the
    /// fold. Carries the already-validated [`Callsign`] newtype —
    /// parsing happened at the command boundary, not here.
    CheckinAdded {
        /// Stable id the fold keys roster dedupe on.
        check_in_id: Uuid,
        /// The validated, normalized base call (no re-validation in the fold).
        callsign: Callsign,
        /// The originating client's optimistic-write id, echoed for
        /// reconciliation. Carried only; the reconciliation LOGIC is
        /// the frontend store's job.
        client_event_id: Option<Uuid>,
        /// The mode-shaped signal report captured at check-in, if any.
        /// Staff-only, enforced at the API boundary.
        signal_report: Option<SignalReport>,
        /// Whether the station stays for comments or is in-and-out. Defaults to
        /// `in-and-out` when omitted, and when folding a field-less event.
        staying: StayingStatus,
        /// The operator name captured at check-in, if any. A
        /// prefilled-then-committed value from the per-net roster memory is
        /// persisted here; a field-less event folds to `None`.
        name: Option<Name>,
        /// The free-text location captured at check-in, if any, mirroring
        /// `name`; `None` on a field-less event.
        location: Option<Location>,
        /// The Maidenhead grid locator captured at check-in, if any. A SECOND,
        /// independent field beside `location`: one is a place name a human
        /// reads, this is a machine-meaningful locator under
        /// [`crate::profile::parse_grid`]'s grammar. `None` on every event
        /// written before the field existed — the adapter's serde `default`
        /// supplies that, and no stored payload is ever rewritten.
        grid: Option<Grid>,
        /// Who created this entry — `staff` (an operator) or `self` (a
        /// participant self-check-in). A field-less event folds to
        /// [`CheckInSource::Staff`], which is correct because no self entry
        /// could exist before the field did. The adapter's payload seam carries
        /// the `#[serde(default)]`; the domain type stays serde-free.
        source: CheckInSource,
        /// WHICH way in this station arrived on. A connection
        /// in the session's frozen snapshot, named by its stable id, OR the
        /// operator's own words for a way the owner never listed.
        ///
        /// **`None` is a FACT, not a default.** It means *nobody recorded it*,
        /// which is genuinely different from *arrived on the connection the ADIF
        /// export describes* — and no code path may convert one into the other
        /// except the single export fallback in
        /// [`crate::net::wire::adif_fields_for_via_wire`]. A required field
        /// would force every existing path to fabricate a value, which is how
        /// `HF, 20m` gets stamped onto four hundred EchoLink QSOs in a file that
        /// reaches LoTW and cannot be recalled.
        ///
        /// Every event written before the field existed has no key and folds
        /// to `None`.
        via: Option<Via>,
        /// WHICH STATION passed this check-in's traffic.
        ///
        /// **`None` means *not relayed*.** A DISTINCT fact from
        /// [`Self::CheckinAdded::via`] beside it, which records how the traffic
        /// travelled rather than who passed it — one field cannot hold both
        /// without becoming ambiguous the first time they differ. See
        /// [`crate::fold::RosterEntry::relayed_by`] for why it is also neither
        /// `Role::Relay` nor `added_by`.
        ///
        /// A [`Callsign`] and not an account id: the ordinary relaying station
        /// holds no NetRoll account, and making it one would render the common
        /// case unrepresentable.
        ///
        /// Every event written before the field existed has no key and folds
        /// to `None`.
        relayed_by: Option<Callsign>,
    },
    /// A roster entry was edited in the detail modal. An edit is a new event,
    /// never a mutation of [`SessionEventBody::CheckinAdded`]. The payload
    /// carries the FULL
    /// post-edit editable field set (last-write-wins REPLACE); the fold derives
    /// per-field corrections by diffing old→new and NEVER stores them in the
    /// event. Keyed on the stable `check_in_id` (the callsign may itself be
    /// corrected, so it is never the roster key).
    CheckinUpdated {
        /// The stable id of the entry being edited (the roster key).
        check_in_id: Uuid,
        /// The post-edit callsign (a correction; validated at the boundary).
        callsign: Callsign,
        /// The post-edit operator name, if any (manual entry).
        name: Option<Name>,
        /// The post-edit free-text location, if any (manual entry).
        location: Option<Location>,
        /// The post-edit Maidenhead grid, if any. Independent of `location`: an
        /// edit may change either without touching the other. `None` decodes
        /// from every payload written before the field existed, so an older edit
        /// derives no grid correction on replay.
        grid: Option<Grid>,
        /// The post-edit mode-shaped signal report, if any.
        signal_report: Option<SignalReport>,
        /// The post-edit staying status (always present; defaults `in-and-out`).
        staying: StayingStatus,
        /// The post-edit traffic/emergency precedence; always present,
        /// defaulting to `routine`.
        precedence: Precedence,
        /// The post-edit traffic count. `None` means none declared.
        traffic: Option<TrafficCount>,
        /// The post-edit per-station STAFF note, if any. REPLACE semantics on
        /// the fold, and unlike the other editable fields it derives NO
        /// `Correction` — a note is running round commentary, not a corrected
        /// mis-entry.
        ///
        /// The OPERATOR-PRIVATE half of a split pair; it crosses no public
        /// surface. See [`SessionEventBody::CheckinUpdated::public_note`] for
        /// the other half and for why the persisted key was not renamed.
        notes: Option<Note>,
        /// The post-edit per-station PUBLIC note, if any. Free prose written FOR
        /// the observer, projected onto every roster surface including the
        /// account-less one. Same REPLACE semantics, same shared `parse_note`
        /// validation and same no-`Correction` rule as `notes`.
        ///
        /// Writing it needs the same `EditCheckIn` (Logger+) capability the
        /// staff note needs; the participant self path stays a staying toggle.
        public_note: Option<Note>,
        /// The post-edit way in — see
        /// [`SessionEventBody::CheckinAdded::via`] for what `None` means.
        ///
        /// `via` IS editable, through this ORDINARY edit path: no new event kind
        /// and no correction event. Like every
        /// other member of this payload it carries the FULL post-edit value, so
        /// a writer that fails to carry it forward WIPES it rather than merely
        /// leaving it un-editable.
        via: Option<Via>,
        /// The post-edit relaying station — see
        /// [`Self::CheckinAdded::relayed_by`] for what `None` means.
        ///
        /// Editable through this ORDINARY edit path: no new event kind and no
        /// correction event, the same posture `via` has. Like every other member
        /// of this payload it carries the FULL post-edit value, so a writer that
        /// fails to carry it forward WIPES it.
        relayed_by: Option<Callsign>,
    },
    /// A roster entry was removed — a tombstone. The fold DROPS the row from the
    /// projected roster while the append-only log retains the full history.
    /// Public-safe as a bare `{ checkInId }`.
    CheckinRemoved {
        /// The stable id of the entry to drop from the projected roster.
        check_in_id: Uuid,
    },
    /// The NCS reordered the shared roster. Carries the EXPLICIT ordered list of
    /// `check_in_id`s (a permutation), NOT a sort-strategy token,
    /// so the fold stays a dumb, total, deterministic projection that just applies
    /// the given order — replay-safe regardless of later precedence edits. The
    /// precedence-sort LOGIC lives at the command boundary
    /// ([`crate::fold::order_by_precedence`]), never in the fold.
    RosterReordered {
        /// The `check_in_id`s in their new display order. Ids not present on the
        /// roster are ignored; roster entries not named keep their relative order,
        /// appended after the ordered ones (the fold is total/panic-free).
        order: Vec<Uuid>,
    },
    /// The NCS moved the single working-station cursor. `Some(id)` sets the
    /// cursor to that on-roster entry, marking any DIFFERENT prior working entry
    /// `worked`;
    /// `None` clears it (completing the current station). An off-roster
    /// `Some(id)` is a total no-op in the fold. Carries no version bump and
    /// derives no corrections.
    ///
    /// The FOLD ARM reorders nothing: it moves the cursor and flips one `worked`
    /// bool, deriving no permutation. But marking a station worked does NOT
    /// leave the roster order unchanged — under the worked-sink ordering mode
    /// ([`SessionEventBody::RosterOrderModeSet`]) the COMMAND BOUNDARY appends a
    /// [`SessionEventBody::RosterReordered`] alongside this event, in the SAME
    /// transaction at the next `seq`.
    StationWorkedSet {
        /// The entry to make the working station, or `None` to clear the cursor.
        check_in_id: Option<Uuid>,
    },
    /// The net-level note was set or cleared. Folds to
    /// `SessionState.net_note` (REPLACE,
    /// last-write-wins). `None` clears it. The note TEXT is operator-only — its
    /// public projection is empty (never crosses the public wire).
    SessionNoteSet {
        /// The new net-level note, or `None` to clear it.
        note: Option<Note>,
    },
    /// The NCS set the session's ROSTER ORDERING MODE. Folds to
    /// [`crate::fold::SessionState::roster_order_mode`] (REPLACE,
    /// last-write-wins), mirroring [`SessionEventBody::SessionNoteSet`] exactly.
    ///
    /// The mode is DELIBERATELY INERT inside the fold: no fold arm reads it and
    /// no fold arm sorts. It records the standing operator INTENT, while the
    /// order itself continues to arrive only as an explicit
    /// [`SessionEventBody::RosterReordered`] permutation appended at the command
    /// boundary — which is what keeps replay deterministic against later
    /// precedence edits. Storing a strategy the fold re-evaluated at replay time
    /// would not.
    RosterOrderModeSet {
        /// The standing roster ordering mode from this event forward.
        mode: crate::fold::RosterOrderMode,
    },
    /// The session was closed. No payload — the raw event's own envelope
    /// carries `at`/`actor_id`, so the durable log retains when and by whom.
    /// Note this is a fact about the LOG, not the fold's projection:
    /// [`crate::fold::SessionState`] projects only `closed_at` (from
    /// `event.at`) and has no `closed_by` field — `event.actor_id` is not
    /// read by [`crate::fold::fold`] for this variant. Add a `closed_by` field
    /// to `SessionState` if "who closed it" ever has to be queryable from the
    /// projected state rather than recovered from the log.
    SessionClosed,
    /// The active NCS's presence heartbeat dropped past the stall threshold, so
    /// the session's control status went `Active → Stalled`. This is the
    /// ORTHOGONAL control-status axis — the session is
    /// still `Live` at the lifecycle level. No payload: the transition IS the
    /// fact, and the envelope's `at`/`actor_id` carry when/who. System-originated
    /// (minted by the presence monitor), so `actor_id` is `None`.
    NcsStalled,
    /// The stalled session's active NCS returned (a fresh presence heartbeat)
    /// before auto-close, so the control status went `Stalled → Active` under
    /// the SAME active NCS. No payload; system-originated (`actor_id = None`) —
    /// the presence monitor mints it.
    NcsResumed,
    /// Control of the session moved to a new active NCS. The ONE kind both
    /// handoff paths mint: a VOLUNTARY handoff by the current active
    /// NCS on a healthy session, and an INVOLUNTARY claim by an Owner/NetControl/
    /// Logger on a stalled session. Folds `control_state → Active` under the new
    /// controller. `actor_id` on the envelope is the operator who handed off or
    /// claimed; the payload carries the new active NCS's account id, an operator
    /// id redacted on the public wire.
    ControlHandedOff {
        /// The account id of the new active NCS (the handoff target, or the
        /// claimer themselves). Operator id — never crosses the public wire.
        new_ncs_account_id: Uuid,
    },
    /// The NCS blocked a disruptive account from re-checking-in for the rest of
    /// the session. Folds the account into
    /// [`crate::fold::SessionState::blocked_account_ids`], a deduped, session-
    /// scoped, ephemeral set the self-check-in path enforces against. Keyed on the
    /// ACCOUNT id (resolved server-side from the target's `added_by`), never the
    /// callsign string, so a re-attempt under a different callsign still fails.
    /// The payload's `account_id` is an operator/participant id — redacted from
    /// the public wire.
    StationBlocked {
        /// The account that may no longer self-check-in to this session.
        account_id: Uuid,
    },
}

impl SessionEventBody {
    /// The stable `noun.verb` past-tense kind token for this body.
    ///
    /// This `match` is the ONLY place the session-event kind vocabulary lives.
    /// Kinds are additive-only: a new transition gets a new variant and a new
    /// token here, never a repurposed field. There is no domain-side string →
    /// body parse, because a token alone cannot reconstruct a payload; the
    /// adapter deserializes `payload` then matches on `kind`.
    pub fn kind(&self) -> &'static str {
        match self {
            SessionEventBody::SessionStarted { .. } => "session.started",
            SessionEventBody::FrequencyChanged { .. } => "frequency.changed",
            SessionEventBody::CheckinAdded { .. } => "checkin.added",
            SessionEventBody::CheckinUpdated { .. } => "checkin.updated",
            SessionEventBody::CheckinRemoved { .. } => "checkin.removed",
            SessionEventBody::RosterReordered { .. } => "roster.reordered",
            SessionEventBody::StationWorkedSet { .. } => "station.worked-set",
            SessionEventBody::SessionNoteSet { .. } => "session.note-set",
            SessionEventBody::RosterOrderModeSet { .. } => "roster.order-mode-set",
            SessionEventBody::SessionClosed => "session.closed",
            SessionEventBody::NcsStalled => "ncs.stalled",
            SessionEventBody::NcsResumed => "ncs.resumed",
            SessionEventBody::ControlHandedOff { .. } => "control.handed-off",
            SessionEventBody::StationBlocked { .. } => "station.blocked",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn callsign(s: &str) -> Callsign {
        crate::callsign::parse_callsign(s).expect("valid test callsign")
    }

    #[test]
    fn kind_maps_each_variant_to_its_noun_verb_token() {
        for (body, token) in [
            (
                SessionEventBody::SessionStarted {
                    definition_id: Uuid::from_u128(1),
                    definition_version: 1,
                },
                "session.started",
            ),
            (
                SessionEventBody::FrequencyChanged {
                    connection_id: Uuid::from_u128(2),
                    operating_frequency_hz: 7_200_000,
                },
                "frequency.changed",
            ),
            (
                SessionEventBody::CheckinAdded {
                    check_in_id: Uuid::from_u128(2),
                    callsign: callsign("W1AW"),
                    client_event_id: None,
                    signal_report: None,
                    staying: StayingStatus::InAndOut,
                    name: None,
                    location: None,
                    grid: None,
                    source: CheckInSource::Staff,
                    via: None,
                    relayed_by: None,
                },
                "checkin.added",
            ),
            (
                SessionEventBody::CheckinUpdated {
                    check_in_id: Uuid::from_u128(2),
                    callsign: callsign("W1AW"),
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
                },
                "checkin.updated",
            ),
            (
                SessionEventBody::CheckinRemoved {
                    check_in_id: Uuid::from_u128(2),
                },
                "checkin.removed",
            ),
            (
                SessionEventBody::RosterReordered {
                    order: vec![Uuid::from_u128(2), Uuid::from_u128(3)],
                },
                "roster.reordered",
            ),
            (
                SessionEventBody::StationWorkedSet {
                    check_in_id: Some(Uuid::from_u128(2)),
                },
                "station.worked-set",
            ),
            (
                SessionEventBody::SessionNoteSet { note: None },
                "session.note-set",
            ),
            (
                SessionEventBody::RosterOrderModeSet {
                    mode: crate::fold::RosterOrderMode::WorkedSink,
                },
                "roster.order-mode-set",
            ),
            (SessionEventBody::SessionClosed, "session.closed"),
            (SessionEventBody::NcsStalled, "ncs.stalled"),
            (SessionEventBody::NcsResumed, "ncs.resumed"),
            (
                SessionEventBody::ControlHandedOff {
                    new_ncs_account_id: Uuid::from_u128(5),
                },
                "control.handed-off",
            ),
            (
                SessionEventBody::StationBlocked {
                    account_id: Uuid::from_u128(6),
                },
                "station.blocked",
            ),
        ] {
            assert_eq!(body.kind(), token);
        }
    }

    #[test]
    fn control_handed_off_carries_the_new_active_ncs_account_id() {
        // Both the voluntary handoff and the involuntary claim mint this ONE
        // kind, carrying the operator id of the new active NCS.
        let target = Uuid::from_u128(77);
        let body = SessionEventBody::ControlHandedOff {
            new_ncs_account_id: target,
        };
        assert_eq!(body.kind(), "control.handed-off");
        match body {
            SessionEventBody::ControlHandedOff { new_ncs_account_id } => {
                assert_eq!(new_ncs_account_id, target);
            }
            _ => panic!("expected ControlHandedOff"),
        }
    }

    #[test]
    fn ncs_stall_and_resume_are_payload_free_system_transitions() {
        // The fact IS the transition: the envelope's at/actor_id carry
        // when/who; these bodies hold no payload.
        assert_eq!(SessionEventBody::NcsStalled.kind(), "ncs.stalled");
        assert_eq!(SessionEventBody::NcsResumed.kind(), "ncs.resumed");
    }

    #[test]
    fn station_worked_set_carries_an_optional_cursor_target() {
        // Some(id) sets the cursor; None clears it (completes the current
        // station). The kind token is stable regardless of the payload.
        let set = SessionEventBody::StationWorkedSet {
            check_in_id: Some(Uuid::from_u128(9)),
        };
        let clear = SessionEventBody::StationWorkedSet { check_in_id: None };
        assert_eq!(set.kind(), "station.worked-set");
        assert_eq!(clear.kind(), "station.worked-set");
    }
}
