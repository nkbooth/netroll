// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
//! Deterministic fold/replay of a session's ordered event log into state.
//!
//! A TOTAL, panic-free PROJECTION that never rejects an ordering: scalars are
//! last-write-wins in `seq` order, the roster appends with dedupe by
//! `check_in_id`, and command-side legality lives in `session_sm.rs`, not here.

use std::collections::BTreeMap;

use uuid::Uuid;

use crate::callsign::Callsign;
use crate::check_in::{
    CheckInSource, Location, Name, Note, Precedence, SignalReport, StayingStatus, TrafficCount,
};
use crate::event::{SessionEvent, SessionEventBody};
use crate::net::connection::Via;
use crate::net::wire::NetConnectionWire;
use crate::profile::Grid;

/// Which editable field a [`Correction`] annotates. Wire form is
/// the lowercase-kebab token the adapter/frontend key the amber
/// annotation on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CorrectionField {
    /// The base callsign was corrected.
    Callsign,
    /// The operator name was set/changed/cleared.
    Name,
    /// The free-text location was set/changed/cleared.
    Location,
    /// The Maidenhead grid was set/changed/cleared. Distinct from
    /// [`CorrectionField::Location`]: correcting a mis-heard grid is a different
    /// audit fact from correcting a mis-typed town name.
    Grid,
    /// The signal report was set/changed/cleared.
    SignalReport,
    /// The staying status was toggled.
    Staying,
    /// The traffic/emergency precedence was changed.
    Precedence,
    /// The traffic count was set/changed/cleared.
    Traffic,
    /// The way the station came in on was set/changed/cleared. A `via` is a
    /// corrected MIS-ENTRY — "she was on the repeater, not HF" — not running
    /// commentary, so unlike the note pair it DOES derive an annotation.
    Via,
    /// The relaying station was set/changed/cleared. Like
    /// [`CorrectionField::Via`] and unlike the note pair, this is a corrected
    /// MIS-ENTRY — "it was W1ABC who relayed her, not W1ABD" — not running
    /// commentary, so it DOES derive an annotation.
    RelayedBy,
}

impl CorrectionField {
    /// The stable lowercase-kebab wire/storage token.
    pub fn as_str(self) -> &'static str {
        match self {
            CorrectionField::Callsign => "callsign",
            CorrectionField::Name => "name",
            CorrectionField::Location => "location",
            CorrectionField::Grid => "grid",
            CorrectionField::SignalReport => "signal-report",
            CorrectionField::Staying => "staying",
            CorrectionField::Precedence => "precedence",
            CorrectionField::Traffic => "traffic",
            CorrectionField::Via => "via",
            CorrectionField::RelayedBy => "relayedBy",
        }
    }
}

/// One side of a [`Correction`], in the form the surface that renders it reads.
///
/// Eight of the nine editable fields are already display strings by the time the
/// fold sees them. `via` is not: it is a connection UUID, and turning one into a
/// label needs the session's frozen connection SET — which the fold has never
/// seen and cannot be handed without changing `replay`'s signature at every one
/// of its call sites.
///
/// So the fold carries the STRUCTURED value for that one field and the
/// projection resolves it, and this sum type is what makes the projection unable
/// to forget: a consumer cannot reach for a `String` and silently print a UUID
/// on a surface. There is no `as_str`, deliberately.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CorrectionValue {
    /// A value the fold could already render for a reader.
    Text(String),
    /// A `via`, which becomes a label only against the session's connections.
    Via(Via),
}

/// One field correction, DERIVED by the fold when a `checkin.updated` changes a
/// field. A pure, deterministic, replayable projection —
/// NEVER stored in the event payload, so it reconstructs identically on any
/// replay/resume. `from`/`to` are what the surface renders (`None` = the
/// field was absent on that side); `at` is the editing event's `at` (epoch
/// millis).
///
/// `from`/`to` are [`CorrectionValue`] rather than `String`, for the one field
/// whose display form the fold cannot compute; everything else arrives as
/// [`CorrectionValue::Text`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Correction {
    /// Which editable field changed.
    pub field: CorrectionField,
    /// The prior value, or `None` when it was absent.
    pub from: Option<CorrectionValue>,
    /// The new value, or `None` when cleared.
    pub to: Option<CorrectionValue>,
    /// When the correction happened, epoch millis (the editing event's `at`).
    pub at: u64,
}

/// The projected lifecycle value of a session.
///
/// This is the fold's projection target only. The *guard* state machine that
/// decides which transitions are legal (scheduled → live → closed, rejecting
/// illegal ones) is `session_sm.rs`; this enum carries no legality
/// meaning on its own.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SessionLifecycle {
    /// Not yet started — the default before any `session.started` is folded.
    #[default]
    Scheduled,
    /// Started and running.
    Live,
    /// Closed.
    Closed,
}

/// The projected control status of a session: the ORTHOGONAL second axis on top
/// of [`SessionLifecycle`], NEVER a repurposed lifecycle value.
///
/// A `Live` session is independently `Active` or `Stalled`. `Active` means
/// the single active NCS's presence is fresh; `Stalled` means their presence
/// dropped past the threshold and nobody is running the net right now (the
/// session has NOT closed). Wire form is lowercase-kebab: `active`,
/// `stalled`. Defaults to `Active` — a fresh/legacy field-less session is active.
///
/// Like [`SessionLifecycle`] this enum is the fold's projection target only; the
/// GUARD deciding which control transitions are legal is `session_sm.rs`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ControlState {
    /// The active NCS is present; the net is running normally.
    #[default]
    Active,
    /// The active NCS's presence dropped; the net is paused pending resume,
    /// claim, or auto-close.
    Stalled,
}

impl ControlState {
    /// The stable lowercase-kebab wire/storage token — the single source
    /// of truth the adapter's `control_state` column persists.
    pub fn as_str(self) -> &'static str {
        match self {
            ControlState::Active => "active",
            ControlState::Stalled => "stalled",
        }
    }
}

/// The projected ROSTER ORDERING MODE of a session — the
/// standing operator intent the NCS toggles, folded from `roster.order-mode-set`
/// (REPLACE, last-write-wins). Wire form is lowercase-kebab: `manual`,
/// `worked-sink`. Defaults to `Manual`, so every session recorded before the
/// mode existed, and every new one, folds to that behaviour.
///
/// **This value is INERT inside [`fold`]**: no arm reads it and no arm sorts on
/// it. It records what the NCS asked for; the ORDER itself still arrives only as
/// an explicit `roster.reordered` permutation appended at the command boundary
/// (see [`partition_worked_last`]). That separation is what keeps replay
/// deterministic — a recorded permutation replays identically forever, whereas a
/// strategy re-evaluated at replay time against later precedence edits does not.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RosterOrderMode {
    /// The shipped behaviour: roster order changes only when the NCS issues an
    /// explicit reorder command. A cursor move or a new check-in leaves it alone.
    #[default]
    Manual,
    /// Worked stations sink below unworked ones and STAY sunk, re-applied by the
    /// command boundary after every trigger that can break the grouping.
    WorkedSink,
}

impl RosterOrderMode {
    /// The stable lowercase-kebab wire/storage token — the single source
    /// of truth the event payload persists and the HTTP surface validates.
    pub fn as_str(self) -> &'static str {
        match self {
            RosterOrderMode::Manual => "manual",
            RosterOrderMode::WorkedSink => "worked-sink",
        }
    }
}

/// Rejects a token outside the [`RosterOrderMode`] vocabulary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RosterOrderModeParseError;

impl core::fmt::Display for RosterOrderModeParseError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str("unrecognized roster order mode token")
    }
}

impl TryFrom<&str> for RosterOrderMode {
    type Error = RosterOrderModeParseError;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        match value {
            "manual" => Ok(RosterOrderMode::Manual),
            "worked-sink" => Ok(RosterOrderMode::WorkedSink),
            _ => Err(RosterOrderModeParseError),
        }
    }
}

/// One station on the live roster, projected from a `checkin.added` event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RosterEntry {
    /// The stable check-in id the fold dedupes on.
    pub check_in_id: Uuid,
    /// The validated, normalized base call.
    pub callsign: Callsign,
    /// When the check-in was added, epoch millis (the event's `at`).
    pub added_at: u64,
    /// The account that added it, if any (the event's `actor_id`). For a
    /// self-sourced entry this is the participant's OWN account — the ownership
    /// handle [`crate::authz::owns_check_in`] reads.
    pub added_by: Option<Uuid>,
    /// The `seq` of the `checkin.added` that created this entry —
    /// the third envelope fact beside `added_at` (the event's `at`) and
    /// `added_by` (its `actor_id`).
    ///
    /// It exists so a historical surface can ask what the connection set looked
    /// like WHEN this station checked in — [`SessionState::connections_at`] —
    /// because a worked station reports the frequency it was
    /// worked on, and a net that QSYs afterwards must not rewrite it.
    ///
    /// Never projected onto any wire: a log cursor is not a fact about the
    /// station, and every projection site `_`-binds it with that reason.
    pub added_seq: u64,
    /// Who created this entry — `staff` or `self`. Defaults to
    /// [`CheckInSource::Staff`] for a historical field-less add (additive-compat).
    /// Public provenance (crosses the public wire), not PII — the redacted public
    /// roster carries `source` but never `added_by`.
    pub source: CheckInSource,
    /// The mode-shaped signal report, if any.
    pub signal_report: Option<SignalReport>,
    /// Whether the station stays for comments or is in-and-out; defaults to
    /// `in-and-out` for a field-less event.
    pub staying: StayingStatus,
    /// The operator name. Set at `checkin.added` from the per-net roster-memory
    /// prefill or via an edit; `None` when neither
    /// supplied one.
    pub name: Option<Name>,
    /// The free-text location. Set at `checkin.added` from the roster-memory
    /// prefill or via an edit; `None` otherwise.
    pub location: Option<Location>,
    /// The Maidenhead grid locator. Deliberately SEPARATE from
    /// [`RosterEntry::location`]: that field is a free-text place name a human
    /// reads ("Hartford, CT"), this one is a machine-meaningful locator the
    /// `profile::parse_grid` grammar validates and canonicalizes (`"FN31pr"`). An
    /// operator may record either, both, or neither. `None` for every entry
    /// recorded before the field existed.
    pub grid: Option<Grid>,
    /// The traffic/emergency precedence; defaults to `Routine`
    /// at `checkin.added`, replaced by an edit.
    pub precedence: Precedence,
    /// The optional declared traffic count; `None` at add.
    pub traffic: Option<TrafficCount>,
    /// The per-station STAFF note, set via an edit; `None` at
    /// add. REPLACE semantics; derives NO correction (it is running commentary).
    ///
    /// The OPERATOR-PRIVATE half of a split pair: it crosses the owner surfaces
    /// and the operator's own exports, and it is redacted from every public
    /// projection exactly as
    /// before. `public_note` beside it is the half the observer reads.
    ///
    /// **The PERSISTED payload key is still `notes`, and that is deliberate.**
    /// Renaming it to `staff_note` would have forced either a decode alias on
    /// every historical `checkin.updated` event or a backfill of an append-only
    /// log — and, done carelessly, would have BLANKED every note written before
    /// the split rather than migrating it. Notes authored under the old
    /// assumption are private, so the encoding that makes that fall out by
    /// construction is the one that ships: the key stays, and its MEANING is
    /// now "the staff note". The user-facing labels are "Staff note" and "Public
    /// note" regardless; a wire key is not a label.
    pub notes: Option<Note>,
    /// The per-station PUBLIC note, set via an edit; `None` at add and for every
    /// entry folded from a log written before the field existed. REPLACE
    /// semantics; derives NO correction, exactly like `notes`.
    ///
    /// Written FOR the observer and projected onto every roster surface,
    /// including the account-less one — so it is validated by the SAME shared
    /// `parse_note` guard (bounded, control- and bidi-rejecting) that the staff
    /// note uses. It is the first free-prose field this project renders on an
    /// unauthenticated page.
    pub public_note: Option<Note>,
    /// Whether the working cursor has LEFT this entry. `false`
    /// at `checkin.added`; set `true` when the cursor moves away from it.
    /// Monotonic within a run — once worked, stays worked — until a
    /// remove+re-add starts a fresh entry. Drives the 0.55-opacity + green-tick
    /// treatment. The currently-working entry is NOT itself `worked`;
    /// the cursor render takes visual precedence.
    pub worked: bool,
    /// The fold-derived optimistic-concurrency version: `1` at
    /// `checkin.added`, `+1` on each applied `checkin.updated`. The edit command
    /// compares its `expectedVersion` against this (compare-and-swap); there is
    /// NO physical `version` column.
    pub version: u64,
    /// WHICH way in this station arrived on — a connection in
    /// the session's frozen snapshot, named by its stable id, or the operator's
    /// own words for a way the owner never listed.
    ///
    /// `None` means *nobody recorded it* and is a FACT in its own right, never
    /// interchangeable with "arrived on the ADIF-export connection". The label a
    /// surface renders is a PROJECTION of this, resolved against the session's
    /// connection set through `crate::net::wire::resolve_via`; the id is the key
    /// and no stored record carries a label in its place.
    ///
    /// `None` for every entry folded from a log written before the field existed.
    pub via: Option<Via>,
    /// WHICH STATION passed this check-in's traffic, when one
    /// did. `None` means *not relayed*.
    ///
    /// **Deliberately NOT [`RosterEntry::via`], and the distinction is the whole
    /// reason this field exists.** `via` records how the traffic TRAVELLED — a
    /// connection of the session's snapshot, or the operator's words for one the
    /// owner never listed. This records WHO PASSED IT. They are different facts
    /// and one field cannot hold both without becoming ambiguous the first time
    /// they differ. An entry may carry both, one,
    /// or neither, and setting one never reads or writes the other.
    ///
    /// **Deliberately NOT [`crate::authz::Role`]`::Relay`, which shares the word
    /// and nothing else.** `Role::Relay` is an ACCOUNT permitted to log check-ins
    /// on this session — the lowest staff tier. This is a STATION that passed one
    /// check-in's traffic on the air. A **Logger** sitting beside net control may
    /// log a check-in that W1ABC relayed; an account holding **`Role::Relay`**
    /// may log a check-in nobody relayed.
    ///
    /// **Deliberately NOT [`RosterEntry::added_by`]**, which this field does not
    /// repeal: `added_by` answers which ACCOUNT typed the entry
    /// and stays the attribution every source badge reads. This answers which
    /// STATION passed the traffic, and it is a [`Callsign`] rather than an
    /// account id precisely because the ordinary on-air relaying station holds no
    /// NetRoll account at all. For an operator who holds `Role::Relay` and logs a
    /// station they themselves relayed the two carry the same callsign, and that
    /// coincidence is why the difference is stated here rather than inferred.
    ///
    /// `None` for every entry folded from a log written before the field existed.
    pub relayed_by: Option<Callsign>,
    /// The derived per-field corrections, newest last. A pure
    /// fold projection, never carried in the event payload.
    pub corrections: Vec<Correction>,
}

/// One applied `frequency.changed`, in the order the fold applied it.
///
/// [`SessionState::connection_frequencies`] answers "where is this connection
/// NOW"; a run of these answers "where was it AS AT a seq", which is what a
/// logged check-in needs, and which a last-write-wins map cannot say.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FrequencyMove {
    /// The `seq` of the `frequency.changed` event.
    pub seq: u64,
    /// The snapshot connection it moved.
    pub connection_id: Uuid,
    /// The frequency it moved that connection to, exact Hz.
    pub operating_frequency_hz: i64,
}

/// The last-write-wins frequency map from an ordered run of moves, cut off at
/// `seq` inclusive.
///
/// `moves` must arrive in non-decreasing `seq` order — the fold's own
/// precondition, which is why [`SessionState::frequency_moves`] is ordered by
/// construction and the adapter's lateral selects `ORDER BY seq`. The function
/// does not sort: sorting here would hide a caller that broke the precondition.
/// A debug build names that caller instead — `take_while` on a broken run would
/// otherwise return a plausible WRONG map with no signal at all.
pub fn frequencies_as_at(
    moves: impl IntoIterator<Item = FrequencyMove>,
    seq: u64,
) -> BTreeMap<Uuid, i64> {
    let mut previous_seq = 0u64;
    moves
        .into_iter()
        .inspect(|m| {
            debug_assert!(
                m.seq >= previous_seq,
                "frequencies_as_at: moves must arrive in non-decreasing seq order \
                 (saw seq {} after seq {previous_seq})",
                m.seq
            );
            previous_seq = m.seq;
        })
        .take_while(|m| m.seq <= seq)
        .map(|m| (m.connection_id, m.operating_frequency_hz))
        .collect()
}

/// The ONE overlay: `snapshot` with `frequencies` laid over each connection's
/// `planned_frequency_hz`, extracted into its own overlay because a moved
/// station keeps its planned frequency until it is explicitly changed.
///
/// A connection nobody moved keeps the frequency the snapshot recorded for it,
/// which is why an absent key leaves the value alone rather than clearing it.
/// `pub` because three consumers need it — [`SessionState::live_connections`],
/// [`SessionState::connections_at`] and the adapter's per-`seq` history read —
/// and a private second copy in `netroll-adapters` is exactly the drift that
/// once put a stale connection set into `list_active_now`.
pub fn overlay_frequencies(
    snapshot: &[NetConnectionWire],
    frequencies: &BTreeMap<Uuid, i64>,
) -> Vec<NetConnectionWire> {
    snapshot
        .iter()
        .map(|connection| match frequencies.get(&connection.id) {
            Some(hz) => NetConnectionWire {
                planned_frequency_hz: Some(*hz),
                ..connection.clone()
            },
            None => connection.clone(),
        })
        .collect()
}

/// The state projected from a session's ordered event log.
///
/// Every field is the fold's OUTPUT. `last_seq` is the highest `seq` the fold
/// has applied (0 = no events applied) — the snapshot cursor, a
/// value not a count. Scalars are last-write-wins in `seq` order; `roster`
/// preserves stable insertion (`seq`) order.
///
/// **`last_seq == 0` is overloaded**: it means both "no events applied yet"
/// and, indistinguishably, "the last applied event had `seq == 0`." This is
/// safe only because `seq` is contractually 1-indexed (see
/// [`crate::event::SessionEvent::seq`]) — the durable log must never
/// assign `seq = 0`. `fold`/`replay` do not and cannot detect a violation of
/// that contract; a `seq == 0` event is silently absorbed as a no-op.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SessionState {
    /// The projected lifecycle.
    pub lifecycle: SessionLifecycle,
    /// When the session started, epoch millis, once `session.started` folded.
    pub started_at: Option<u64>,
    /// When the session closed, epoch millis, once `session.closed` folded.
    pub closed_at: Option<u64>,
    /// The frequency each snapshot connection is operating on RIGHT NOW, keyed
    /// by the connection's id.
    ///
    /// Empty until a `frequency.changed` names one: a connection nobody moved
    /// is still on the frequency the session's frozen `definition_snapshot`
    /// recorded for it, so an absent key means "unchanged", never "zero". Only
    /// a renderer that holds the snapshot can overlay this, and every one of
    /// them does.
    ///
    /// A `BTreeMap` rather than a `HashMap` so a folded state is `Eq` and its
    /// iteration order is stable — `SessionState` is compared whole in tests
    /// and in the WS resume path.
    pub connection_frequencies: BTreeMap<Uuid, i64>,
    /// Every applied `frequency.changed`, in `seq` order by construction — the
    /// fold's non-decreasing-`seq` precondition is what orders it, so nothing
    /// sorts it and nothing may.
    ///
    /// Kept BESIDE the last-write-wins map above rather than replacing it: a
    /// live surface wants "now" and reads the map; a historical surface wants
    /// "as at the seq this station checked in", and folds this run
    /// through [`Self::connections_at`]. A map of "now" cannot answer the
    /// second question, and the two are tied together by a test so they cannot
    /// drift.
    pub frequency_moves: Vec<FrequencyMove>,
    /// The definition this session was spawned from (provenance).
    pub definition_id: Option<Uuid>,
    /// The definition version snapshotted at start (provenance).
    pub definition_version: Option<i32>,
    /// The live roster in stable insertion order.
    pub roster: Vec<RosterEntry>,
    /// The single working-station cursor: the `check_in_id` of
    /// the station currently being worked, or `None` when none is. Because it is
    /// ONE optional value, "at most one station worked at a time" is
    /// a structural invariant — the fold cannot represent two. Moved only by
    /// `station.worked-set`; cleared when the working entry is removed.
    pub working_check_in_id: Option<Uuid>,
    /// The net-level note, set by `session.note-set` (REPLACE,
    /// last-write-wins). Operator-only — never crosses the public wire.
    pub net_note: Option<Note>,
    /// The standing roster ordering mode, set by
    /// `roster.order-mode-set` (REPLACE, last-write-wins). INERT here: nothing in
    /// [`fold`] reads it, so replay order is unaffected by its value. The command
    /// boundary reads it to decide whether to append a follow-on permutation.
    pub roster_order_mode: RosterOrderMode,
    /// The single server-authoritative active NCS. Set to the
    /// starter at `session.started` (the starter is always a definition owner),
    /// moved by `control.handed-off`. `None` on a session that never started, or
    /// an older session with no recoverable starter (treated as "no active
    /// NCS" — the safe/inert reading for the presence sweep). Operator id —
    /// redacted from the public summary.
    pub active_ncs_account_id: Option<Uuid>,
    /// The projected control status: `Active` (default) or
    /// `Stalled`. Orthogonal to `lifecycle`. Public radio data — a paused net is
    /// visibly frozen, so this DOES cross the public wire (unlike
    /// `active_ncs_account_id`).
    pub control_state: ControlState,
    /// When the session stalled, epoch millis (the `ncs.stalled` event's `at`),
    /// or `None` while `Active`. The auto-close clock reads this:
    /// the presence sweep auto-closes at `stalled_at + AUTO_CLOSE_MILLIS`.
    pub stalled_at: Option<u64>,
    /// The accounts the NCS blocked from re-checking-in for the rest of THIS
    /// session — a deduped set folded from `station.blocked`
    /// events. The self-check-in path refuses an acting account present here.
    /// Session-scoped and ephemeral: it is derived solely from this session's own
    /// log, so a fresh session for the same net definition starts empty. Keyed on
    /// the account id, never the callsign. Operator/participant ids — redacted
    /// from the public projection.
    pub blocked_account_ids: Vec<Uuid>,
    /// The highest `seq` applied — the snapshot cursor.
    pub last_seq: u64,
}

impl SessionState {
    /// The session's connection set as it stands RIGHT NOW: the frozen
    /// `snapshot` overlaid with every `frequency.changed` this fold has seen.
    ///
    /// The stored snapshot is never rewritten — the moves live in the
    /// event log, exactly as the retired session-level frequency's did — so
    /// every surface that serves a session's connections has to apply them on
    /// the way out. This is the ONE place a LIVE set is made: the session
    /// summary, the public session view, the close webhook's top-level
    /// `connections` array, the summary email's `Reachable on:` lines AND the
    /// public discovery card all call it, so a QSY'd net cannot read one
    /// frequency on its own page and another on the card that links to it (the
    /// drift that made this a method rather than a per-crate `.map`).
    ///
    /// **Not for the exports, and not for a per-station label.**
    /// Those answer "what was this station worked on", which is a fact about a
    /// moment, not about now; they take the frozen snapshot and call
    /// [`Self::connections_at`] with each entry's `added_seq` instead. Handing
    /// this set to a per-station resolver was a real defect this shape now
    /// prevents.
    ///
    /// A connection nobody moved keeps the frequency the snapshot recorded for
    /// it — see [`overlay_frequencies`] and [`Self::connection_frequencies`].
    pub fn live_connections(&self, snapshot: &[NetConnectionWire]) -> Vec<NetConnectionWire> {
        overlay_frequencies(snapshot, &self.connection_frequencies)
    }

    /// The session's connection set as it stood when the fold applied `seq`:
    /// the frozen `snapshot` overlaid with every
    /// `frequency.changed` whose `seq <= seq`, last write wins.
    ///
    /// Called with a [`RosterEntry::added_seq`], it answers the frequency that
    /// station was worked on. A move AFTER the check-in is excluded — that is
    /// the defect; a move BEFORE it, even before any check-in, is included; no
    /// move at all yields the snapshot's planned value, which is why every
    /// never-moving fixture is green under this AND under the two wrong
    /// implementations. At `last_seq` it equals [`Self::live_connections`],
    /// and a test pins that so the two cannot drift.
    pub fn connections_at(
        &self,
        seq: u64,
        snapshot: &[NetConnectionWire],
    ) -> Vec<NetConnectionWire> {
        overlay_frequencies(
            snapshot,
            &frequencies_as_at(self.frequency_moves.iter().copied(), seq),
        )
    }
}

/// Projects a single event onto the state, returning the new state.
///
/// Total and panic-free (never returns an error, never rejects an ordering).
/// Events whose `seq` does not advance `last_seq` (`seq <= last_seq`) are
/// no-ops — the general idempotency guard. On an applied event: scalars are
/// last-write-wins; a `checkin.added` appends a roster entry only if its
/// `check_in_id` is not already present; `last_seq` is set to the event's
/// `seq`.
///
/// **Precondition: `events` (across successive `fold` calls) must be applied
/// in non-decreasing `seq` order.** The guard is a plain numeric comparison
/// against `last_seq`, not a set/log-membership check — it cannot tell "a
/// `seq` I've already applied" apart from "a `seq` that arrives out of order
/// after a higher one." Folding a lower `seq` after a higher one has already
/// been applied silently discards that lower-`seq` event as if it were a
/// duplicate, even though it was never actually folded. Supplying an ordered
/// slice is [`replay`]'s and its caller's responsibility (the adapter's query is
/// expected to `ORDER BY seq`); `fold` itself does not sort or validate.
pub fn fold(mut state: SessionState, event: &SessionEvent) -> SessionState {
    // Monotonic idempotency guard: a non-advancing seq is inert for every kind.
    if event.seq <= state.last_seq {
        return state;
    }
    match &event.body {
        SessionEventBody::SessionStarted {
            definition_id,
            definition_version,
        } => {
            state.lifecycle = SessionLifecycle::Live;
            state.started_at = Some(event.at);
            state.definition_id = Some(*definition_id);
            state.definition_version = Some(*definition_version);
            // The starter (always a definition owner) is the
            // initial active NCS. Reading the envelope's actor_id here is
            // additive — the payload is unchanged. control_state defaults Active.
            state.active_ncs_account_id = event.actor_id;
            state.control_state = ControlState::Active;
            state.stalled_at = None;
        }
        SessionEventBody::FrequencyChanged {
            connection_id,
            operating_frequency_hz,
        } => {
            state
                .connection_frequencies
                .insert(*connection_id, *operating_frequency_hz);
            // The same move, kept in order, so a historical surface
            // can ask where this connection was AS AT a check-in's seq. Pushed
            // after the idempotency guard above, so the run is strictly
            // increasing in seq by construction.
            state.frequency_moves.push(FrequencyMove {
                seq: event.seq,
                connection_id: *connection_id,
                operating_frequency_hz: *operating_frequency_hz,
            });
        }
        // EXHAUSTIVE `_`-bound destructure, no `..` rest pattern: a new field on
        // `checkin.added` must fail to compile HERE until someone decides
        // whether the fold projects it onto `RosterEntry`, rather than being
        // silently dropped. That compile error is the guard working.
        //
        // `client_event_id` is the only field NOT projected, and deliberately:
        // it is the CALLER's idempotency token, consumed by the write path's
        // dedupe before the fold ever sees it, and it is not a fact about the
        // station.
        SessionEventBody::CheckinAdded {
            check_in_id,
            callsign,
            signal_report,
            staying,
            name,
            location,
            grid,
            source,
            via,
            relayed_by,
            client_event_id: _,
        } => {
            // Entity-level dedupe: skip the add if this check_in_id is already
            // on the roster, even at a distinct advancing seq.
            let already_present = state
                .roster
                .iter()
                .any(|entry| entry.check_in_id == *check_in_id);
            if !already_present {
                state.roster.push(RosterEntry {
                    check_in_id: *check_in_id,
                    callsign: callsign.clone(),
                    added_at: event.at,
                    added_by: event.actor_id,
                    // The third envelope fact — WHEN in the log this
                    // station arrived, so its frequency can be read as at that
                    // moment rather than as at close.
                    added_seq: event.seq,
                    // The staff/self provenance set at the write
                    // path; a historical field-less add carries Staff by default.
                    source: *source,
                    signal_report: signal_report.clone(),
                    staying: *staying,
                    // Name/location are captured at add time from the per-net
                    // roster memory prefill; a field-less/historical
                    // add folds them to None. Earlier they were set only
                    // via an edit.
                    name: name.clone(),
                    location: location.clone(),
                    // The grid captured at add time (quick-add, or the
                    // profile on a self check-in); an older add folds it to None.
                    grid: grid.clone(),
                    // Precedence/traffic are edit-only: the
                    // conservative Routine default, no declared traffic, at add.
                    precedence: Precedence::default(),
                    traffic: None,
                    // Both notes are edit-only;
                    // worked starts false — a fresh check-in has not yet been
                    // worked-and-left.
                    notes: None,
                    public_note: None,
                    worked: false,
                    // The way in, captured AT ADD — unlike the eight
                    // edit-only fields above. An older add folds to None,
                    // which means "nobody recorded it" and not "the export
                    // connection".
                    via: via.clone(),
                    // WHO passed the traffic, captured AT ADD beside
                    // the way in — a different fact from it, never a substitute
                    // for it. An older add folds to None, which means "not
                    // relayed".
                    relayed_by: relayed_by.clone(),
                    // The CAS version starts at 1; each applied edit bumps it.
                    version: 1,
                    corrections: Vec::new(),
                });
            }
        }
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
        } => {
            // Find the roster entry by its stable id; a no-op when absent,
            // because the fold is total and panic-free on an unknown id.
            if let Some(entry) = state
                .roster
                .iter_mut()
                .find(|entry| entry.check_in_id == *check_in_id)
            {
                let at = event.at;
                // Derive a Correction for each editable field whose value CHANGED
                // The audit annotation reconstructs identically on replay.
                if entry.callsign != *callsign {
                    entry.corrections.push(Correction {
                        field: CorrectionField::Callsign,
                        from: Some(CorrectionValue::Text(entry.callsign.as_str().to_owned())),
                        to: Some(CorrectionValue::Text(callsign.as_str().to_owned())),
                        at,
                    });
                }
                if entry.name != *name {
                    entry.corrections.push(Correction {
                        field: CorrectionField::Name,
                        from: entry
                            .name
                            .as_ref()
                            .map(|n| CorrectionValue::Text(n.as_str().to_owned())),
                        to: name
                            .as_ref()
                            .map(|n| CorrectionValue::Text(n.as_str().to_owned())),
                        at,
                    });
                }
                if entry.location != *location {
                    entry.corrections.push(Correction {
                        field: CorrectionField::Location,
                        from: entry
                            .location
                            .as_ref()
                            .map(|l| CorrectionValue::Text(l.as_str().to_owned())),
                        to: location
                            .as_ref()
                            .map(|l| CorrectionValue::Text(l.as_str().to_owned())),
                        at,
                    });
                }
                // The grid annotates BETWEEN location and
                // signal-report, holding the documented callsign→name→location→
                // grid→signal-report→staying→precedence→traffic order the TS
                // reducer mirrors byte-for-byte. Comparing `Option<Grid>`
                // directly is what keeps a historical entry (None vs None) from
                // fabricating a correction for a field it never had.
                if entry.grid != *grid {
                    entry.corrections.push(Correction {
                        field: CorrectionField::Grid,
                        from: entry
                            .grid
                            .as_ref()
                            .map(|g| CorrectionValue::Text(g.as_str().to_owned())),
                        to: grid
                            .as_ref()
                            .map(|g| CorrectionValue::Text(g.as_str().to_owned())),
                        at,
                    });
                }
                if entry.signal_report != *signal_report {
                    entry.corrections.push(Correction {
                        field: CorrectionField::SignalReport,
                        from: entry
                            .signal_report
                            .as_ref()
                            .map(|r| CorrectionValue::Text(r.as_str().to_owned())),
                        to: signal_report
                            .as_ref()
                            .map(|r| CorrectionValue::Text(r.as_str().to_owned())),
                        at,
                    });
                }
                if entry.staying != *staying {
                    entry.corrections.push(Correction {
                        field: CorrectionField::Staying,
                        from: Some(CorrectionValue::Text(entry.staying.as_str().to_owned())),
                        to: Some(CorrectionValue::Text(staying.as_str().to_owned())),
                        at,
                    });
                }
                // Precedence/traffic corrections are appended AFTER the existing
                // five field checks so the derived order stays deterministic and
                // the TS mirror matches byte-for-byte.
                if entry.precedence != *precedence {
                    entry.corrections.push(Correction {
                        field: CorrectionField::Precedence,
                        from: Some(CorrectionValue::Text(entry.precedence.as_str().to_owned())),
                        to: Some(CorrectionValue::Text(precedence.as_str().to_owned())),
                        at,
                    });
                }
                if entry.traffic != *traffic {
                    entry.corrections.push(Correction {
                        field: CorrectionField::Traffic,
                        from: entry
                            .traffic
                            .map(|t| CorrectionValue::Text(t.get().to_string())),
                        to: traffic.map(|t| CorrectionValue::Text(t.get().to_string())),
                        at,
                    });
                }
                // `via` annotates LAST, after the eight above, so the
                // derived order stays deterministic and the TS mirror matches
                // byte-for-byte. `from`/`to` carry the STRUCTURED value — the
                // fold cannot resolve a connection id to a label, and a
                // projection that could is the only place allowed to try.
                if entry.via != *via {
                    entry.corrections.push(Correction {
                        field: CorrectionField::Via,
                        from: entry.via.clone().map(CorrectionValue::Via),
                        to: via.clone().map(CorrectionValue::Via),
                        at,
                    });
                }
                // The relaying station annotates LAST, after `via`,
                // for the same determinism reason. It carries `Text` and NOT a
                // third `CorrectionValue` variant: a callsign is already the
                // text a reader reads, which is the whole reason `Via` needed a
                // variant of its own and this does not.
                if entry.relayed_by != *relayed_by {
                    entry.corrections.push(Correction {
                        field: CorrectionField::RelayedBy,
                        from: entry
                            .relayed_by
                            .as_ref()
                            .map(|c| CorrectionValue::Text(c.as_str().to_owned())),
                        to: relayed_by
                            .as_ref()
                            .map(|c| CorrectionValue::Text(c.as_str().to_owned())),
                        at,
                    });
                }
                // Last-write-wins REPLACE of the editable fields + version bump.
                entry.callsign = callsign.clone();
                entry.name = name.clone();
                entry.location = location.clone();
                entry.grid = grid.clone();
                entry.signal_report = signal_report.clone();
                entry.staying = *staying;
                entry.precedence = *precedence;
                entry.traffic = *traffic;
                // BOTH notes are REPLACED last-write-wins with NO correction
                // derived: a note is running
                // round commentary, not a corrected mis-entry, so neither a
                // `CorrectionField::Notes` nor a public-note member exists and no
                // annotation is pushed above. They are INDEPENDENT fields —
                // replacing one never moves the other.
                entry.notes = notes.clone();
                entry.public_note = public_note.clone();
                // REPLACE like every other member of the FULL
                // post-edit set — which is why a writer that fails to carry
                // `via` forward WIPES it rather than leaving it alone.
                entry.via = via.clone();
                // REPLACE, exactly like `via` — so a writer that
                // fails to carry the relaying station forward ERASES it. That is
                // what projection site 8 exists to prevent.
                entry.relayed_by = relayed_by.clone();
                entry.version += 1;
            }
        }
        SessionEventBody::CheckinRemoved { check_in_id } => {
            // Tombstone: drop the row from the PROJECTED roster; the append-only
            // log retains the full history. A no-op on an absent id.
            state
                .roster
                .retain(|entry| entry.check_in_id != *check_in_id);
            // A removed entry must not remain the phantom cursor target: clear
            // the cursor if it pointed at the removed row.
            if state.working_check_in_id == Some(*check_in_id) {
                state.working_check_in_id = None;
            }
        }
        SessionEventBody::RosterReordered { order } => {
            // A dumb, total projection: reorder `state.roster` so
            // entries appear in `order`'s sequence, then append any entry NOT
            // named in `order` in its current relative position. Unknown ids in
            // `order` are ignored. No version bump, no corrections — a reorder is
            // not a per-entry edit. The sort LOGIC lives at the command boundary
            // (`order_by_precedence`), never here.
            let mut remaining = std::mem::take(&mut state.roster);
            let mut reordered = Vec::with_capacity(remaining.len());
            for id in order {
                if let Some(pos) = remaining.iter().position(|entry| entry.check_in_id == *id) {
                    reordered.push(remaining.remove(pos));
                }
            }
            reordered.append(&mut remaining);
            state.roster = reordered;
        }
        SessionEventBody::StationWorkedSet { check_in_id } => {
            // Move the SINGLE working-cursor pointer. THIS ARM
            // never reorders the roster, bumps a version, or derives corrections —
            // it is a pointer move, not a permutation or a per-entry edit.
            //
            // That holds of the FOLD, not of the whole system: under
            // `RosterOrderMode::WorkedSink`
            // the COMMAND BOUNDARY appends a `roster.reordered` alongside this
            // event in the same transaction, so the roster order DOES change when
            // a station is marked worked. What stays true is that the change
            // arrives as a recorded permutation, never as a sort performed here.
            match check_in_id {
                Some(id) => {
                    // Only move to an id that is actually on the roster; an
                    // off-roster/unknown id is a TOTAL no-op (the fold-tolerance
                    // posture the reorder/edit arms use). The COMMAND boundary
                    // rejects unknown ids — this is defense in depth.
                    let on_roster = state.roster.iter().any(|e| e.check_in_id == *id);
                    if on_roster {
                        // Mark any DIFFERENT prior-working entry `worked` — the
                        // cursor has left it. Re-setting the SAME id leaves
                        // its `worked` false (it never left).
                        if let Some(prev) = state.working_check_in_id
                            && prev != *id
                            && let Some(entry) =
                                state.roster.iter_mut().find(|e| e.check_in_id == prev)
                        {
                            entry.worked = true;
                        }
                        state.working_check_in_id = Some(*id);
                    }
                }
                None => {
                    // Clear the cursor, marking the completed station worked.
                    if let Some(prev) = state.working_check_in_id
                        && let Some(entry) = state.roster.iter_mut().find(|e| e.check_in_id == prev)
                    {
                        entry.worked = true;
                    }
                    state.working_check_in_id = None;
                }
            }
        }
        SessionEventBody::SessionNoteSet { note } => {
            // REPLACE the net-level note last-write-wins; `None`
            // clears it. No version bump, no corrections (session-scoped).
            state.net_note = note.clone();
        }
        SessionEventBody::RosterOrderModeSet { mode } => {
            // REPLACE the ordering mode last-write-wins, mirroring
            // the net note. Deliberately the WHOLE arm: the fold records the
            // intent and never acts on it, so the roster order below is still
            // produced only by recorded `roster.reordered` permutations.
            state.roster_order_mode = *mode;
        }
        SessionEventBody::SessionClosed => {
            state.lifecycle = SessionLifecycle::Closed;
            state.closed_at = Some(event.at);
            // A closed session accepts no further `station.worked-set`
            // events (review finding): clear a lingering cursor
            // the same way `CheckinRemoved` does, so "currently working"
            // never renders as a permanent artifact on a closed roster.
            state.working_check_in_id = None;
            // A closed net is neither active nor stalled: reset
            // the control axis to its inert defaults so a closed session never
            // renders net-paused.
            state.control_state = ControlState::Active;
            state.stalled_at = None;
        }
        SessionEventBody::NcsStalled => {
            // The active NCS's presence dropped. Move the
            // control axis to Stalled and stamp the stall instant (the auto-close
            // clock). The lifecycle is UNTOUCHED — a stalled session is still Live.
            state.control_state = ControlState::Stalled;
            state.stalled_at = Some(event.at);
        }
        SessionEventBody::NcsResumed => {
            // The active NCS returned before auto-close. Back
            // to Active under the SAME active NCS; clear the stall instant.
            state.control_state = ControlState::Active;
            state.stalled_at = None;
        }
        SessionEventBody::ControlHandedOff { new_ncs_account_id } => {
            // A handoff or claim moves the active NCS and
            // always brings the net back to Active under the new controller.
            state.active_ncs_account_id = Some(*new_ncs_account_id);
            state.control_state = ControlState::Active;
            state.stalled_at = None;
        }
        SessionEventBody::StationBlocked { account_id } => {
            // Append the account to the session's blocklist,
            // deduped (append-if-absent) so re-blocking the same account keeps a
            // single entry. Session-scoped and ephemeral — derived solely from this
            // session's own log. No version bump, no corrections.
            if !state.blocked_account_ids.contains(account_id) {
                state.blocked_account_ids.push(*account_id);
            }
        }
    }
    state.last_seq = event.seq;
    state
}

/// Reconstructs session state by folding the events with `seq > since`.
///
/// `replay(log, 0)` is the full reconstruction from empty. `replay(log, since)`
/// is the delta a resuming client applies over its known state;
/// because [`fold`]'s guard makes non-advancing events inert, applying the tail
/// on top of state@since reproduces the authoritative state@latest — provided
/// the tail is drawn from the SAME session's log as the baseline state (the
/// guard has no way to detect a baseline/tail mismatch; scoping to one
/// session's log is the adapter's job, per [`SessionEvent`]'s doc).
///
/// **`events` must already be in non-decreasing `seq` order** — see [`fold`]'s
/// precondition note; `replay` filters and folds the slice as given, it does
/// not sort it.
pub fn replay(events: &[SessionEvent], since: u64) -> SessionState {
    events
        .iter()
        .filter(|event| event.seq > since)
        .fold(SessionState::default(), fold)
}

/// Computes the precedence display order for a roster: a PURE,
/// STABLE sort by [`Precedence::sort_rank`] — Emergency → Priority → Routine,
/// preserving each entry's existing relative (insertion/`added_at`) order within
/// a tier. Returns the `check_in_id`s in the resulting order — exactly the
/// `roster.reordered` payload the reorder command emits. No I/O; the fold applies
/// the returned permutation as a dumb projection (the sort logic is here, at the
/// command boundary, NOT in `fold`).
pub fn order_by_precedence(roster: &[RosterEntry]) -> Vec<Uuid> {
    let mut ordered: Vec<&RosterEntry> = roster.iter().collect();
    // `sort_by_key` is a STABLE sort, so entries sharing a rank keep their prior
    // relative order (the intra-tier tie-break).
    ordered.sort_by_key(|entry| entry.precedence.sort_rank());
    ordered.iter().map(|entry| entry.check_in_id).collect()
}

/// Stable-partitions a display `order` so worked stations sink below unworked
/// ones: a PURE partition on [`RosterEntry::worked`] that
/// preserves the relative order it is handed inside each group. Returns the
/// `check_in_id`s in the resulting order — exactly the `roster.reordered`
/// payload the command boundary emits. No I/O; the fold applies the returned
/// permutation as a dumb projection (the ordering logic is here, at the command
/// boundary, NOT in [`fold`]).
///
/// `order` is the display order to partition: the roster's own current order, or
/// the output of [`order_by_precedence`]. Composing the two is the whole of the
/// "worked-sink OUTER, precedence INNER" ruling — because the partition is
/// STABLE, whatever inner order it is handed survives, so no second piece of
/// "precedence ordering is active" state is needed or exists.
///
/// `working` is the session's working-cursor id. The entry holding it is EXEMPT
/// from the sink and stays with the unworked group, because [`RosterEntry::worked`]
/// is monotonic — nothing ever sets it back to false — so from the second
/// round onward the station the NCS is working RIGHT NOW is itself `worked`.
/// Reading the flag alone would file that station into the collapsed worked
/// block, where a default page load cannot see it. The exemption lives HERE,
/// at the command boundary, rather than as a fold-side reset of the flag: `worked`
/// is event-sourced, so clearing it would change what replay MEANS for every
/// historical log, while an ordering rule confined to this function changes only
/// the permutation already emitted. It is what makes "the NCS's next station is
/// the top of the unworked group" true rather than aspirational.
///
/// Total: the result is a permutation of `order` (same length, same set). An id
/// in `order` that is not on `roster` is treated as unworked, matching the fold's
/// own tolerance for ids it does not recognize.
pub fn partition_worked_last(
    roster: &[RosterEntry],
    working: Option<Uuid>,
    order: &[Uuid],
) -> Vec<Uuid> {
    let (worked, unworked): (Vec<Uuid>, Vec<Uuid>) = order.iter().partition(|id| {
        working != Some(**id)
            && roster
                .iter()
                .find(|entry| entry.check_in_id == **id)
                .is_some_and(|entry| entry.worked)
    });
    // `Iterator::partition` preserves the input order within each output, which
    // IS the stability the "precedence inner" ordering rests on.
    unworked.into_iter().chain(worked).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Reads the TEXT side of a correction value, for the eight fields whose
    /// display form the fold can compute. A `via` correction carries
    /// [`CorrectionValue::Via`] instead and is asserted structurally.
    fn text_of(value: Option<&CorrectionValue>) -> Option<&str> {
        match value? {
            CorrectionValue::Text(text) => Some(text.as_str()),
            CorrectionValue::Via(_) => None,
        }
    }

    fn callsign(s: &str) -> Callsign {
        crate::callsign::parse_callsign(s).expect("valid test callsign")
    }

    fn started(seq: u64, at: u64) -> SessionEvent {
        SessionEvent {
            seq,
            actor_id: Some(Uuid::from_u128(100)),
            at,
            body: SessionEventBody::SessionStarted {
                definition_id: Uuid::from_u128(7),
                definition_version: 3,
            },
        }
    }

    /// The snapshot connection the frequency fixtures move.
    fn connection(n: u128) -> Uuid {
        Uuid::from_u128(0xC0FFEE << 32 | n)
    }

    fn freq(seq: u64, at: u64, connection_n: u128, hz: i64) -> SessionEvent {
        SessionEvent {
            seq,
            actor_id: None,
            at,
            body: SessionEventBody::FrequencyChanged {
                connection_id: connection(connection_n),
                operating_frequency_hz: hz,
            },
        }
    }

    fn checkin(seq: u64, at: u64, id: u128, call: &str) -> SessionEvent {
        SessionEvent {
            seq,
            actor_id: Some(Uuid::from_u128(200)),
            at,
            body: SessionEventBody::CheckinAdded {
                check_in_id: Uuid::from_u128(id),
                callsign: callsign(call),
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
        }
    }

    /// A self-sourced `checkin.added` — the participant checked
    /// themselves in, so `source` is `SelfService` and `actor_id` is the
    /// participant's own account (the ownership handle `owns_check_in` reads).
    fn checkin_self(seq: u64, at: u64, id: u128, call: &str) -> SessionEvent {
        SessionEvent {
            seq,
            actor_id: Some(Uuid::from_u128(200)),
            at,
            body: SessionEventBody::CheckinAdded {
                check_in_id: Uuid::from_u128(id),
                callsign: callsign(call),
                client_event_id: None,
                signal_report: None,
                staying: StayingStatus::InAndOut,
                name: None,
                location: None,
                grid: None,
                source: CheckInSource::SelfService,
                via: None,
                relayed_by: None,
            },
        }
    }

    /// A `checkin.added` carrying a prefilled name/location — the
    /// identity-only fields the roster memory remembers, projected onto the
    /// roster entry at add time.
    fn checkin_named(
        seq: u64,
        at: u64,
        id: u128,
        call: &str,
        name: Option<&str>,
        location: Option<&str>,
    ) -> SessionEvent {
        SessionEvent {
            seq,
            actor_id: Some(Uuid::from_u128(200)),
            at,
            body: SessionEventBody::CheckinAdded {
                check_in_id: Uuid::from_u128(id),
                callsign: callsign(call),
                client_event_id: None,
                signal_report: None,
                staying: StayingStatus::InAndOut,
                name: name.map(|n| {
                    crate::check_in::parse_name(n)
                        .expect("valid")
                        .expect("non-blank")
                }),
                location: location.map(|l| {
                    crate::check_in::parse_location(l)
                        .expect("valid")
                        .expect("non-blank")
                }),
                grid: None,
                source: CheckInSource::Staff,
                via: None,
                relayed_by: None,
            },
        }
    }

    /// A `checkin.added` carrying a free-text location AND a Maidenhead grid
    /// — the two are independent fields, so the helper takes both.
    fn checkin_located(
        seq: u64,
        at: u64,
        id: u128,
        call: &str,
        location: Option<&str>,
        grid: Option<&str>,
    ) -> SessionEvent {
        SessionEvent {
            seq,
            actor_id: Some(Uuid::from_u128(200)),
            at,
            body: SessionEventBody::CheckinAdded {
                check_in_id: Uuid::from_u128(id),
                callsign: callsign(call),
                client_event_id: None,
                signal_report: None,
                staying: StayingStatus::InAndOut,
                name: None,
                location: location.map(|l| {
                    crate::check_in::parse_location(l)
                        .expect("valid")
                        .expect("non-blank")
                }),
                grid: grid.map(|g| crate::profile::parse_grid(g).expect("valid grid")),
                source: CheckInSource::Staff,
                via: None,
                relayed_by: None,
            },
        }
    }

    /// A `checkin.updated` that carries a location, a grid and a report, keeping
    /// the other editable fields at their unchanged baselines so a test can pin
    /// exactly which corrections derive and in what order.
    fn updated_grid(
        seq: u64,
        at: u64,
        id: u128,
        call: &str,
        location: Option<&str>,
        grid: Option<&str>,
        signal_report: Option<&str>,
    ) -> SessionEvent {
        SessionEvent {
            seq,
            actor_id: Some(Uuid::from_u128(300)),
            at,
            body: SessionEventBody::CheckinUpdated {
                check_in_id: Uuid::from_u128(id),
                callsign: callsign(call),
                name: None,
                location: location.map(|l| {
                    crate::check_in::parse_location(l)
                        .expect("valid")
                        .expect("non-blank")
                }),
                grid: grid.map(|g| crate::profile::parse_grid(g).expect("valid grid")),
                signal_report: signal_report.map(report),
                staying: StayingStatus::InAndOut,
                precedence: Precedence::Routine,
                traffic: None,
                notes: None,
                public_note: None,
                via: None,
                relayed_by: None,
            },
        }
    }

    fn closed(seq: u64, at: u64) -> SessionEvent {
        SessionEvent {
            seq,
            actor_id: Some(Uuid::from_u128(100)),
            at,
            body: SessionEventBody::SessionClosed,
        }
    }

    fn report(s: &str) -> SignalReport {
        crate::check_in::parse_signal_report(s)
            .expect("valid")
            .expect("non-blank")
    }

    #[allow(clippy::too_many_arguments)]
    fn updated(
        seq: u64,
        at: u64,
        id: u128,
        call: &str,
        name: Option<&str>,
        location: Option<&str>,
        signal_report: Option<&str>,
        staying: StayingStatus,
    ) -> SessionEvent {
        SessionEvent {
            seq,
            actor_id: Some(Uuid::from_u128(300)),
            at,
            body: SessionEventBody::CheckinUpdated {
                check_in_id: Uuid::from_u128(id),
                callsign: callsign(call),
                name: name.map(|n| {
                    crate::check_in::parse_name(n)
                        .expect("valid")
                        .expect("non-blank")
                }),
                location: location.map(|l| {
                    crate::check_in::parse_location(l)
                        .expect("valid")
                        .expect("non-blank")
                }),
                grid: None,
                signal_report: signal_report.map(report),
                staying,
                // Precedence/traffic default to the conservative baseline; the
                // dedicated `updated_precedence` helper exercises non-defaults.
                precedence: Precedence::Routine,
                traffic: None,
                // Notes default to None; the dedicated `updated_notes` helper
                // exercises the per-station note edit path.
                notes: None,
                public_note: None,
                via: None,
                relayed_by: None,
            },
        }
    }

    /// A `checkin.updated` that carries a specific precedence + traffic count,
    /// keeping the other editable fields at their current-unchanged values.
    #[allow(clippy::too_many_arguments)]
    fn updated_precedence(
        seq: u64,
        at: u64,
        id: u128,
        call: &str,
        precedence: Precedence,
        traffic: Option<i64>,
    ) -> SessionEvent {
        SessionEvent {
            seq,
            actor_id: Some(Uuid::from_u128(300)),
            at,
            body: SessionEventBody::CheckinUpdated {
                check_in_id: Uuid::from_u128(id),
                callsign: callsign(call),
                name: None,
                location: None,
                grid: None,
                signal_report: None,
                staying: StayingStatus::InAndOut,
                precedence,
                traffic: crate::check_in::parse_traffic_count(traffic).expect("valid traffic"),
                notes: None,
                public_note: None,
                via: None,
                relayed_by: None,
            },
        }
    }

    /// A `checkin.updated` that carries a per-station note, keeping
    /// the other editable fields at their current-unchanged baseline values.
    fn updated_notes(seq: u64, at: u64, id: u128, call: &str, notes: Option<&str>) -> SessionEvent {
        SessionEvent {
            seq,
            actor_id: Some(Uuid::from_u128(300)),
            at,
            body: SessionEventBody::CheckinUpdated {
                check_in_id: Uuid::from_u128(id),
                callsign: callsign(call),
                name: None,
                location: None,
                grid: None,
                signal_report: None,
                staying: StayingStatus::InAndOut,
                precedence: Precedence::Routine,
                traffic: None,
                notes: notes.map(|n| {
                    crate::check_in::parse_note(n)
                        .expect("valid")
                        .expect("non-blank")
                }),
                public_note: None,
                via: None,
                relayed_by: None,
            },
        }
    }

    /// A `checkin.updated` carrying BOTH per-station notes — a
    /// sibling of [`updated_notes`], not a rewrite of it, so the shipped
    /// staff-note cases keep their fixture unchanged.
    fn updated_both_notes(
        seq: u64,
        at: u64,
        id: u128,
        call: &str,
        notes: Option<&str>,
        public_note: Option<&str>,
    ) -> SessionEvent {
        let parse = |n: &str| {
            crate::check_in::parse_note(n)
                .expect("valid")
                .expect("non-blank")
        };
        SessionEvent {
            seq,
            actor_id: Some(Uuid::from_u128(300)),
            at,
            body: SessionEventBody::CheckinUpdated {
                check_in_id: Uuid::from_u128(id),
                callsign: callsign(call),
                name: None,
                location: None,
                grid: None,
                signal_report: None,
                staying: StayingStatus::InAndOut,
                precedence: Precedence::Routine,
                traffic: None,
                notes: notes.map(parse),
                public_note: public_note.map(parse),
                via: None,
                relayed_by: None,
            },
        }
    }

    /// A `station.worked-set` event: `Some(id)` sets the working cursor to that
    /// entry, `None` clears it.
    fn worked_set(seq: u64, at: u64, id: Option<u128>) -> SessionEvent {
        SessionEvent {
            seq,
            actor_id: Some(Uuid::from_u128(300)),
            at,
            body: SessionEventBody::StationWorkedSet {
                check_in_id: id.map(Uuid::from_u128),
            },
        }
    }

    /// A `session.note-set` event: `Some(text)` sets the net-level note, `None`
    /// clears it.
    fn note_set(seq: u64, at: u64, note: Option<&str>) -> SessionEvent {
        SessionEvent {
            seq,
            actor_id: Some(Uuid::from_u128(300)),
            at,
            body: SessionEventBody::SessionNoteSet {
                note: note.map(|n| {
                    crate::check_in::parse_note(n)
                        .expect("valid")
                        .expect("non-blank")
                }),
            },
        }
    }

    /// A `roster.order-mode-set` event: the session-scoped roster
    /// ordering mode, REPLACE/last-write-wins like the net note.
    fn order_mode_set(seq: u64, at: u64, mode: RosterOrderMode) -> SessionEvent {
        SessionEvent {
            seq,
            actor_id: Some(Uuid::from_u128(300)),
            at,
            body: SessionEventBody::RosterOrderModeSet { mode },
        }
    }

    fn reordered(seq: u64, at: u64, order: Vec<u128>) -> SessionEvent {
        SessionEvent {
            seq,
            actor_id: Some(Uuid::from_u128(300)),
            at,
            body: SessionEventBody::RosterReordered {
                order: order.into_iter().map(Uuid::from_u128).collect(),
            },
        }
    }

    fn removed(seq: u64, at: u64, id: u128) -> SessionEvent {
        SessionEvent {
            seq,
            actor_id: Some(Uuid::from_u128(300)),
            at,
            body: SessionEventBody::CheckinRemoved {
                check_in_id: Uuid::from_u128(id),
            },
        }
    }

    /// A `station.blocked` event: the NCS blocked an account from re-checking-in
    /// for the rest of the session.
    fn blocked(seq: u64, at: u64, account: u128) -> SessionEvent {
        SessionEvent {
            seq,
            actor_id: Some(Uuid::from_u128(300)),
            at,
            body: SessionEventBody::StationBlocked {
                account_id: Uuid::from_u128(account),
            },
        }
    }

    #[test]
    fn station_blocked_adds_the_account_to_the_blocklist() {
        // The blocked account is folded into the session's
        // blocklist so the self-check-in path can refuse it.
        let s = replay(&[started(1, 1_000), blocked(2, 2_000, 500)], 0);
        assert_eq!(s.blocked_account_ids, vec![Uuid::from_u128(500)]);
    }

    #[test]
    fn station_blocked_is_deduped_on_repeat() {
        // Folding the same account twice keeps a single entry — the blocklist is a
        // deduped set (append-if-absent), mirroring the roster add-dedupe posture.
        let s = replay(
            &[
                started(1, 1_000),
                blocked(2, 2_000, 500),
                blocked(3, 3_000, 500),
            ],
            0,
        );
        assert_eq!(s.blocked_account_ids, vec![Uuid::from_u128(500)]);
    }

    #[test]
    fn distinct_blocked_accounts_all_accumulate() {
        let s = replay(
            &[
                started(1, 1_000),
                blocked(2, 2_000, 500),
                blocked(3, 3_000, 501),
            ],
            0,
        );
        assert_eq!(
            s.blocked_account_ids,
            vec![Uuid::from_u128(500), Uuid::from_u128(501)]
        );
    }

    #[test]
    fn an_unrelated_event_leaves_the_blocklist_unchanged() {
        let s = replay(
            &[
                started(1, 1_000),
                blocked(2, 2_000, 500),
                checkin(3, 3_000, 42, "W1AW"),
            ],
            0,
        );
        assert_eq!(s.blocked_account_ids, vec![Uuid::from_u128(500)]);
    }

    #[test]
    fn a_fresh_session_has_an_empty_blocklist() {
        // The block is fold-derived from THIS session's own log: a
        // session with no block events (a fresh session for the same net) starts
        // with an empty blocklist — the block is session-scoped and ephemeral.
        let s = replay(&[started(1, 1_000)], 0);
        assert!(s.blocked_account_ids.is_empty());
        assert!(SessionState::default().blocked_account_ids.is_empty());
    }

    #[test]
    fn checkin_added_defaults_precedence_to_routine_and_traffic_to_none() {
        // Precedence/traffic are edit-only; a fresh check-in carries the
        // conservative Routine default and no declared traffic.
        let s = replay(&[started(1, 1_000), checkin(2, 2_500, 42, "W1AW")], 0);
        assert_eq!(s.roster[0].precedence, Precedence::Routine);
        assert_eq!(s.roster[0].traffic, None);
    }

    #[test]
    fn checkin_updated_replaces_precedence_and_traffic_and_derives_a_correction_for_each() {
        let s = replay(
            &[
                started(1, 1_000),
                checkin(2, 2_500, 42, "W1AW"),
                updated_precedence(3, 3_000, 42, "W1AW", Precedence::Emergency, Some(3)),
            ],
            0,
        );
        let entry = &s.roster[0];
        assert_eq!(entry.precedence, Precedence::Emergency);
        assert_eq!(entry.traffic.map(|t| t.get()), Some(3));
        // Version bumped once past the add's 1.
        assert_eq!(entry.version, 2);
        // A correction was derived for precedence (Routine -> Emergency) and for
        // traffic (none -> 3), appended AFTER the existing five field checks.
        let precedence_corr = entry
            .corrections
            .iter()
            .find(|c| c.field == CorrectionField::Precedence)
            .expect("precedence correction");
        assert_eq!(text_of(precedence_corr.from.as_ref()), Some("routine"));
        assert_eq!(text_of(precedence_corr.to.as_ref()), Some("emergency"));
        let traffic_corr = entry
            .corrections
            .iter()
            .find(|c| c.field == CorrectionField::Traffic)
            .expect("traffic correction");
        assert_eq!(traffic_corr.from, None);
        assert_eq!(text_of(traffic_corr.to.as_ref()), Some("3"));
    }

    #[test]
    fn checkin_updated_with_unchanged_precedence_and_traffic_derives_no_correction_for_them() {
        let s = replay(
            &[
                started(1, 1_000),
                checkin(2, 2_500, 42, "W1AW"),
                // Same defaults (Routine / no traffic) as the add.
                updated_precedence(3, 3_000, 42, "W1AW", Precedence::Routine, None),
            ],
            0,
        );
        let corr = &s.roster[0].corrections;
        assert!(!corr.iter().any(|c| c.field == CorrectionField::Precedence));
        assert!(!corr.iter().any(|c| c.field == CorrectionField::Traffic));
    }

    #[test]
    fn roster_reordered_applies_the_given_permutation() {
        // Three check-ins added in insertion order 42, 43, 44; a reorder puts
        // them in 44, 42, 43 sequence — the fold applies the permutation verbatim.
        let s = replay(
            &[
                started(1, 1_000),
                checkin(2, 2_100, 42, "W1AW"),
                checkin(3, 2_200, 43, "N1CCK"),
                checkin(4, 2_300, 44, "K1XYZ"),
                reordered(5, 3_000, vec![44, 42, 43]),
            ],
            0,
        );
        let ids: Vec<_> = s.roster.iter().map(|e| e.check_in_id).collect();
        assert_eq!(
            ids,
            vec![
                Uuid::from_u128(44),
                Uuid::from_u128(42),
                Uuid::from_u128(43)
            ]
        );
        // added_at is preserved so check-in order stays recoverable.
        let by_id = |id: u128| {
            s.roster
                .iter()
                .find(|e| e.check_in_id == Uuid::from_u128(id))
                .expect("present")
                .added_at
        };
        assert_eq!(by_id(42), 2_100);
        assert_eq!(by_id(44), 2_300);
    }

    #[test]
    fn roster_reordered_keeps_unlisted_entries_and_ignores_unknown_ids() {
        // Only 43 is named; 42 and 44 (unlisted) keep their current relative
        // order, appended AFTER the named ones. An unknown id (999) is ignored.
        let s = replay(
            &[
                started(1, 1_000),
                checkin(2, 2_100, 42, "W1AW"),
                checkin(3, 2_200, 43, "N1CCK"),
                checkin(4, 2_300, 44, "K1XYZ"),
                reordered(5, 3_000, vec![999, 43]),
            ],
            0,
        );
        let ids: Vec<_> = s.roster.iter().map(|e| e.check_in_id).collect();
        assert_eq!(
            ids,
            vec![
                Uuid::from_u128(43),
                Uuid::from_u128(42),
                Uuid::from_u128(44)
            ]
        );
    }

    #[test]
    fn roster_reordered_does_not_bump_version_or_derive_corrections() {
        let s = replay(
            &[
                started(1, 1_000),
                checkin(2, 2_100, 42, "W1AW"),
                checkin(3, 2_200, 43, "N1CCK"),
                reordered(4, 3_000, vec![43, 42]),
            ],
            0,
        );
        for entry in &s.roster {
            assert_eq!(entry.version, 1);
            assert!(entry.corrections.is_empty());
        }
        assert_eq!(s.last_seq, 4);
    }

    #[test]
    fn non_advancing_roster_reordered_is_inert() {
        let base = replay(
            &[
                started(1, 1_000),
                checkin(2, 2_100, 42, "W1AW"),
                checkin(3, 2_200, 43, "N1CCK"),
            ],
            0,
        );
        // seq <= last_seq: the reorder is absorbed as a no-op, order unchanged.
        let stale = fold(base.clone(), &reordered(3, 9_999, vec![43, 42]));
        assert_eq!(stale, base);
    }

    #[test]
    fn a_check_in_added_after_a_reorder_appends_at_the_end() {
        // A station checking in after a reorder lands last until the next reorder
        // — check-in order stays recoverable from added_at.
        let s = replay(
            &[
                started(1, 1_000),
                checkin(2, 2_100, 42, "W1AW"),
                checkin(3, 2_200, 43, "N1CCK"),
                reordered(4, 3_000, vec![43, 42]),
                checkin(5, 4_000, 44, "K1XYZ"),
            ],
            0,
        );
        let ids: Vec<_> = s.roster.iter().map(|e| e.check_in_id).collect();
        assert_eq!(
            ids,
            vec![
                Uuid::from_u128(43),
                Uuid::from_u128(42),
                Uuid::from_u128(44)
            ]
        );
    }

    #[test]
    fn order_by_precedence_is_a_stable_sort_emergency_priority_routine() {
        // Build a roster with mixed precedence in a deliberate insertion order,
        // then assert the stable sort outcome (not text).
        let mut state = replay(
            &[
                started(1, 1_000),
                checkin(2, 2_100, 1, "W1AW"),  // routine (default)
                checkin(3, 2_200, 2, "N1CCK"), // -> emergency
                checkin(4, 2_300, 3, "K1XYZ"), // routine
                checkin(5, 2_400, 4, "W2ABC"), // -> emergency
                checkin(6, 2_500, 5, "K5DEF"), // -> priority
            ],
            0,
        );
        state = fold(
            state,
            &updated_precedence(7, 3_000, 2, "N1CCK", Precedence::Emergency, None),
        );
        state = fold(
            state,
            &updated_precedence(8, 3_100, 4, "W2ABC", Precedence::Emergency, None),
        );
        state = fold(
            state,
            &updated_precedence(9, 3_200, 5, "K5DEF", Precedence::Priority, None),
        );

        let order = order_by_precedence(&state.roster);
        // Emergency tier first (ids 2 then 4 — their insertion order preserved),
        // then Priority (id 5), then Routine (ids 1 then 3 — insertion order).
        assert_eq!(
            order,
            vec![
                Uuid::from_u128(2),
                Uuid::from_u128(4),
                Uuid::from_u128(5),
                Uuid::from_u128(1),
                Uuid::from_u128(3),
            ]
        );
    }
    #[test]
    fn partition_worked_last_sinks_worked_entries_keeping_each_group_stable() {
        // A STABLE partition on `worked` — unworked first,
        // worked after, each group keeping the relative order it was handed.
        let mut state = replay(
            &[
                started(1, 1_000),
                checkin(2, 2_100, 1, "W1AW"),
                checkin(3, 2_200, 2, "N1CCK"),
                checkin(4, 2_300, 3, "K1XYZ"),
                checkin(5, 2_400, 4, "W2ABC"),
            ],
            0,
        );
        // Work 1 → 2 → 3, then clear: 1, 2 and 3 end up worked; 4 never was.
        state = fold(state, &worked_set(6, 3_000, Some(1)));
        state = fold(state, &worked_set(7, 3_100, Some(2)));
        state = fold(state, &worked_set(8, 3_200, Some(3)));
        state = fold(state, &worked_set(9, 3_300, None));

        let current: Vec<Uuid> = state.roster.iter().map(|e| e.check_in_id).collect();
        assert_eq!(
            partition_worked_last(&state.roster, state.working_check_in_id, &current),
            vec![
                Uuid::from_u128(4),
                Uuid::from_u128(1),
                Uuid::from_u128(2),
                Uuid::from_u128(3),
            ]
        );
    }

    #[test]
    fn partition_worked_last_is_stable_at_a_real_net_size() {
        // The stability property, exercised over a roster large enough that an
        // UNSTABLE sort is actually distinguishable from a stable one. Measured
        // 2026-08-29 by substituting `sort_unstable_by_key` for the partition:
        // a 4-station and a 30-station fixture BOTH still passed (the standard
        // library's unstable sort falls back to an insertion sort on short
        // slices, so it is stable in practice there); at 120 it failed. A small
        // fixture therefore cannot pin this property, however obvious it looks.
        let mut events = vec![started(1, 1_000)];
        for i in 0..120u128 {
            events.push(checkin(2 + i as u64, 2_000 + i as u64, i + 1, "W1AW"));
        }
        let mut state = replay(&events, 0);
        // Work every THIRD station, then clear, so worked and unworked interleave
        // across the whole roster rather than clustering at one end.
        let mut seq = 100;
        for i in (0..120u128).step_by(3) {
            state = fold(state, &worked_set(seq, 5_000 + seq, Some(i + 1)));
            seq += 1;
        }
        state = fold(state, &worked_set(seq, 5_000 + seq, None));

        let current: Vec<Uuid> = state.roster.iter().map(|e| e.check_in_id).collect();
        let order = partition_worked_last(&state.roster, state.working_check_in_id, &current);
        let expected_unworked: Vec<Uuid> = current
            .iter()
            .filter(|id| {
                !state
                    .roster
                    .iter()
                    .find(|e| e.check_in_id == **id)
                    .expect("on roster")
                    .worked
            })
            .copied()
            .collect();
        let expected_worked: Vec<Uuid> = current
            .iter()
            .filter(|id| {
                state
                    .roster
                    .iter()
                    .find(|e| e.check_in_id == **id)
                    .expect("on roster")
                    .worked
            })
            .copied()
            .collect();
        assert_eq!(
            order,
            expected_unworked
                .into_iter()
                .chain(expected_worked)
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn partition_worked_last_returns_a_total_permutation() {
        // The returned order names every entry exactly once — the fold
        // applies it as a permutation, so a lossy result would silently drop rows.
        let mut state = replay(
            &[
                started(1, 1_000),
                checkin(2, 2_100, 1, "W1AW"),
                checkin(3, 2_200, 2, "N1CCK"),
                checkin(4, 2_300, 3, "K1XYZ"),
            ],
            0,
        );
        state = fold(state, &worked_set(5, 3_000, Some(2)));
        state = fold(state, &worked_set(6, 3_100, Some(3)));

        let current: Vec<Uuid> = state.roster.iter().map(|e| e.check_in_id).collect();
        let order = partition_worked_last(&state.roster, state.working_check_in_id, &current);
        assert_eq!(order.len(), current.len());
        let mut sorted_order = order.clone();
        sorted_order.sort();
        let mut sorted_current = current.clone();
        sorted_current.sort();
        assert_eq!(sorted_order, sorted_current);
    }

    #[test]
    fn precedence_then_partition_orders_within_each_group_never_across_it() {
        // Worked-sink is the OUTER key, precedence the INNER one. Composing
        // the two pure command-boundary functions is the whole of that ruling —
        // no second "precedence mode is active" flag exists or is needed.
        let mut state = replay(
            &[
                started(1, 1_000),
                checkin(2, 2_100, 1, "W1AW"),  // routine
                checkin(3, 2_200, 2, "N1CCK"), // -> emergency
                checkin(4, 2_300, 3, "K1XYZ"), // routine
                checkin(5, 2_400, 4, "W2ABC"), // -> emergency
                checkin(6, 2_500, 5, "K5DEF"), // -> priority
            ],
            0,
        );
        state = fold(
            state,
            &updated_precedence(7, 3_000, 2, "N1CCK", Precedence::Emergency, None),
        );
        state = fold(
            state,
            &updated_precedence(8, 3_100, 4, "W2ABC", Precedence::Emergency, None),
        );
        state = fold(
            state,
            &updated_precedence(9, 3_200, 5, "K5DEF", Precedence::Priority, None),
        );
        // Work 1, then 2, then 3: 1 and 2 are worked; 3 holds the cursor.
        state = fold(state, &worked_set(10, 4_000, Some(1)));
        state = fold(state, &worked_set(11, 4_100, Some(2)));
        state = fold(state, &worked_set(12, 4_200, Some(3)));

        let by_precedence = order_by_precedence(&state.roster);
        let composed =
            partition_worked_last(&state.roster, state.working_check_in_id, &by_precedence);
        // Unworked group in precedence order (emergency 4, priority 5, routine 3),
        // then the worked group in precedence order (emergency 2, routine 1).
        assert_eq!(
            composed,
            vec![
                Uuid::from_u128(4),
                Uuid::from_u128(5),
                Uuid::from_u128(3),
                Uuid::from_u128(2),
                Uuid::from_u128(1),
            ]
        );
    }

    #[test]
    fn the_currently_working_station_stays_in_the_unworked_group() {
        // In the FIRST round the entry holding the cursor has never been
        // left behind, so it is not yet `worked` and the partition leaves it
        // with the unworked group on the flag alone. That is only half the
        // property — `worked` is monotonic, so from round 2 on the cursor
        // sits on an already-worked entry and the exemption in
        // `partition_worked_last` is what keeps it there. See
        // `a_second_round_keeps_the_station_being_worked_out_of_the_worked_group`.
        let mut state = replay(
            &[
                started(1, 1_000),
                checkin(2, 2_100, 1, "W1AW"),
                checkin(3, 2_200, 2, "N1CCK"),
                checkin(4, 2_300, 3, "K1XYZ"),
            ],
            0,
        );
        state = fold(state, &worked_set(5, 3_000, Some(1)));
        state = fold(state, &worked_set(6, 3_100, Some(2)));

        let current: Vec<Uuid> = state.roster.iter().map(|e| e.check_in_id).collect();
        let order = partition_worked_last(&state.roster, state.working_check_in_id, &current);
        assert_eq!(state.working_check_in_id, Some(Uuid::from_u128(2)));
        assert_eq!(
            order,
            vec![Uuid::from_u128(2), Uuid::from_u128(3), Uuid::from_u128(1),]
        );
    }

    #[test]
    fn a_second_round_keeps_the_station_being_worked_out_of_the_worked_group() {
        // `worked` is MONOTONIC: re-working a
        // station never clears it, so from round 2 onward the entry HOLDING the
        // cursor is itself `worked`. A partition that reads `worked` alone
        // therefore sinks the station the NCS is working RIGHT NOW into the
        // collapsed group — invisible on a default page load.
        let mut state = replay(
            &[
                started(1, 1_000),
                checkin(2, 2_100, 1, "W1AW"),
                checkin(3, 2_200, 2, "N1CCK"),
                checkin(4, 2_300, 3, "K1XYZ"),
            ],
            0,
        );
        // Round 1: work A, then B — which leaves A behind and sinks it.
        state = fold(state, &worked_set(5, 3_000, Some(1)));
        state = fold(state, &worked_set(6, 3_100, Some(2)));
        state = fold(state, &reordered(7, 3_150, vec![2, 3, 1]));
        // Round 2: the NCS calls A back. B is left behind; A re-holds the cursor
        // while still carrying the `worked` flag round 1 gave it.
        state = fold(state, &worked_set(8, 4_000, Some(1)));
        let a = state
            .roster
            .iter()
            .find(|e| e.check_in_id == Uuid::from_u128(1))
            .expect("A is on the roster");
        assert!(
            a.worked,
            "`worked` is monotonic — re-working a station never clears it"
        );

        let current: Vec<Uuid> = state.roster.iter().map(|e| e.check_in_id).collect();
        let order = partition_worked_last(&state.roster, state.working_check_in_id, &current);
        // C still awaits its turn, so it heads the unworked group; A follows it
        // there because it holds the cursor; only B — worked and left — sinks.
        assert_eq!(
            order,
            vec![Uuid::from_u128(3), Uuid::from_u128(1), Uuid::from_u128(2)]
        );
        // The load-bearing half, stated independently of the exact permutation:
        // the station being worked is NOT in the trailing worked run.
        let worked_run: Vec<Uuid> = order
            .iter()
            .rev()
            .take_while(|id| {
                state
                    .roster
                    .iter()
                    .find(|e| e.check_in_id == **id)
                    .is_some_and(|e| e.worked)
                    && state.working_check_in_id != Some(**id)
            })
            .copied()
            .collect();
        assert!(
            !worked_run.contains(&Uuid::from_u128(1)),
            "the station holding the cursor must never sink into the worked group"
        );
    }

    #[test]
    fn roster_order_mode_set_replaces_last_write_wins_from_a_manual_default() {
        // The mode is durable session-scoped state folded from its own
        // event, mirroring `session.note-set` → `net_note`. Every historical
        // session folds to `Manual`, so nothing that shipped changes behaviour.
        assert_eq!(
            SessionState::default().roster_order_mode,
            RosterOrderMode::Manual
        );
        let s = replay(
            &[
                started(1, 1_000),
                order_mode_set(2, 2_000, RosterOrderMode::WorkedSink),
            ],
            0,
        );
        assert_eq!(s.roster_order_mode, RosterOrderMode::WorkedSink);
        let s2 = fold(s, &order_mode_set(3, 3_000, RosterOrderMode::Manual));
        assert_eq!(s2.roster_order_mode, RosterOrderMode::Manual);
    }

    #[test]
    fn the_roster_order_mode_is_inert_in_the_fold() {
        // No fold arm reads the mode and no fold arm sorts. Replaying a
        // log that carries mode changes must yield the SAME roster order, worked
        // flags and cursor as the same log with those events stripped out — the
        // ordering only ever arrives as a recorded permutation, which is what
        // keeps replay deterministic against later precedence edits.
        let with_mode = vec![
            started(1, 1_000),
            checkin(2, 2_100, 1, "W1AW"),
            checkin(3, 2_200, 2, "N1CCK"),
            checkin(4, 2_300, 3, "K1XYZ"),
            order_mode_set(5, 3_000, RosterOrderMode::WorkedSink),
            worked_set(6, 3_100, Some(1)),
            worked_set(7, 3_200, Some(2)),
            reordered(8, 3_300, vec![3, 2, 1]),
            order_mode_set(9, 3_400, RosterOrderMode::Manual),
            checkin(10, 3_500, 4, "W2ABC"),
        ];
        let without_mode: Vec<SessionEvent> = with_mode
            .iter()
            .filter(|e| !matches!(e.body, SessionEventBody::RosterOrderModeSet { .. }))
            .cloned()
            .collect();

        let folded = replay(&with_mode, 0);
        let stripped = replay(&without_mode, 0);
        assert_eq!(
            folded
                .roster
                .iter()
                .map(|e| e.check_in_id)
                .collect::<Vec<_>>(),
            stripped
                .roster
                .iter()
                .map(|e| e.check_in_id)
                .collect::<Vec<_>>()
        );
        assert_eq!(
            folded.roster.iter().map(|e| e.worked).collect::<Vec<_>>(),
            stripped.roster.iter().map(|e| e.worked).collect::<Vec<_>>()
        );
        assert_eq!(folded.working_check_in_id, stripped.working_check_in_id);
        // Forcing the field to its default leaves the projection byte-identical
        // apart from the field itself — nothing downstream of the fold read it.
        let mut forced = folded.clone();
        forced.roster_order_mode = RosterOrderMode::default();
        forced.last_seq = stripped.last_seq;
        assert_eq!(forced.roster, stripped.roster);
    }

    #[test]
    fn checkin_added_starts_at_version_one_with_no_corrections() {
        let s = replay(&[started(1, 1_000), checkin(2, 2_500, 42, "W1AW")], 0);
        assert_eq!(s.roster[0].version, 1);
        assert!(s.roster[0].corrections.is_empty());
        assert_eq!(s.roster[0].name, None);
        assert_eq!(s.roster[0].location, None);
    }

    #[test]
    fn checkin_updated_replaces_editable_fields_and_bumps_version() {
        let s = replay(
            &[
                started(1, 1_000),
                checkin(2, 2_500, 42, "W1AW"),
                updated(
                    3,
                    3_000,
                    42,
                    "W1AX",
                    Some("Maria"),
                    Some("Hartford, CT"),
                    Some("599"),
                    StayingStatus::StayingForComments,
                ),
            ],
            0,
        );
        assert_eq!(s.roster.len(), 1);
        let entry = &s.roster[0];
        assert_eq!(entry.callsign, callsign("W1AX"));
        assert_eq!(entry.name.as_ref().map(|n| n.as_str()), Some("Maria"));
        assert_eq!(
            entry.location.as_ref().map(|l| l.as_str()),
            Some("Hartford, CT")
        );
        assert_eq!(
            entry.signal_report.as_ref().map(|r| r.as_str()),
            Some("599")
        );
        assert_eq!(entry.staying, StayingStatus::StayingForComments);
        // Version bumped once past the add's `1`.
        assert_eq!(entry.version, 2);
    }

    #[test]
    fn checkin_updated_derives_a_correction_per_changed_field_only() {
        let s = replay(
            &[
                started(1, 1_000),
                // Add with a report already set, so the report change is old->new.
                SessionEvent {
                    seq: 2,
                    actor_id: Some(Uuid::from_u128(200)),
                    at: 2_500,
                    body: SessionEventBody::CheckinAdded {
                        check_in_id: Uuid::from_u128(42),
                        callsign: callsign("W1AW"),
                        client_event_id: None,
                        signal_report: Some(report("339")),
                        staying: StayingStatus::InAndOut,
                        name: None,
                        location: None,
                        grid: None,
                        source: CheckInSource::Staff,
                        via: None,
                        relayed_by: None,
                    },
                },
                // Change callsign + report; keep staying the same (no annotation).
                updated(
                    3,
                    3_000,
                    42,
                    "W1AX",
                    None,
                    None,
                    Some("599"),
                    StayingStatus::InAndOut,
                ),
            ],
            0,
        );
        let corr = &s.roster[0].corrections;
        // Exactly two: callsign and signal-report changed; staying did not, and
        // name/location stayed None (unchanged) so neither annotates.
        assert_eq!(corr.len(), 2);
        assert_eq!(corr[0].field, CorrectionField::Callsign);
        assert_eq!(text_of(corr[0].from.as_ref()), Some("W1AW"));
        assert_eq!(text_of(corr[0].to.as_ref()), Some("W1AX"));
        assert_eq!(corr[0].at, 3_000);
        assert_eq!(corr[1].field, CorrectionField::SignalReport);
        assert_eq!(text_of(corr[1].from.as_ref()), Some("339"));
        assert_eq!(text_of(corr[1].to.as_ref()), Some("599"));
    }

    #[test]
    fn checkin_updated_with_all_fields_unchanged_derives_no_correction() {
        let s = replay(
            &[
                started(1, 1_000),
                checkin(2, 2_500, 42, "W1AW"),
                // Same callsign, still no name/location/report, same staying.
                updated(
                    3,
                    3_000,
                    42,
                    "W1AW",
                    None,
                    None,
                    None,
                    StayingStatus::InAndOut,
                ),
            ],
            0,
        );
        assert!(s.roster[0].corrections.is_empty());
        // Version still bumps even with no field change (an edit event applied).
        assert_eq!(s.roster[0].version, 2);
    }

    #[test]
    fn checkin_updated_on_an_absent_id_is_a_no_op() {
        let s = replay(
            &[
                started(1, 1_000),
                checkin(2, 2_500, 42, "W1AW"),
                updated(
                    3,
                    3_000,
                    999,
                    "N0BODY",
                    None,
                    None,
                    None,
                    StayingStatus::InAndOut,
                ),
            ],
            0,
        );
        assert_eq!(s.roster.len(), 1);
        assert_eq!(s.roster[0].callsign, callsign("W1AW"));
        assert_eq!(s.roster[0].version, 1);
        assert_eq!(s.last_seq, 3);
    }

    #[test]
    fn checkin_removed_drops_the_row_and_absent_id_is_a_no_op() {
        let s = replay(
            &[
                started(1, 1_000),
                checkin(2, 2_500, 42, "W1AW"),
                checkin(3, 3_000, 43, "N1CCK"),
                removed(4, 4_000, 42),
            ],
            0,
        );
        assert_eq!(s.roster.len(), 1);
        assert_eq!(s.roster[0].check_in_id, Uuid::from_u128(43));
        // A remove of an already-absent id is inert (still advances the cursor).
        let s2 = fold(s.clone(), &removed(5, 5_000, 999));
        assert_eq!(s2.roster.len(), 1);
        assert_eq!(s2.last_seq, 5);
    }

    #[test]
    fn a_removed_entry_re_added_at_a_new_seq_reappears_fresh() {
        // The append-only log retains history; the projected roster drops then
        // re-adds. The re-add starts a fresh version-1 entry.
        let s = replay(
            &[
                started(1, 1_000),
                checkin(2, 2_500, 42, "W1AW"),
                removed(3, 3_000, 42),
                checkin(4, 4_000, 42, "W1AW"),
            ],
            0,
        );
        assert_eq!(s.roster.len(), 1);
        assert_eq!(s.roster[0].version, 1);
    }

    #[test]
    fn non_advancing_seq_is_inert_for_the_new_kinds_too() {
        let base = replay(&[started(1, 1_000), checkin(2, 2_500, 42, "W1AW")], 0);
        // seq <= last_seq: both new kinds are absorbed as no-ops.
        let stale_update = fold(
            base.clone(),
            &updated(
                2,
                9_999,
                42,
                "W9XX",
                None,
                None,
                None,
                StayingStatus::StayingForComments,
            ),
        );
        let stale_remove = fold(base.clone(), &removed(1, 9_999, 42));
        assert_eq!(stale_update, base);
        assert_eq!(stale_remove, base);
    }

    #[test]
    fn default_state_is_empty_scheduled_with_zero_cursor() {
        let s = SessionState::default();
        assert_eq!(s.lifecycle, SessionLifecycle::Scheduled);
        assert_eq!(s.last_seq, 0);
        assert!(s.roster.is_empty());
        assert_eq!(s.started_at, None);
        assert!(s.connection_frequencies.is_empty());
    }

    #[test]
    fn session_started_sets_live_start_and_provenance_and_no_frequency() {
        let s = fold(SessionState::default(), &started(1, 1_000));
        assert_eq!(s.lifecycle, SessionLifecycle::Live);
        assert_eq!(s.started_at, Some(1_000));
        // Starting a session moves no frequency. An internet-only
        // net has none to give, and an RF one is already on the frequency its
        // frozen connection snapshot records.
        assert!(s.connection_frequencies.is_empty());
        assert_eq!(s.definition_id, Some(Uuid::from_u128(7)));
        assert_eq!(s.definition_version, Some(3));
        assert_eq!(s.last_seq, 1);
    }

    #[test]
    fn frequency_changed_moves_only_the_connection_it_names() {
        // The event addresses ONE connection, so the fold never
        // has to infer which entry of the snapshot's set it mutates — and a
        // three-way net does not have all three frequencies rewritten by one
        // QSY on one of them.
        let s = replay(
            &[
                started(1, 1_000),
                freq(2, 2_000, 1, 7_200_000),
                freq(3, 3_000, 2, 14_300_000),
                freq(4, 4_000, 1, 7_250_000),
            ],
            0,
        );
        assert_eq!(
            s.connection_frequencies.get(&connection(1)),
            Some(&7_250_000)
        );
        assert_eq!(
            s.connection_frequencies.get(&connection(2)),
            Some(&14_300_000)
        );
        assert_eq!(s.connection_frequencies.get(&connection(3)), None);
        assert_eq!(s.lifecycle, SessionLifecycle::Live);
        assert_eq!(s.last_seq, 4);
    }

    #[test]
    fn checkin_added_appends_one_roster_row_with_envelope_provenance() {
        let s = replay(&[started(1, 1_000), checkin(2, 2_500, 42, "W1AW")], 0);
        assert_eq!(s.roster.len(), 1);
        let entry = &s.roster[0];
        assert_eq!(entry.check_in_id, Uuid::from_u128(42));
        assert_eq!(entry.callsign, callsign("W1AW"));
        assert_eq!(entry.added_at, 2_500);
        assert_eq!(entry.added_by, Some(Uuid::from_u128(200)));
    }

    #[test]
    fn checkin_added_projects_signal_report_and_staying_onto_the_roster() {
        // The fold surfaces the two additive fields verbatim onto
        // the roster entry. A staff report + staying-for-comments.
        let report = crate::check_in::parse_signal_report("599")
            .expect("valid")
            .expect("non-blank");
        let s = replay(
            &[
                started(1, 1_000),
                SessionEvent {
                    seq: 2,
                    actor_id: Some(Uuid::from_u128(200)),
                    at: 2_500,
                    body: SessionEventBody::CheckinAdded {
                        check_in_id: Uuid::from_u128(42),
                        callsign: callsign("W1AW"),
                        client_event_id: None,
                        signal_report: Some(report.clone()),
                        staying: StayingStatus::StayingForComments,
                        name: None,
                        location: None,
                        grid: None,
                        source: CheckInSource::Staff,
                        via: None,
                        relayed_by: None,
                    },
                },
            ],
            0,
        );
        assert_eq!(s.roster.len(), 1);
        assert_eq!(s.roster[0].signal_report, Some(report));
        assert_eq!(s.roster[0].staying, StayingStatus::StayingForComments);
    }

    #[test]
    fn checkin_added_projects_name_and_location_onto_the_roster() {
        // The prefilled identity fields committed at add time are projected
        // verbatim onto the roster entry, unlike a field-less add, which still
        // folds them to None (see the test below).
        let s = replay(
            &[
                started(1, 1_000),
                checkin_named(2, 2_500, 42, "W1AW", Some("Maria"), Some("Hartford, CT")),
            ],
            0,
        );
        assert_eq!(s.roster.len(), 1);
        assert_eq!(s.roster[0].name.as_ref().map(|n| n.as_str()), Some("Maria"));
        assert_eq!(
            s.roster[0].location.as_ref().map(|l| l.as_str()),
            Some("Hartford, CT")
        );
        // Identity only: version stays 1 and NO corrections derive at add.
        assert_eq!(s.roster[0].version, 1);
        assert!(s.roster[0].corrections.is_empty());
    }

    #[test]
    fn a_field_less_checkin_added_folds_name_and_location_to_none() {
        // A callsign-only add carries no name or location and folds both to
        // None.
        let s = replay(&[started(1, 1_000), checkin(2, 2_500, 42, "W1AW")], 0);
        assert_eq!(s.roster[0].name, None);
        assert_eq!(s.roster[0].location, None);
    }

    #[test]
    fn a_named_add_then_an_edit_still_replaces_name_and_location_and_derives_corrections() {
        // A checkin.updated REPLACEs name/location and derives a correction when
        // they change from the add-time values.
        let s = replay(
            &[
                started(1, 1_000),
                checkin_named(2, 2_500, 42, "W1AW", Some("Maria"), Some("Hartford, CT")),
                updated(
                    3,
                    3_000,
                    42,
                    "W1AW",
                    Some("Maria K."),
                    Some("New Haven, CT"),
                    None,
                    StayingStatus::InAndOut,
                ),
            ],
            0,
        );
        let entry = &s.roster[0];
        assert_eq!(entry.name.as_ref().map(|n| n.as_str()), Some("Maria K."));
        assert_eq!(
            entry.location.as_ref().map(|l| l.as_str()),
            Some("New Haven, CT")
        );
        assert_eq!(entry.version, 2);
        // A correction was derived for the changed name (Maria -> Maria K.)…
        let name_corr = entry
            .corrections
            .iter()
            .find(|c| c.field == CorrectionField::Name)
            .expect("name correction");
        assert_eq!(text_of(name_corr.from.as_ref()), Some("Maria"));
        assert_eq!(text_of(name_corr.to.as_ref()), Some("Maria K."));
        // …and for the changed location.
        let loc_corr = entry
            .corrections
            .iter()
            .find(|c| c.field == CorrectionField::Location)
            .expect("location correction");
        assert_eq!(text_of(loc_corr.from.as_ref()), Some("Hartford, CT"));
        assert_eq!(text_of(loc_corr.to.as_ref()), Some("New Haven, CT"));
    }

    // --- The per-check-in Maidenhead grid ----------------

    #[test]
    fn checkin_added_projects_a_grid_onto_the_roster_row() {
        // The grid is a SECOND, independent field alongside
        // the free-text location — both are set on the SAME add and both survive
        // the fold, which is what makes grid distinct from location.
        let s = replay(
            &[
                started(1, 1_000),
                checkin_located(2, 2_500, 42, "W1AW", Some("Hartford, CT"), Some("FN31pr")),
            ],
            0,
        );
        assert_eq!(s.roster.len(), 1);
        assert_eq!(
            s.roster[0].location.as_ref().map(|l| l.as_str()),
            Some("Hartford, CT")
        );
        assert_eq!(
            s.roster[0].grid.as_ref().map(|g| g.as_str()),
            Some("FN31pr")
        );
        // Identity only: version stays 1 and NO correction derives at add.
        assert_eq!(s.roster[0].version, 1);
        assert!(s.roster[0].corrections.is_empty());
    }

    #[test]
    fn checkin_updated_replaces_the_grid_and_derives_a_grid_correction() {
        // An edit REPLACEs the grid and derives exactly one
        // Grid correction — and it sits BETWEEN the location and signal-report
        // corrections, the ordinal position the TS reducer mirrors byte-for-byte.
        let s = replay(
            &[
                started(1, 1_000),
                checkin_located(2, 2_500, 42, "W1AW", Some("Hartford, CT"), Some("FN31")),
                // Change location, grid AND report in ONE edit so the derived
                // vector actually contains all three and the ordering is testable.
                updated_grid(
                    3,
                    3_000,
                    42,
                    "W1AW",
                    Some("New Haven, CT"),
                    Some("FN42"),
                    Some("599"),
                ),
            ],
            0,
        );
        let entry = &s.roster[0];
        assert_eq!(entry.grid.as_ref().map(|g| g.as_str()), Some("FN42"));
        assert_eq!(entry.version, 2);
        let fields: Vec<CorrectionField> = entry.corrections.iter().map(|c| c.field).collect();
        assert_eq!(
            fields,
            vec![
                CorrectionField::Location,
                CorrectionField::Grid,
                CorrectionField::SignalReport,
            ],
            "grid annotates between location and signal-report"
        );
        let grid_corr = entry
            .corrections
            .iter()
            .find(|c| c.field == CorrectionField::Grid)
            .expect("grid correction");
        assert_eq!(text_of(grid_corr.from.as_ref()), Some("FN31"));
        assert_eq!(text_of(grid_corr.to.as_ref()), Some("FN42"));
        assert_eq!(grid_corr.at, 3_000);
    }

    #[test]
    fn clearing_a_grid_derives_a_correction_to_none() {
        // A cleared grid is `None` (absent), not `""`, and the
        // annotation records the clear.
        let s = replay(
            &[
                started(1, 1_000),
                checkin_located(2, 2_500, 42, "W1AW", None, Some("FN31")),
                updated_grid(3, 3_000, 42, "W1AW", None, None, None),
            ],
            0,
        );
        let entry = &s.roster[0];
        assert_eq!(entry.grid, None);
        let grid_corrections: Vec<&Correction> = entry
            .corrections
            .iter()
            .filter(|c| c.field == CorrectionField::Grid)
            .collect();
        assert_eq!(grid_corrections.len(), 1);
        assert_eq!(text_of(grid_corrections[0].from.as_ref()), Some("FN31"));
        assert_eq!(grid_corrections[0].to, None);
    }

    #[test]
    fn a_historical_entry_with_no_grid_replays_with_grid_absent_and_no_grid_correction() {
        // Every event here is exactly the shape a log written before the grid
        // field carries: `grid: None` on both the add and the update, which is
        // what such a payload decodes to. The update changes ONLY `staying`, so
        // the ONLY correction that may derive is Staying — a fabricated
        // "Correcting grid: (was nothing)" is a replay-fidelity failure, and
        // `None != None` must stay false.
        let events = [
            started(1, 1_000),
            checkin(2, 2_500, 42, "W1AW"),
            updated(
                3,
                3_000,
                42,
                "W1AW",
                None,
                None,
                None,
                StayingStatus::StayingForComments,
            ),
        ];
        let s = replay(&events, 0);
        let entry = &s.roster[0];
        assert_eq!(entry.grid, None, "a historical entry has NO grid");
        let fields: Vec<CorrectionField> = entry.corrections.iter().map(|c| c.field).collect();
        assert_eq!(
            fields,
            vec![CorrectionField::Staying],
            "only the field that actually changed annotates"
        );
    }

    #[test]
    fn duplicate_check_in_id_at_advancing_seq_does_not_grow_roster() {
        let s = replay(
            &[
                started(1, 1_000),
                checkin(2, 2_500, 42, "W1AW"),
                checkin(3, 3_000, 42, "W1AW"),
            ],
            0,
        );
        assert_eq!(s.roster.len(), 1);
        assert_eq!(s.last_seq, 3);
    }

    #[test]
    fn distinct_check_in_ids_preserve_seq_insertion_order() {
        let s = replay(
            &[
                started(1, 1_000),
                checkin(2, 2_500, 42, "W1AW"),
                checkin(3, 3_000, 43, "N1CCK"),
            ],
            0,
        );
        assert_eq!(s.roster.len(), 2);
        assert_eq!(s.roster[0].check_in_id, Uuid::from_u128(42));
        assert_eq!(s.roster[1].check_in_id, Uuid::from_u128(43));
    }

    // CHARACTERIZATION / CONFINEMENT — this PASSES at
    // baseline and is not a red test. The fold's dedupe is entity-level and
    // keyed on `check_in_id` alone; no callsign-uniqueness constraint has ever
    // existed. Making duplicate check-ins VISIBLE in the console must never be
    // taken to imply making them SINGULAR here, because each roster slot owns
    // its own report/precedence/traffic/notes/worked state, and the ADIF export
    // emits one record per roster entry — collapsing two check-ins of one
    // station onto a single slot would silently drop a logged QSO. This test
    // fails loudly if anyone adds callsign-level dedupe to the add arm.
    #[test]
    fn a_second_check_in_of_one_callsign_is_an_independent_roster_slot() {
        let s = replay(
            &[
                started(1, 1_000),
                checkin(2, 2_500, 42, "W1AW"),
                checkin(3, 3_000, 43, "W1AW"),
                updated(
                    4,
                    3_500,
                    43,
                    "W1AW",
                    Some("Hiram"),
                    None,
                    Some("59"),
                    StayingStatus::StayingForComments,
                ),
            ],
            0,
        );

        assert_eq!(s.roster.len(), 2, "the same callsign holds two slots");
        assert_eq!(s.roster[0].check_in_id, Uuid::from_u128(42));
        assert_eq!(s.roster[1].check_in_id, Uuid::from_u128(43));

        // Editing the second slot leaves the first contact wholly untouched.
        let first = &s.roster[0];
        assert_eq!(first.name, None);
        assert_eq!(first.signal_report, None);
        assert_eq!(first.staying, StayingStatus::InAndOut);
        assert_eq!(
            first.version, 1,
            "an edit to a sibling slot bumps no version"
        );
        assert!(first.corrections.is_empty());

        let second = &s.roster[1];
        assert_eq!(second.version, 2);
        assert_eq!(second.staying, StayingStatus::StayingForComments);
    }

    #[test]
    fn session_closed_sets_closed_lifecycle_and_instant() {
        let s = replay(&[started(1, 1_000), closed(2, 5_000)], 0);
        assert_eq!(s.lifecycle, SessionLifecycle::Closed);
        assert_eq!(s.closed_at, Some(5_000));
        assert_eq!(s.last_seq, 2);
    }

    #[test]
    fn non_advancing_seq_is_a_no_op_for_every_kind() {
        let base = fold(SessionState::default(), &started(5, 1_000));
        // seq == last_seq and seq < last_seq are both inert, for every one of
        // the four SessionEventBody kinds — not just FrequencyChanged/CheckinAdded.
        let same_freq = fold(base.clone(), &freq(5, 9_999, 1, 7_200_000));
        let lower_checkin = fold(base.clone(), &checkin(2, 9_999, 1, "W1AW"));
        let same_started = fold(base.clone(), &started(5, 9_999));
        let lower_closed = fold(base.clone(), &closed(3, 9_999));
        assert_eq!(same_freq, base);
        assert_eq!(lower_checkin, base);
        assert_eq!(same_started, base);
        assert_eq!(lower_closed, base);
    }

    #[test]
    fn seq_zero_collides_with_the_no_events_applied_sentinel_and_is_dropped() {
        // Pins the documented seq==0/last_seq==0 overlap (fold.rs module docs):
        // a hypothetical FIRST event carrying seq == 0 is indistinguishable
        // from "nothing applied yet" and is silently absorbed as a no-op. This
        // is a GIVEN contract violation by the caller (seq is 1-indexed,
        // SessionEvent::seq doc), not something fold detects or rejects.
        let s = fold(SessionState::default(), &started(0, 1_000));
        assert_eq!(s, SessionState::default());
        assert_eq!(s.last_seq, 0);
        assert_eq!(s.lifecycle, SessionLifecycle::Scheduled);
    }

    #[test]
    fn fold_is_total_on_illegal_orderings_no_panic() {
        // close-before-start, then a start at a later seq: fold projects, never rejects.
        let s = replay(&[closed(1, 500), started(2, 1_000)], 0);
        assert_eq!(s.closed_at, Some(500));
        assert_eq!(s.lifecycle, SessionLifecycle::Live);
        assert_eq!(s.last_seq, 2);
        // a second start is folded (last-write-wins), not rejected.
        let s2 = replay(&[started(1, 1_000), started(2, 2_000)], 0);
        assert_eq!(s2.started_at, Some(2_000));
    }

    // --- Worked-station cursor + notes -----------------------------

    #[test]
    fn checkin_added_projects_source_onto_the_roster_defaulting_to_staff() {
        // `source` folds onto the roster entry. A staff-sourced add
        // (the historical/default shape) folds to Staff; a self-sourced add folds
        // to SelfService. This is the ONLY producer of a self entry.
        let staff = replay(&[started(1, 1_000), checkin(2, 2_500, 42, "W1AW")], 0);
        assert_eq!(staff.roster[0].source, CheckInSource::Staff);

        let self_added = replay(&[started(1, 1_000), checkin_self(2, 2_500, 43, "N1CCK")], 0);
        assert_eq!(self_added.roster[0].source, CheckInSource::SelfService);
    }

    #[test]
    fn checkin_added_defaults_worked_false_and_notes_none() {
        let s = replay(&[started(1, 1_000), checkin(2, 2_500, 42, "W1AW")], 0);
        assert!(!s.roster[0].worked);
        assert_eq!(s.roster[0].notes, None);
        // No station is being worked before any cursor move.
        assert_eq!(s.working_check_in_id, None);
        assert_eq!(s.net_note, None);
    }

    #[test]
    fn station_worked_set_some_moves_the_cursor_to_an_on_roster_entry() {
        let s = replay(
            &[
                started(1, 1_000),
                checkin(2, 2_100, 42, "W1AW"),
                checkin(3, 2_200, 43, "N1CCK"),
                worked_set(4, 3_000, Some(42)),
            ],
            0,
        );
        assert_eq!(s.working_check_in_id, Some(Uuid::from_u128(42)));
        // The currently-working entry is NOT itself marked worked, because the
        // cursor render takes precedence; nobody has been left yet.
        assert!(!s.roster.iter().any(|e| e.worked));
    }

    #[test]
    fn moving_the_cursor_marks_the_prior_working_entry_worked() {
        // Work A, then move to B: A is now completed (worked=true), B holds the
        // cursor and is not itself worked.
        let s = replay(
            &[
                started(1, 1_000),
                checkin(2, 2_100, 42, "W1AW"),
                checkin(3, 2_200, 43, "N1CCK"),
                worked_set(4, 3_000, Some(42)),
                worked_set(5, 3_100, Some(43)),
            ],
            0,
        );
        assert_eq!(s.working_check_in_id, Some(Uuid::from_u128(43)));
        let a = s
            .roster
            .iter()
            .find(|e| e.check_in_id == Uuid::from_u128(42))
            .unwrap();
        let b = s
            .roster
            .iter()
            .find(|e| e.check_in_id == Uuid::from_u128(43))
            .unwrap();
        assert!(a.worked, "the left-behind entry is marked worked");
        assert!(
            !b.worked,
            "the currently-working entry is not marked worked"
        );
    }

    #[test]
    fn station_worked_set_none_clears_the_cursor_and_marks_prior_worked() {
        let s = replay(
            &[
                started(1, 1_000),
                checkin(2, 2_100, 42, "W1AW"),
                worked_set(3, 3_000, Some(42)),
                worked_set(4, 3_100, None),
            ],
            0,
        );
        assert_eq!(s.working_check_in_id, None);
        assert!(s.roster[0].worked, "completing a station marks it worked");
    }

    #[test]
    fn session_closed_clears_a_lingering_working_cursor() {
        // Review finding: a closed session accepts no further
        // `station.worked-set` events, so a station left "currently working"
        // at the moment of close would otherwise linger as the cursor
        // forever — the same phantom-target concern `CheckinRemoved` already
        // guards against, now also guarded on `session.closed`.
        let s = replay(
            &[
                started(1, 1_000),
                checkin(2, 2_100, 42, "W1AW"),
                worked_set(3, 3_000, Some(42)),
                closed(4, 4_000),
            ],
            0,
        );
        assert_eq!(s.working_check_in_id, None);
        assert_eq!(s.lifecycle, SessionLifecycle::Closed);
    }

    #[test]
    fn station_worked_set_to_an_off_roster_id_is_a_total_no_op_for_the_cursor() {
        let s = replay(
            &[
                started(1, 1_000),
                checkin(2, 2_100, 42, "W1AW"),
                worked_set(3, 3_000, Some(42)),
                // 999 is not on the roster: the cursor is unchanged, no entry
                // is newly marked worked.
                worked_set(4, 3_100, Some(999)),
            ],
            0,
        );
        assert_eq!(s.working_check_in_id, Some(Uuid::from_u128(42)));
        assert!(
            !s.roster[0].worked,
            "the still-working entry stays un-worked"
        );
        // The event still advanced the cursor (seq), it was just inert on state.
        assert_eq!(s.last_seq, 4);
    }

    #[test]
    fn the_worked_set_fold_arm_leaves_a_sunk_entry_where_it_was() {
        // The pinned test
        // below works B then A on a TWO-station roster — where "sink the entry
        // the cursor left" happens to be a no-op, because the entry it leaves is
        // already last. Measured: mutating the arm to sink left that test green.
        // Three stations worked front-to-back is the smallest fixture that can
        // tell "the arm moved nothing" apart from "the arm sank something".
        let s = replay(
            &[
                started(1, 1_000),
                checkin(2, 2_100, 42, "W1AW"),
                checkin(3, 2_200, 43, "N1CCK"),
                checkin(4, 2_300, 44, "K1XYZ"),
                worked_set(5, 3_000, Some(42)),
                worked_set(6, 3_100, Some(43)),
                worked_set(7, 3_200, Some(44)),
            ],
            0,
        );
        let ids: Vec<_> = s.roster.iter().map(|e| e.check_in_id).collect();
        assert_eq!(
            ids,
            vec![
                Uuid::from_u128(42),
                Uuid::from_u128(43),
                Uuid::from_u128(44)
            ],
            "the fold arm moves a pointer; any sinking is the command boundary's"
        );
        assert!(s.roster[0].worked && s.roster[1].worked);
        assert!(
            !s.roster[2].worked,
            "in the FIRST round the working entry has not yet been left behind, \
             so it is not yet worked — NOT a general invariant: `worked` is \
             monotonic, so a re-worked station holds the cursor while \
             `worked` is true, and `partition_worked_last` exempts it there"
        );
    }

    #[test]
    fn station_worked_set_does_not_reorder_bump_version_or_derive_corrections() {
        let s = replay(
            &[
                started(1, 1_000),
                checkin(2, 2_100, 42, "W1AW"),
                checkin(3, 2_200, 43, "N1CCK"),
                worked_set(4, 3_000, Some(43)),
                worked_set(5, 3_100, Some(42)),
            ],
            0,
        );
        // Roster ORDER is unchanged BY THE FOLD — a cursor move is a POINTER
        // move, not a permutation: this log carries no `roster.reordered`, and
        // the fold derives none, so the order stands FOR THIS LOG. That is not
        // a general claim that the system leaves the order alone on a
        // worked-set: under the worked-sink ordering mode the command boundary
        // appends a permutation next to the worked-set, and that permutation
        // would appear in this log if it did.
        let ids: Vec<_> = s.roster.iter().map(|e| e.check_in_id).collect();
        assert_eq!(ids, vec![Uuid::from_u128(42), Uuid::from_u128(43)]);
        for entry in &s.roster {
            assert_eq!(entry.version, 1, "no version bump on a cursor move");
            assert!(
                entry.corrections.is_empty(),
                "no corrections on a cursor move"
            );
        }
    }

    #[test]
    fn non_advancing_station_worked_set_is_inert() {
        let base = replay(
            &[
                started(1, 1_000),
                checkin(2, 2_100, 42, "W1AW"),
                worked_set(3, 3_000, Some(42)),
            ],
            0,
        );
        // seq <= last_seq: a stale cursor move is absorbed as a no-op.
        let stale = fold(base.clone(), &worked_set(3, 9_999, None));
        assert_eq!(stale, base);
    }

    #[test]
    fn removing_the_working_entry_clears_the_cursor() {
        let s = replay(
            &[
                started(1, 1_000),
                checkin(2, 2_100, 42, "W1AW"),
                checkin(3, 2_200, 43, "N1CCK"),
                worked_set(4, 3_000, Some(42)),
                removed(5, 4_000, 42),
            ],
            0,
        );
        assert_eq!(
            s.working_check_in_id, None,
            "a removed working entry clears the cursor"
        );
        assert_eq!(s.roster.len(), 1);
    }

    #[test]
    fn removing_a_non_working_entry_leaves_the_cursor_intact() {
        let s = replay(
            &[
                started(1, 1_000),
                checkin(2, 2_100, 42, "W1AW"),
                checkin(3, 2_200, 43, "N1CCK"),
                worked_set(4, 3_000, Some(42)),
                removed(5, 4_000, 43),
            ],
            0,
        );
        assert_eq!(s.working_check_in_id, Some(Uuid::from_u128(42)));
    }

    #[test]
    fn checkin_updated_replaces_notes_and_derives_no_correction_for_it() {
        let s = replay(
            &[
                started(1, 1_000),
                checkin(2, 2_500, 42, "W1AW"),
                updated_notes(3, 3_000, 42, "W1AW", Some("handling one piece of traffic")),
            ],
            0,
        );
        let entry = &s.roster[0];
        assert_eq!(
            entry.notes.as_ref().map(|n| n.as_str()),
            Some("handling one piece of traffic")
        );
        // Notes are running commentary, NOT a corrected mis-entry — no Correction
        // is ever derived for the notes field.
        assert!(
            !entry
                .corrections
                .iter()
                .any(|c| c.field.as_str() == "notes")
        );
        // The edit still bumps version like any applied checkin.updated.
        assert_eq!(entry.version, 2);
    }

    #[test]
    fn checkin_updated_folds_the_public_note_beside_the_staff_note_independently() {
        // The two notes are separate fields with separate
        // audiences: setting one must never move the other. `notes` keeps meaning
        // the STAFF note — the persisted key was deliberately NOT renamed, so
        // every historical event still decodes its note into the private field.
        let s = replay(
            &[
                started(1, 1_000),
                checkin(2, 2_500, 42, "W1AW"),
                updated_both_notes(
                    3,
                    3_000,
                    42,
                    "W1AW",
                    Some("STAFF: sounded rough"),
                    Some("PUBLIC: relaying for W1BBB"),
                ),
            ],
            0,
        );
        let entry = &s.roster[0];
        assert_eq!(
            entry.notes.as_ref().map(|n| n.as_str()),
            Some("STAFF: sounded rough")
        );
        assert_eq!(
            entry.public_note.as_ref().map(|n| n.as_str()),
            Some("PUBLIC: relaying for W1BBB")
        );
    }

    #[test]
    fn a_historical_update_with_no_public_note_folds_it_to_none_and_keeps_the_staff_note() {
        // The migration outcome, by CONSTRUCTION. A log
        // written before the split carries no public-note key at all, so on the
        // first replay afterwards every existing note is in the STAFF field,
        // character for character, and the public note is empty. No migration
        // file, no backfill, no UPDATE against the append-only log.
        let s = replay(
            &[
                started(1, 1_000),
                checkin(2, 2_500, 42, "W1AW"),
                updated_notes(3, 3_000, 42, "W1AW", Some("written months ago")),
            ],
            0,
        );
        let entry = &s.roster[0];
        assert_eq!(
            entry.notes.as_ref().map(|n| n.as_str()),
            Some("written months ago"),
            "the historical note is the STAFF note, unchanged"
        );
        assert!(
            entry.public_note.is_none(),
            "nothing already written was retroactively published"
        );
    }

    #[test]
    fn checkin_added_defaults_the_public_note_to_none() {
        // The public note is edit-only, exactly as the staff note is.
        let s = replay(&[started(1, 1_000), checkin(2, 2_500, 42, "W1AW")], 0);
        assert!(s.roster[0].public_note.is_none());
    }

    #[test]
    fn checkin_updated_derives_no_correction_for_the_public_note() {
        // The public note inherits the staff note's rule
        // exactly — running commentary, not a corrected mis-entry — so the
        // `CorrectionField` vocabulary gains no member.
        let s = replay(
            &[
                started(1, 1_000),
                checkin(2, 2_500, 42, "W1AW"),
                updated_both_notes(3, 3_000, 42, "W1AW", None, Some("first")),
                updated_both_notes(4, 4_000, 42, "W1AW", None, Some("second")),
            ],
            0,
        );
        let entry = &s.roster[0];
        assert_eq!(
            entry.public_note.as_ref().map(|n| n.as_str()),
            Some("second"),
            "REPLACE, last-write-wins"
        );
        assert!(
            entry.corrections.is_empty(),
            "a public-note edit derives no correction at all: {:?}",
            entry.corrections
        );
        assert_eq!(entry.version, 3, "it still bumps the CAS version");
    }

    #[test]
    fn checkin_updated_still_derives_the_other_corrections_alongside_a_note() {
        // A single edit that changes callsign AND sets a note derives exactly the
        // callsign correction — never a notes one.
        let s = replay(
            &[
                started(1, 1_000),
                checkin(2, 2_500, 42, "W1AW"),
                SessionEvent {
                    seq: 3,
                    actor_id: Some(Uuid::from_u128(300)),
                    at: 3_000,
                    body: SessionEventBody::CheckinUpdated {
                        check_in_id: Uuid::from_u128(42),
                        callsign: callsign("W1AX"),
                        name: None,
                        location: None,
                        grid: None,
                        signal_report: None,
                        staying: StayingStatus::InAndOut,
                        precedence: Precedence::Routine,
                        traffic: None,
                        notes: crate::check_in::parse_note("late check-in").expect("valid"),
                        public_note: None,
                        via: None,
                        relayed_by: None,
                    },
                },
            ],
            0,
        );
        let corr = &s.roster[0].corrections;
        assert_eq!(corr.len(), 1);
        assert_eq!(corr[0].field, CorrectionField::Callsign);
    }

    #[test]
    fn session_note_set_sets_and_clears_the_net_note() {
        let s = replay(
            &[
                started(1, 1_000),
                note_set(2, 2_000, Some("Weekly traffic net, all welcome")),
            ],
            0,
        );
        assert_eq!(
            s.net_note.as_ref().map(|n| n.as_str()),
            Some("Weekly traffic net, all welcome")
        );
        // Setting again REPLACES (last-write-wins); None clears.
        let s2 = fold(s.clone(), &note_set(3, 3_000, Some("Net closing in 5")));
        assert_eq!(
            s2.net_note.as_ref().map(|n| n.as_str()),
            Some("Net closing in 5")
        );
        let s3 = fold(s2, &note_set(4, 4_000, None));
        assert_eq!(s3.net_note, None);
    }

    #[test]
    fn rounds_are_repeatable_without_losing_prior_notes_or_traffic() {
        // A multi-pass round. Work A→B→C, capturing a per-station note and
        // traffic on A, then a SECOND round re-works A. A's note/traffic from
        // round 1 survive untouched, and A re-holds the cursor while STAYING
        // `worked` — the flag is monotonic and no arm ever clears it. The
        // assertion below pins that, because this comment once claimed the
        // opposite of the one forty-four lines beneath it.
        let s = replay(
            &[
                started(1, 1_000),
                checkin(2, 2_100, 42, "W1AW"),
                checkin(3, 2_200, 43, "N1CCK"),
                checkin(4, 2_300, 44, "K1XYZ"),
                // Round 1: work A, then a single edit setting A's note +
                // precedence + traffic together (the modal sends the full field
                // set at once), then move to B, then C.
                worked_set(5, 3_000, Some(42)),
                SessionEvent {
                    seq: 6,
                    actor_id: Some(Uuid::from_u128(300)),
                    at: 3_050,
                    body: SessionEventBody::CheckinUpdated {
                        check_in_id: Uuid::from_u128(42),
                        callsign: callsign("W1AW"),
                        name: None,
                        location: None,
                        grid: None,
                        signal_report: None,
                        staying: StayingStatus::InAndOut,
                        precedence: Precedence::Priority,
                        traffic: crate::check_in::parse_traffic_count(Some(2)).expect("valid"),
                        notes: crate::check_in::parse_note("passing NTS traffic").expect("valid"),
                        public_note: None,
                        via: None,
                        relayed_by: None,
                    },
                },
                worked_set(7, 3_100, Some(43)),
                worked_set(8, 3_200, Some(44)),
                // Round 2: revisit A.
                worked_set(9, 4_000, Some(42)),
            ],
            0,
        );
        let a = s
            .roster
            .iter()
            .find(|e| e.check_in_id == Uuid::from_u128(42))
            .unwrap();
        // A holds the cursor again in round 2, so it RENDERS as the working
        // station (working takes visual precedence over the worked-dim).
        // `worked` is monotonic: A was left behind in round 1, so its flag
        // stays true; the render precedence — not a fold-side reset — is what
        // shows the cursor rather than the dim.
        assert_eq!(s.working_check_in_id, Some(Uuid::from_u128(42)));
        assert!(
            a.worked,
            "`worked` is monotonic — re-working A must not clear the flag \
             round 1 set. `partition_worked_last` exempts the cursor from the \
             sink instead; a fold-side reset would change what replay MEANS."
        );
        // Round-1 note + traffic are untouched by any later cursor move.
        assert_eq!(
            a.notes.as_ref().map(|n| n.as_str()),
            Some("passing NTS traffic")
        );
        assert_eq!(a.precedence, Precedence::Priority);
        assert_eq!(a.traffic.map(|t| t.get()), Some(2));
        // B and C were left behind, so both are worked.
        let b = s
            .roster
            .iter()
            .find(|e| e.check_in_id == Uuid::from_u128(43))
            .unwrap();
        let c = s
            .roster
            .iter()
            .find(|e| e.check_in_id == Uuid::from_u128(44))
            .unwrap();
        assert!(b.worked && c.worked);
    }

    // --- Control-status axis (stall / resume / handoff) -----------

    fn stalled(seq: u64, at: u64) -> SessionEvent {
        SessionEvent {
            seq,
            actor_id: None,
            at,
            body: SessionEventBody::NcsStalled,
        }
    }

    fn resumed(seq: u64, at: u64) -> SessionEvent {
        SessionEvent {
            seq,
            actor_id: None,
            at,
            body: SessionEventBody::NcsResumed,
        }
    }

    fn handed_off(seq: u64, at: u64, new_ncs: u128) -> SessionEvent {
        SessionEvent {
            seq,
            actor_id: Some(Uuid::from_u128(300)),
            at,
            body: SessionEventBody::ControlHandedOff {
                new_ncs_account_id: Uuid::from_u128(new_ncs),
            },
        }
    }

    #[test]
    fn a_fresh_session_defaults_to_active_with_no_active_ncs() {
        // The two-axis default: an unstarted session is Scheduled AND Active with
        // no active NCS and no stall instant.
        let s = SessionState::default();
        assert_eq!(s.control_state, ControlState::Active);
        assert_eq!(s.active_ncs_account_id, None);
        assert_eq!(s.stalled_at, None);
    }

    #[test]
    fn session_started_sets_the_starter_as_the_initial_active_ncs() {
        // Session.started sets active_ncs = the starter (the
        // envelope's actor_id), control_state Active.
        let s = fold(SessionState::default(), &started(1, 1_000));
        assert_eq!(s.active_ncs_account_id, Some(Uuid::from_u128(100)));
        assert_eq!(s.control_state, ControlState::Active);
        assert_eq!(s.stalled_at, None);
        // The lifecycle axis is unaffected by the new control fields.
        assert_eq!(s.lifecycle, SessionLifecycle::Live);
    }

    #[test]
    fn ncs_stalled_moves_control_to_stalled_and_stamps_the_stall_instant() {
        // The lifecycle stays Live — the two axes are independent.
        let s = replay(&[started(1, 1_000), stalled(2, 90_000)], 0);
        assert_eq!(s.control_state, ControlState::Stalled);
        assert_eq!(s.stalled_at, Some(90_000));
        assert_eq!(s.lifecycle, SessionLifecycle::Live);
        // The active NCS is unchanged by a stall — nobody took control.
        assert_eq!(s.active_ncs_account_id, Some(Uuid::from_u128(100)));
    }

    #[test]
    fn ncs_resumed_returns_to_active_under_the_same_ncs_and_clears_the_stall_instant() {
        let s = replay(
            &[started(1, 1_000), stalled(2, 90_000), resumed(3, 120_000)],
            0,
        );
        assert_eq!(s.control_state, ControlState::Active);
        assert_eq!(s.stalled_at, None);
        // Resume keeps the ORIGINAL active NCS.
        assert_eq!(s.active_ncs_account_id, Some(Uuid::from_u128(100)));
    }

    #[test]
    fn control_handed_off_moves_the_active_ncs_and_returns_to_active() {
        // A claim from a stalled session: the claimer becomes active NCS and the
        // net resumes under them.
        let s = replay(
            &[
                started(1, 1_000),
                stalled(2, 90_000),
                handed_off(3, 100_000, 555),
            ],
            0,
        );
        assert_eq!(s.active_ncs_account_id, Some(Uuid::from_u128(555)));
        assert_eq!(s.control_state, ControlState::Active);
        assert_eq!(s.stalled_at, None);
    }

    #[test]
    fn voluntary_handoff_on_a_healthy_session_stays_active_and_moves_control() {
        // A handoff with no intervening stall (voluntary path): control_state
        // is Active before and after; only the active NCS moves.
        let s = replay(&[started(1, 1_000), handed_off(2, 2_000, 555)], 0);
        assert_eq!(s.control_state, ControlState::Active);
        assert_eq!(s.active_ncs_account_id, Some(Uuid::from_u128(555)));
        assert_eq!(s.stalled_at, None);
    }

    #[test]
    fn closing_a_stalled_session_resets_the_control_axis_to_inert_defaults() {
        // Auto-close from a stall: SessionClosed resets control_state/stalled_at
        // so a closed net never renders net-paused.
        let s = replay(
            &[started(1, 1_000), stalled(2, 90_000), closed(3, 900_000)],
            0,
        );
        assert_eq!(s.lifecycle, SessionLifecycle::Closed);
        assert_eq!(s.control_state, ControlState::Active);
        assert_eq!(s.stalled_at, None);
    }

    #[test]
    fn non_advancing_seq_is_inert_for_the_control_kinds() {
        let base = replay(&[started(1, 1_000), stalled(2, 90_000)], 0);
        // seq <= last_seq: each control kind is absorbed as a no-op.
        let stale_resume = fold(base.clone(), &resumed(2, 9_999));
        let stale_handoff = fold(base.clone(), &handed_off(1, 9_999, 555));
        assert_eq!(stale_resume, base);
        assert_eq!(stale_handoff, base);
    }

    #[test]
    fn control_state_wire_tokens_are_lowercase_kebab() {
        assert_eq!(ControlState::Active.as_str(), "active");
        assert_eq!(ControlState::Stalled.as_str(), "stalled");
    }

    #[test]
    fn replay_since_folds_only_the_tail() {
        let log = [
            started(1, 1_000),
            checkin(2, 2_500, 42, "W1AW"),
            checkin(3, 3_000, 43, "N1CCK"),
        ];
        let tail = replay(&log, 1);
        // Only seq > 1 applied: two check-ins, but no start ⇒ still Scheduled.
        assert_eq!(tail.lifecycle, SessionLifecycle::Scheduled);
        assert_eq!(tail.roster.len(), 2);
        assert_eq!(tail.last_seq, 3);
    }

    // --- `via` through the fold ---------------------------------

    fn via_connection(n: u128) -> Via {
        Via::Connection(Uuid::from_u128(n))
    }

    fn edited_via(seq: u64, at: u64, id: u128, call: &str, via: Option<Via>) -> SessionEvent {
        SessionEvent {
            seq,
            actor_id: Some(Uuid::from_u128(300)),
            at,
            body: SessionEventBody::CheckinUpdated {
                check_in_id: Uuid::from_u128(id),
                callsign: callsign(call),
                name: None,
                location: None,
                grid: None,
                signal_report: None,
                staying: StayingStatus::InAndOut,
                precedence: Precedence::Routine,
                traffic: None,
                notes: None,
                public_note: None,
                via,
                relayed_by: None,
            },
        }
    }

    fn checkin_via(seq: u64, at: u64, id: u128, call: &str, via: Option<Via>) -> SessionEvent {
        let mut event = checkin(seq, at, id, call);
        if let SessionEventBody::CheckinAdded {
            check_in_id,
            callsign,
            signal_report,
            staying,
            name,
            location,
            grid,
            source,
            client_event_id,
            ..
        } = event.body
        {
            event.body = SessionEventBody::CheckinAdded {
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
                relayed_by: None,
            };
        }
        event
    }

    #[test]
    fn a_checkin_added_carrying_a_via_projects_it_onto_the_roster_entry() {
        let s = replay(
            &[
                started(1, 1_000),
                checkin_via(2, 2_500, 42, "W1AW", Some(via_connection(7))),
            ],
            0,
        );
        assert_eq!(s.roster[0].via, Some(via_connection(7)));
    }

    #[test]
    fn a_checkin_added_without_a_via_key_folds_to_not_recorded() {
        let s = replay(&[started(1, 1_000), checkin(2, 2_500, 42, "W1AW")], 0);
        assert_eq!(
            s.roster[0].via, None,
            "`None` means nobody recorded it — never `arrived on the export connection`"
        );
    }

    // --- The connection set AS AT a check-in's seq ---------------

    /// An HF snapshot connection with the id the `freq` fixture moves.
    fn wire(n: u128, planned_hz: i64) -> NetConnectionWire {
        NetConnectionWire {
            id: connection(n),
            position: 0,
            kind: "hf".to_owned(),
            planned_frequency_hz: Some(planned_hz),
            band: Some("20m".to_owned()),
            mode: Some("ssb".to_owned()),
            repeater_offset_hz: None,
            tone_mode: None,
            tone_value: None,
            node: None,
            reflector: None,
            network: None,
            talkgroup: None,
            label: None,
            detail: None,
        }
    }

    fn planned_hz(connections: &[NetConnectionWire], n: u128) -> Option<i64> {
        connections
            .iter()
            .find(|c| c.id == connection(n))
            .and_then(|c| c.planned_frequency_hz)
    }

    #[test]
    fn checkin_added_records_the_seq_it_arrived_at_on_the_roster_entry() {
        let s = replay(&[started(1, 1_000), checkin(7, 2_500, 42, "W1AW")], 0);
        assert_eq!(s.roster[0].added_seq, 7);
    }

    #[test]
    fn frequency_moves_are_kept_in_seq_order_beside_the_last_write_wins_map() {
        let s = replay(
            &[
                started(1, 1_000),
                freq(2, 2_000, 1, 7_200_000),
                freq(3, 3_000, 2, 14_300_000),
                freq(4, 4_000, 1, 7_250_000),
            ],
            0,
        );
        assert_eq!(
            s.frequency_moves,
            vec![
                FrequencyMove {
                    seq: 2,
                    connection_id: connection(1),
                    operating_frequency_hz: 7_200_000
                },
                FrequencyMove {
                    seq: 3,
                    connection_id: connection(2),
                    operating_frequency_hz: 14_300_000
                },
                FrequencyMove {
                    seq: 4,
                    connection_id: connection(1),
                    operating_frequency_hz: 7_250_000
                },
            ]
        );
        // The map still answers "now"; the run is not a replacement for it.
        assert_eq!(
            s.connection_frequencies.get(&connection(1)),
            Some(&7_250_000)
        );
    }

    #[test]
    fn a_non_advancing_frequency_changed_does_not_join_the_run() {
        let s = replay(
            &[
                started(1, 1_000),
                freq(2, 2_000, 1, 7_200_000),
                freq(2, 2_000, 1, 7_999_999),
            ],
            0,
        );
        assert_eq!(s.frequency_moves.len(), 1);
    }

    #[test]
    fn connections_at_reports_the_planned_frequency_before_a_move_and_the_moved_to_one_after() {
        // The shape: a station checks in at seq 2, the net QSYs at seq
        // 3, a second station checks in at seq 4.
        let s = replay(
            &[
                started(1, 1_000),
                checkin(2, 2_000, 1, "W1AW"),
                freq(3, 3_000, 1, 14_250_000),
                checkin(4, 4_000, 2, "W1ABC"),
            ],
            0,
        );
        let snapshot = [wire(1, 14_230_000)];
        let before = s.connections_at(s.roster[0].added_seq, &snapshot);
        let after = s.connections_at(s.roster[1].added_seq, &snapshot);
        assert_eq!(planned_hz(&before, 1), Some(14_230_000));
        assert_eq!(planned_hz(&after, 1), Some(14_250_000));
    }

    #[test]
    fn connections_at_applies_last_write_wins_up_to_and_including_the_seq() {
        let s = replay(
            &[
                started(1, 1_000),
                freq(2, 2_000, 1, 14_240_000),
                freq(3, 3_000, 1, 14_250_000),
                freq(5, 5_000, 1, 14_260_000),
            ],
            0,
        );
        let snapshot = [wire(1, 14_230_000)];
        assert_eq!(
            planned_hz(&s.connections_at(1, &snapshot), 1),
            Some(14_230_000)
        );
        // Inclusive: a move AT the seq asked for has already happened.
        assert_eq!(
            planned_hz(&s.connections_at(2, &snapshot), 1),
            Some(14_240_000)
        );
        assert_eq!(
            planned_hz(&s.connections_at(3, &snapshot), 1),
            Some(14_250_000)
        );
        // Between moves: the latest one at or before the seq wins.
        assert_eq!(
            planned_hz(&s.connections_at(4, &snapshot), 1),
            Some(14_250_000)
        );
        assert_eq!(
            planned_hz(&s.connections_at(5, &snapshot), 1),
            Some(14_260_000)
        );
        assert_eq!(
            planned_hz(&s.connections_at(99, &snapshot), 1),
            Some(14_260_000)
        );
    }

    #[test]
    fn connections_at_leaves_a_connection_nobody_moved_on_its_planned_frequency() {
        let s = replay(&[started(1, 1_000), freq(2, 2_000, 1, 14_250_000)], 0);
        let snapshot = [wire(1, 14_230_000), wire(2, 7_200_000)];
        let at = s.connections_at(2, &snapshot);
        assert_eq!(planned_hz(&at, 1), Some(14_250_000));
        assert_eq!(planned_hz(&at, 2), Some(7_200_000));
        // With no moves at all the answer IS the snapshot — which is why every
        // never-moving fixture is green under the wrong implementations too.
        let unmoved = replay(&[started(1, 1_000)], 0);
        assert_eq!(unmoved.connections_at(1, &snapshot), snapshot.to_vec());
    }

    #[test]
    fn connections_at_the_last_seq_is_live_connections() {
        // The invariant that stops the two projections drifting: "as at the
        // latest seq" and "now" are the same question.
        let s = replay(
            &[
                started(1, 1_000),
                freq(2, 2_000, 1, 7_200_000),
                checkin(3, 3_000, 1, "W1AW"),
                freq(4, 4_000, 2, 14_300_000),
                freq(5, 5_000, 1, 7_250_000),
                checkin(6, 6_000, 2, "W1ABC"),
            ],
            0,
        );
        let snapshot = [wire(1, 7_100_000), wire(2, 14_230_000), wire(3, 3_900_000)];
        assert_eq!(
            s.connections_at(s.last_seq, &snapshot),
            s.live_connections(&snapshot)
        );
        // And strictly BEFORE the last move the two legitimately differ — the
        // whole reason `connections_at` exists.
        assert_ne!(
            s.connections_at(s.roster[0].added_seq, &snapshot),
            s.live_connections(&snapshot)
        );
    }

    #[test]
    fn a_via_corrected_after_a_move_reads_the_new_connection_as_at_the_add_seq() {
        // The `checkin.updated` edge: added at seq 2 on A,
        // B moved at seq 3, via corrected to B at seq 4. The correction fixes
        // WHICH way in; the check-in's own moment fixes WHEN — so B's PLANNED
        // frequency, the one B was on when the station actually arrived.
        let s = replay(
            &[
                started(1, 1_000),
                checkin_via(2, 2_000, 42, "W1AW", Some(via_connection(1))),
                freq(3, 3_000, 2, 14_250_000),
                edited_via(4, 4_000, 42, "W1AW", Some(via_connection(2))),
            ],
            0,
        );
        let entry = &s.roster[0];
        assert_eq!(entry.via, Some(via_connection(2)));
        assert_eq!(entry.added_seq, 2, "a correction does not move the add seq");
        let snapshot = [wire(1, 7_200_000), wire(2, 14_230_000)];
        let as_at = s.connections_at(entry.added_seq, &snapshot);
        assert_eq!(planned_hz(&as_at, 2), Some(14_230_000));
        assert_eq!(
            planned_hz(&s.live_connections(&snapshot), 2),
            Some(14_250_000)
        );
    }

    #[test]
    fn frequencies_as_at_cuts_the_run_off_inclusively_and_does_not_sort() {
        let run = [
            FrequencyMove {
                seq: 2,
                connection_id: connection(1),
                operating_frequency_hz: 1,
            },
            FrequencyMove {
                seq: 4,
                connection_id: connection(1),
                operating_frequency_hz: 2,
            },
            FrequencyMove {
                seq: 6,
                connection_id: connection(2),
                operating_frequency_hz: 3,
            },
        ];
        assert!(frequencies_as_at(run, 1).is_empty());
        assert_eq!(frequencies_as_at(run, 4).get(&connection(1)), Some(&2));
        assert_eq!(frequencies_as_at(run, 4).get(&connection(2)), None);
        assert_eq!(frequencies_as_at(run, 6).len(), 2);
    }

    #[test]
    #[should_panic(expected = "non-decreasing seq")]
    fn frequencies_as_at_refuses_a_run_that_breaks_the_ordering_precondition() {
        // The precondition is the fold's and the lateral's
        // to keep, and `take_while` on a broken one returns a plausible WRONG
        // map with no signal. The function still does not sort (that would hide
        // the broken caller); it names the caller instead, in debug builds.
        let out_of_order = [
            FrequencyMove {
                seq: 4,
                connection_id: connection(1),
                operating_frequency_hz: 2,
            },
            FrequencyMove {
                seq: 2,
                connection_id: connection(1),
                operating_frequency_hz: 1,
            },
        ];
        let _ = frequencies_as_at(out_of_order, 4);
    }

    #[test]
    fn an_edit_that_changes_via_derives_one_correction_carrying_both_structured_sides() {
        let s = replay(
            &[
                started(1, 1_000),
                checkin_via(2, 2_500, 42, "W1AW", Some(via_connection(7))),
                edited_via(3, 3_000, 42, "W1AW", Some(via_connection(9))),
            ],
            0,
        );
        let via_corrections: Vec<&Correction> = s.roster[0]
            .corrections
            .iter()
            .filter(|c| c.field == CorrectionField::Via)
            .collect();
        assert_eq!(
            via_corrections.len(),
            1,
            "exactly one, like every other field"
        );
        assert_eq!(
            via_corrections[0].from,
            Some(CorrectionValue::Via(via_connection(7)))
        );
        assert_eq!(
            via_corrections[0].to,
            Some(CorrectionValue::Via(via_connection(9)))
        );
        assert_eq!(s.roster[0].via, Some(via_connection(9)));
    }

    #[test]
    fn a_free_text_via_round_trips_through_a_correction_byte_for_byte() {
        let typed = Via::Unlisted("Bill's phone patch — 2m simplex".to_owned());
        let s = replay(
            &[
                started(1, 1_000),
                checkin_via(2, 2_500, 42, "W1AW", Some(typed.clone())),
                edited_via(3, 3_000, 42, "W1AW", None),
            ],
            0,
        );
        let correction = s.roster[0]
            .corrections
            .iter()
            .find(|c| c.field == CorrectionField::Via)
            .expect("a via correction");
        assert_eq!(correction.from, Some(CorrectionValue::Via(typed)));
        assert_eq!(
            correction.to, None,
            "clearing derives `None` on the absent side"
        );
    }

    #[test]
    fn an_edit_that_leaves_via_alone_derives_no_via_correction() {
        let s = replay(
            &[
                started(1, 1_000),
                checkin_via(2, 2_500, 42, "W1AW", Some(via_connection(7))),
                edited_via(3, 3_000, 42, "W1AW", Some(via_connection(7))),
            ],
            0,
        );
        assert!(
            !s.roster[0]
                .corrections
                .iter()
                .any(|c| c.field == CorrectionField::Via),
            "an unchanged field must not fabricate a correction"
        );
    }

    #[test]
    fn a_participants_edit_that_wipes_via_is_visible_as_a_correction_not_silent() {
        // `checkin.updated` carries the FULL post-edit set, so a writer that
        // fails to carry `via` forward destroys it. The fold's job is to make
        // that visible rather than to prevent it — the guard is the write path's.
        let s = replay(
            &[
                started(1, 1_000),
                checkin_via(2, 2_500, 42, "W1AW", Some(via_connection(7))),
                edited_via(3, 3_000, 42, "W1AW", None),
            ],
            0,
        );
        assert_eq!(s.roster[0].via, None);
        assert!(
            s.roster[0]
                .corrections
                .iter()
                .any(|c| c.field == CorrectionField::Via && c.to.is_none())
        );
    }

    // --- `relayed_by` through the fold --------------------------

    /// A relaying station, as the ordinary on-air case: a callsign with no
    /// NetRoll account behind it.
    fn relay_station(call: &str) -> Callsign {
        callsign(call)
    }

    /// A `checkin.added` carrying BOTH per-check-in facts, either, or neither —
    /// the shape the independence assertion needs.
    fn checkin_relayed(
        seq: u64,
        at: u64,
        id: u128,
        call: &str,
        via: Option<Via>,
        relayed_by: Option<Callsign>,
    ) -> SessionEvent {
        let mut event = checkin_via(seq, at, id, call, via);
        if let SessionEventBody::CheckinAdded {
            relayed_by: slot, ..
        } = &mut event.body
        {
            *slot = relayed_by;
        }
        event
    }

    fn edited_relayed(
        seq: u64,
        at: u64,
        id: u128,
        call: &str,
        via: Option<Via>,
        relayed_by: Option<Callsign>,
    ) -> SessionEvent {
        let mut event = edited_via(seq, at, id, call, via);
        if let SessionEventBody::CheckinUpdated {
            relayed_by: slot, ..
        } = &mut event.body
        {
            *slot = relayed_by;
        }
        event
    }

    #[test]
    fn a_checkin_added_carrying_a_relaying_station_projects_it_onto_the_roster_entry() {
        let s = replay(
            &[
                started(1, 1_000),
                checkin_relayed(2, 2_500, 42, "W1AW", None, Some(relay_station("W3REL"))),
            ],
            0,
        );
        assert_eq!(s.roster[0].relayed_by, Some(relay_station("W3REL")));
    }

    #[test]
    fn a_checkin_added_without_a_relayed_by_key_folds_to_not_relayed() {
        let s = replay(&[started(1, 1_000), checkin(2, 2_500, 42, "W1AW")], 0);
        assert_eq!(
            s.roster[0].relayed_by, None,
            "`None` means NOT RELAYED, and it is what a field-less historical add folds to"
        );
    }

    #[test]
    fn a_way_in_and_a_relaying_station_are_independent_in_both_directions() {
        // An entry may carry both, one, or neither, and setting one never
        // reads or writes the other. Four entries, one per corner of the pair.
        let s = replay(
            &[
                started(1, 1_000),
                checkin_relayed(2, 2_100, 1, "W1AW", None, None),
                checkin_relayed(3, 2_200, 2, "W2BCD", Some(via_connection(7)), None),
                checkin_relayed(4, 2_300, 3, "W3CDE", None, Some(relay_station("W3REL"))),
                checkin_relayed(
                    5,
                    2_400,
                    4,
                    "W4DEF",
                    Some(via_connection(7)),
                    Some(relay_station("W3REL")),
                ),
            ],
            0,
        );
        let pairs: Vec<(bool, bool)> = s
            .roster
            .iter()
            .map(|e| (e.via.is_some(), e.relayed_by.is_some()))
            .collect();
        assert_eq!(
            pairs,
            vec![(false, false), (true, false), (false, true), (true, true)],
            "neither field may stand in for the other, in either direction"
        );
    }

    #[test]
    fn an_edit_that_changes_the_relaying_station_derives_one_text_correction() {
        // "it was W1ABC who relayed her, not W1ABD" is a corrected
        // MIS-ENTRY, the `via`/`callsign` class — so it DOES derive an
        // annotation, and it carries `Text` because a callsign is already the
        // text a reader reads. No third `CorrectionValue` variant.
        let s = replay(
            &[
                started(1, 1_000),
                checkin_relayed(2, 2_500, 42, "W1AW", None, Some(relay_station("W1ABC"))),
                edited_relayed(3, 3_000, 42, "W1AW", None, Some(relay_station("W1ABD"))),
            ],
            0,
        );
        let derived: Vec<&Correction> = s.roster[0]
            .corrections
            .iter()
            .filter(|c| c.field == CorrectionField::RelayedBy)
            .collect();
        assert_eq!(derived.len(), 1);
        assert_eq!(
            derived[0].from,
            Some(CorrectionValue::Text("W1ABC".to_owned()))
        );
        assert_eq!(
            derived[0].to,
            Some(CorrectionValue::Text("W1ABD".to_owned()))
        );
        assert_eq!(s.roster[0].relayed_by, Some(relay_station("W1ABD")));
    }

    #[test]
    fn an_edit_that_sets_or_clears_the_relaying_station_derives_a_one_sided_correction() {
        let s = replay(
            &[
                started(1, 1_000),
                checkin_relayed(2, 2_500, 42, "W1AW", None, None),
                edited_relayed(3, 3_000, 42, "W1AW", None, Some(relay_station("W3REL"))),
                edited_relayed(4, 3_500, 42, "W1AW", None, None),
            ],
            0,
        );
        let derived: Vec<&Correction> = s.roster[0]
            .corrections
            .iter()
            .filter(|c| c.field == CorrectionField::RelayedBy)
            .collect();
        assert_eq!(derived.len(), 2, "a set and a clear are both corrections");
        assert!(derived[0].from.is_none(), "nothing was there to correct");
        assert_eq!(
            derived[0].to,
            Some(CorrectionValue::Text("W3REL".to_owned()))
        );
        assert!(derived[1].to.is_none(), "the clear has no `to` side");
        assert_eq!(s.roster[0].relayed_by, None);
    }

    #[test]
    fn an_edit_that_leaves_the_relaying_station_alone_derives_no_correction() {
        let s = replay(
            &[
                started(1, 1_000),
                checkin_relayed(2, 2_500, 42, "W1AW", None, Some(relay_station("W3REL"))),
                edited_relayed(3, 3_000, 42, "W1AW", None, Some(relay_station("W3REL"))),
            ],
            0,
        );
        assert!(
            !s.roster[0]
                .corrections
                .iter()
                .any(|c| c.field == CorrectionField::RelayedBy),
            "an unchanged field must not fabricate a correction"
        );
    }

    #[test]
    fn the_note_pair_still_derives_no_correction_beside_a_relay_one() {
        // The contrast: a note is running commentary and derives
        // nothing; a relaying station is a corrected mis-entry and derives one.
        // Asserted together so a future author cannot read the relay derivation
        // as licence to give the notes one.
        let mut noted = edited_relayed(3, 3_000, 42, "W1AW", None, Some(relay_station("W3REL")));
        if let SessionEventBody::CheckinUpdated {
            notes, public_note, ..
        } = &mut noted.body
        {
            *notes = crate::check_in::parse_note("a staff note").expect("a valid note");
            *public_note = crate::check_in::parse_note("a public note").expect("a valid note");
        }
        let s = replay(
            &[
                started(1, 1_000),
                checkin_relayed(2, 2_500, 42, "W1AW", None, None),
                noted,
            ],
            0,
        );
        let fields: Vec<CorrectionField> =
            s.roster[0].corrections.iter().map(|c| c.field).collect();
        assert_eq!(
            fields,
            vec![CorrectionField::RelayedBy],
            "the relaying station annotates; neither note does"
        );
    }

    #[test]
    fn the_via_correction_annotates_after_the_eight_fields_that_shipped_before_it() {
        let s = replay(
            &[
                started(1, 1_000),
                checkin_via(2, 2_500, 42, "W1AW", Some(via_connection(7))),
                edited_via(3, 3_000, 42, "W1ABC", Some(via_connection(9))),
            ],
            0,
        );
        let fields: Vec<CorrectionField> =
            s.roster[0].corrections.iter().map(|c| c.field).collect();
        assert_eq!(
            fields,
            vec![CorrectionField::Callsign, CorrectionField::Via],
            "the derived order is deterministic and the TS reducer mirrors it"
        );
    }

    /// Every kind token `SessionEventBody::kind()` can return, read out of that
    /// function's own `match` rather than restated here.
    ///
    /// An earlier form of this test asserted `kinds.len() == 14` about a
    /// fourteen-element literal it had just written: a FIFTEENTH variant would
    /// have left it green, which is the one thing it exists to make loud.
    /// `kind()`'s `match` is exhaustive, so a
    /// new variant forces a new arm there, and reading the arms is what makes
    /// this count derived instead of asserted against itself. The same
    /// `include_str!` idiom `roster_projection_sites.rs` uses, for the same
    /// reason.
    fn kind_tokens_declared_in_event_rs() -> Vec<String> {
        const EVENT_RS: &str = include_str!("event.rs");
        let body = EVENT_RS
            .split_once("pub fn kind(&self) -> &'static str {")
            .expect("kind() is declared in event.rs")
            .1;
        let body = body.split_once("\n    }").expect("kind()'s match closes").0;
        body.lines()
            .filter_map(|line| {
                let (_, after) = line.split_once("=> \"")?;
                Some(after.split_once('"')?.0.to_owned())
            })
            .collect()
    }

    #[test]
    fn adding_via_grows_no_new_event_kind() {
        // `via` rides the ordinary add/edit events.
        // A new `SessionEventBody` variant here would mean a correction event was
        // minted after all.
        let kinds = kind_tokens_declared_in_event_rs();
        assert_eq!(
            kinds,
            vec![
                "session.started",
                "frequency.changed",
                "checkin.added",
                "checkin.updated",
                "checkin.removed",
                "roster.reordered",
                "station.worked-set",
                "session.note-set",
                "roster.order-mode-set",
                "session.closed",
                "ncs.stalled",
                "ncs.resumed",
                "control.handed-off",
                "station.blocked",
            ],
            "the session-event kind vocabulary is the same fourteen it was before `via`"
        );
        assert!(
            kinds.contains(
                &SessionEventBody::CheckinAdded {
                    check_in_id: Uuid::nil(),
                    callsign: callsign("W1AW"),
                    client_event_id: None,
                    signal_report: None,
                    staying: StayingStatus::default(),
                    name: None,
                    location: None,
                    grid: None,
                    source: CheckInSource::Staff,
                    via: Some(via_connection(1)),
                    relayed_by: None,
                }
                .kind()
                .to_owned()
            ),
            "the parse above reads the same vocabulary `kind()` actually returns"
        );
    }

    #[test]
    fn the_via_correction_token_is_stable_lowercase_kebab() {
        assert_eq!(CorrectionField::Via.as_str(), "via");
    }
}

#[cfg(test)]
mod proptest_invariants {
    use super::*;
    use proptest::prelude::*;

    fn arb_uuid() -> impl Strategy<Value = Uuid> {
        any::<u128>().prop_map(Uuid::from_u128)
    }

    // A small check_in_id pool so `checkin.added` collisions are exercised.
    fn arb_check_in_id() -> impl Strategy<Value = Uuid> {
        (0u128..4).prop_map(|i| Uuid::from_u128(i + 1))
    }

    fn arb_body() -> impl Strategy<Value = SessionEventBody> {
        let call = crate::callsign::parse_callsign("W1AW").expect("valid");
        prop_oneof![
            (arb_uuid(), any::<i32>()).prop_map(|(definition_id, definition_version)| {
                SessionEventBody::SessionStarted {
                    definition_id,
                    definition_version,
                }
            }),
            (arb_uuid(), any::<i64>()).prop_map(|(connection_id, hz)| {
                SessionEventBody::FrequencyChanged {
                    connection_id,
                    operating_frequency_hz: hz,
                }
            }),
            (arb_check_in_id(), any::<Option<u128>>()).prop_map(move |(check_in_id, cid)| {
                SessionEventBody::CheckinAdded {
                    check_in_id,
                    callsign: call.clone(),
                    client_event_id: cid.map(Uuid::from_u128),
                    signal_report: None,
                    staying: StayingStatus::InAndOut,
                    name: None,
                    location: None,
                    grid: None,
                    source: CheckInSource::Staff,
                    via: None,
                    relayed_by: None,
                }
            }),
            // A worked-set cursor move (Some on-roster / off-roster /
            // None) so the replay-composition property exercises the new kind.
            any::<Option<u128>>().prop_map(|id| SessionEventBody::StationWorkedSet {
                check_in_id: id.map(|i| Uuid::from_u128((i % 4) + 1)),
            }),
            Just(SessionEventBody::SessionNoteSet { note: None }),
            // The session-scoped ordering mode, so the total/
            // panic-free and replay-composition properties cover the new kind.
            any::<bool>().prop_map(|sink| SessionEventBody::RosterOrderModeSet {
                mode: if sink {
                    RosterOrderMode::WorkedSink
                } else {
                    RosterOrderMode::Manual
                },
            }),
            Just(SessionEventBody::SessionClosed),
            // The control-axis kinds, so the replay-composition and
            // idempotency properties exercise stall/resume/handoff too.
            Just(SessionEventBody::NcsStalled),
            Just(SessionEventBody::NcsResumed),
            arb_uuid().prop_map(|new_ncs_account_id| SessionEventBody::ControlHandedOff {
                new_ncs_account_id,
            }),
        ]
    }

    // A valid ordered log: strictly increasing `seq` from 1 (arbitrary positive
    // gaps), arbitrary `at`/`actor_id`, a mix of the four bodies.
    fn arb_log() -> impl Strategy<Value = Vec<SessionEvent>> {
        prop::collection::vec(
            (1u64..=5, any::<u64>(), any::<Option<u128>>(), arb_body()),
            0..12,
        )
        .prop_map(|specs| {
            let mut seq = 0u64;
            specs
                .into_iter()
                .map(|(gap, at, actor, body)| {
                    seq += gap;
                    SessionEvent {
                        seq,
                        actor_id: actor.map(Uuid::from_u128),
                        at,
                        body,
                    }
                })
                .collect()
        })
    }

    fn fold_all(events: &[SessionEvent]) -> SessionState {
        events.iter().fold(SessionState::default(), fold)
    }

    proptest! {
        // Property A — determinism: replay is a pure function of the ordered
        // input, and equals folding the log by hand.
        #[test]
        fn a_replay_is_deterministic(log in arb_log()) {
            prop_assert_eq!(replay(&log, 0), replay(&log, 0));
            prop_assert_eq!(fold_all(&log), replay(&log, 0));
        }

        // Property B(i) — replaying the whole log twice equals once: every
        // event in the second pass has `seq <= last_seq` and is inert, for
        // every kind.
        #[test]
        fn b_i_reapplying_the_log_is_inert(log in arb_log()) {
            let once = replay(&log, 0);
            let twice: Vec<SessionEvent> = log.iter().chain(log.iter()).cloned().collect();
            prop_assert_eq!(replay(&twice, 0), once);
        }

        // Property B(ii) — a `checkin.added` re-emitted at a NEW advancing
        // `seq` with a `check_in_id` already present does not grow the roster.
        #[test]
        fn b_ii_duplicate_check_in_id_does_not_grow_roster(log in arb_log()) {
            let base = replay(&log, 0);
            if let Some(existing) = base.roster.first().map(|r| r.check_in_id) {
                let call = crate::callsign::parse_callsign("N1CCK").expect("valid");
                let mut extended = log.clone();
                extended.push(SessionEvent {
                    seq: base.last_seq + 1,
                    actor_id: None,
                    at: u64::MAX,
                    body: SessionEventBody::CheckinAdded {
                        check_in_id: existing,
                        callsign: call,
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
                });
                prop_assert_eq!(replay(&extended, 0).roster.len(), base.roster.len());
            }
        }

        // Property C — replay composition / resume-correctness: for any split
        // `since`, folding the tail (`seq > since`) onto the prefix state
        // equals the full fold; and `replay(log, since)` reproduces exactly the
        // tail's contribution from empty. (clause 3.)
        #[test]
        fn c_replay_composition(log in arb_log(), split in any::<u64>()) {
            let max_seq = log.last().map(|e| e.seq).unwrap_or(0);
            let since = if max_seq == 0 { 0 } else { split % (max_seq + 1) };

            let whole = replay(&log, 0);

            let prefix: Vec<SessionEvent> =
                log.iter().filter(|e| e.seq <= since).cloned().collect();
            let tail: Vec<SessionEvent> =
                log.iter().filter(|e| e.seq > since).cloned().collect();

            let prefix_state = replay(&prefix, 0);
            let composed = tail.iter().fold(prefix_state, fold);
            prop_assert_eq!(composed, whole);

            // The delta a resuming client applies (from empty over the tail)
            // is exactly what `replay(log, since)` produces.
            prop_assert_eq!(replay(&log, since), replay(&tail, 0));
        }
    }
}
