// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
/**
 * The pure client-side event fold — a byte-for-byte re-implementation of
 * `netroll-domain::fold`. Given a session's ordered event log it projects a
 * [`SessionState`], mirroring the Rust rules EXACTLY:
 * - idempotency guard FIRST, every kind: `event.seq <= state.lastSeq` is an
 * inert no-op (`lastSeq` starts at 0; `seq` is 1-indexed);
 * - scalars are last-write-wins in `seq` order;
 * - the roster is append-with-dedupe keyed on `checkInId` (the entity id — NOT
 * `clientEventId`, which the fold never reads);
 * - `lastSeq` advances to the applied event's `seq`.
 *
 * Total and panic-free: it never rejects an ordering and never throws. Illegal
 * orderings (close-before-start) are PROJECTED, not refused — command-side
 * legality lives server-side (`session_sm.rs`) and is never re-implemented here.
 * Updates are immutable (a new state object is returned; the input is untouched)
 * per the architecture's Zustand immutability rule.
 *
 * The TS state keeps timestamps as their RFC 3339 wire strings (`WireEvent.at`),
 * NOT epoch millis: the snapshot seed (`SessionSummaryBody`) and the event
 * deltas both carry the same `rfc3339()`-formatted strings, so a seed-then-apply
 * path and a full-replay path produce identical state. [fold.rs:67-174]
 */

import {
  viaLabel,
  type ViaWire,
} from "../nets/connectionPresentation";
import type { NetConnection } from "../nets/netsApi";
import type {
  CheckInSource,
  ControlState,
  Precedence,
  RosterOrderMode,
  SessionLifecycle,
  StayingStatus,
  WireEvent,
} from "./sessionWire";

/**
 * A `via`'s identity as one comparable string, so "did it change" is a value
 * comparison and not a reference one. `null` is its own key, distinct from every
 * connection id and every free text — the two facts never collapse.
 */
function viaKey(via: ViaWire | null): string {
  if (via === null) {
    return "";
  }
  return via.kind === "connection" ? `c:${via.connectionId}` : `u:${via.text}`;
}

/**
 * One derived field correction — the client mirror of the
 * Rust `Correction`. Derived by the fold when a `checkin.updated` changes a
 * field, NEVER carried in the event payload, so it reconstructs identically on
 * replay/resume (matching the backend byte-for-byte). `field` is the kebab
 * token; `from`/`to` are display strings (`null` = absent on that side).
 *
 * `via` is the ONE field whose two sides are not display strings at fold time.
 * A connection id becomes a label only against the session's connection set,
 * and that set moves under the fold — a mid-session QSY re-labels a way in. So
 * this carries the STRUCTURED value in `fromVia`/`toVia` and the surface
 * resolves it at RENDER, exactly as the Rust `CorrectionValue::Via` does at
 * projection time. Freezing the label here made the live socket and a reload
 * disagree about the same past event, against this module's own invariant.
 */
export interface Correction {
  readonly field:
    | "callsign"
    | "name"
    | "location"
    | "grid"
    | "signal-report"
    | "staying"
    | "precedence"
    | "traffic"
    | "via"
    | "relayedBy";
  readonly from: string | null;
  readonly to: string | null;
  /**
   * The STRUCTURED `via` on each side, present only on a `via` correction the
   * browser's own fold derived. Absent on a correction that arrived already
   * resolved from the server snapshot, whose `from`/`to` strings stand.
   *
   * `null` is a value here and means "no way in on that side"; `undefined` means
   * "this correction carries no structured value", which is a different thing.
   */
  readonly fromVia?: ViaWire | null;
  readonly toVia?: ViaWire | null;
  readonly at: string;
}

/** One projected roster station — shape-identical to the wire `RosterEntryBody`. */
export interface RosterEntry {
  readonly checkInId: string;
  readonly callsign: string;
  readonly addedAt: string;
  readonly addedBy: string | null;
  /** Who created this entry — `staff`/`self`. Defaults to `staff`
   * for a historical/omitted-source add (additive-compat). */
  readonly source: CheckInSource;
  /** The mode-shaped signal report, or `null` when absent. */
  readonly signalReport: string | null;
  /** The staying status. Defaults to `in-and-out` for an OLD entry — such a
   * `checkin.added` delta carries no key. Not for a public one: the public
   * `checkin.added` arm projects `staying`, so an
   * observer's live entry gets the operator's value, never this default. */
  readonly staying: StayingStatus;
  /** The operator name, or `null` (edit-only). */
  readonly name: string | null;
  /** The free-text location, or `null` (edit-only). */
  readonly location: string | null;
  /** The Maidenhead grid, or `null`. Deliberately widened
   * from the wire's omitted-when-absent `?: string` to `string | null` after the
   * fold, exactly as `location` is — distinct from it, never derived from it. */
  readonly grid: string | null;
  /** The traffic/emergency precedence; defaults to `routine` at add. */
  readonly precedence: Precedence;
  /** The declared traffic count, or `null` when none. */
  readonly traffic: number | null;
  /** The per-station STAFF note, or `null` (edit-only). REPLACE
   * semantics; derives NO correction (running commentary, not a corrected
   * mis-entry). Operator-only — the public wire never carries it. */
  readonly notes: string | null;
  /** The per-station PUBLIC note, or `null` (edit-only). Same
   * REPLACE semantics and same no-correction rule as `notes`; unlike `notes` it
   * rides the public wire, so every observer reads it. */
  readonly publicNote: string | null;
  /** WHICH way in this station arrived on, or `null` when
   * nobody recorded one — a FACT in its own right, never interchangeable with
   * "the net's first connection". Structured; the LABEL is a projection,
   * resolved against `connections` by `viaLabel`. */
  readonly via: ViaWire | null;
  /** WHICH STATION passed this check-in's traffic, or `null`
   * when nobody relayed it. A DIFFERENT fact from `via`: one says how the
   * traffic travelled, the other who passed it. A callsign needs no resolution,
   * so there is no label projection beside it. */
  readonly relayedBy: string | null;
  /** Whether the working cursor has LEFT this entry; `false` at add,
   * set `true` when the cursor moves away. Monotonic within a run. */
  readonly worked: boolean;
  /** The fold-derived CAS version: 1 at add, +1 per applied update. */
  readonly version: number;
  /** The derived per-field corrections, newest last. */
  readonly corrections: readonly Correction[];
}

/**
 * The state projected from a session's ordered event log — the TS mirror of the
 * Rust `SessionState`. Unset scalars are `null` (TS-idiomatic); `lastSeq` is the
 * snapshot cursor (0 = no events applied); `roster` preserves `seq` order.
 */
export interface SessionState {
  readonly lifecycle: SessionLifecycle;
  readonly startedAt: string | null;
  readonly closedAt: string | null;
  /**
   * Every way the session is reachable right now, in the owner's order — seeded
   * from the snapshot and moved by `frequency.changed`.
   *
   * Empty before a snapshot seeds it. The reducer NEVER invents a connection:
   * a `frequency.changed` naming an id the list does not hold is inert, which
   * is the same answer the server gives that write.
   */
  readonly connections: readonly NetConnection[];
  readonly definitionId: string | null;
  readonly definitionVersion: number | null;
  readonly roster: readonly RosterEntry[];
  /** The single working-station cursor, or `null` — the `checkInId`
   * of the station being worked. "At most one" holds by construction. */
  readonly workingCheckInId: string | null;
  /** The net-level note, or `null`. */
  readonly netNote: string | null;
  /** The standing roster ordering mode. INERT in this reducer, as
   * it is in the Rust fold: no case reads it and no case sorts. Order still
   * arrives only as a `roster.reordered` permutation. */
  readonly rosterOrderMode: RosterOrderMode;
  /** The projected control status: `active` (default) or
   * `stalled`. Orthogonal to `lifecycle` — a stalled session is still `live`.
   * The `net-paused` connection render derives from this (`displayedConnection`). */
  readonly controlState: ControlState;
  /** The single active NCS's account id, or `null`. Operator id —
   * present on the owner fold, always `null` on the redacted public fold. */
  readonly activeNcsAccountId: string | null;
  /** When the session stalled, RFC 3339 wire string (the `ncs.stalled` event's
   * `at`), or `null` while active. */
  readonly stalledAt: string | null;
  readonly lastSeq: number;
}

/** The default projection before any event folds — `scheduled`, empty, cursor 0. */
export const DEFAULT_SESSION_STATE: SessionState = {
  lifecycle: "scheduled",
  startedAt: null,
  closedAt: null,
  connections: [],
  definitionId: null,
  definitionVersion: null,
  roster: [],
  workingCheckInId: null,
  netNote: null,
  rosterOrderMode: "manual",
  controlState: "active",
  activeNcsAccountId: null,
  stalledAt: null,
  lastSeq: 0,
};

/**
 * Projects a single wire event onto the state, returning the new state. Events
 * whose `seq` does not advance `lastSeq` are inert no-ops (the SAME reference is
 * returned). Mirrors `netroll_domain::fold::fold` exactly.
 */
export function fold(state: SessionState, event: WireEvent): SessionState {
  // Monotonic idempotency guard: a non-advancing seq is inert for every kind.
  if (event.seq <= state.lastSeq) {
    return state;
  }

  const advanced = { ...state, lastSeq: event.seq };

  switch (event.kind) {
    case "session.started":
      return {
        ...advanced,
        lifecycle: "live",
        startedAt: event.at,
        definitionId: event.payload.definitionId,
        definitionVersion: event.payload.definitionVersion,
        // The starter (envelope actorId) is the initial active
        // NCS; controlState defaults active. On the redacted public wire actorId
        // is null, so a public fold seeds a null active NCS (correct).
        activeNcsAccountId: event.actorId ?? null,
        controlState: "active",
        stalledAt: null,
      };
    case "frequency.changed":
      // Moves the ONE connection the event names and no other. A net with three
      // ways to reach it retunes one of them without the other two silently
      // inheriting the number.
      return {
        ...advanced,
        connections: state.connections.map((connection) =>
          connection.id === event.payload.connectionId
            ? {
                ...connection,
                plannedFrequencyHz: event.payload.operatingFrequencyHz,
              }
            : connection,
        ),
      };
    case "checkin.added": {
      // Entity-level dedupe on checkInId (NOT clientEventId): a duplicate id,
      // even at a distinct advancing seq, does not grow the roster.
      const alreadyPresent = state.roster.some(
        (entry) => entry.checkInId === event.payload.checkInId,
      );
      if (alreadyPresent) {
        return advanced;
      }
      return {
        ...advanced,
        roster: [
          ...state.roster,
          {
            checkInId: event.payload.checkInId,
            callsign: event.payload.callsign,
            addedAt: event.at,
            // OMITTED actorId → null-when-absent roster addedBy.
            addedBy: event.actorId ?? null,
            // The staff/self provenance; an omitted-source legacy add folds
            // to `staff` (additive-compat).
            source: event.payload.source ?? "staff",
            // OMITTED signalReport → null (still the public case: the report is
            // one of the fields that stayed redacted). Absent staying → the
            // in-and-out default, which is the HISTORICAL case only — an older
            // delta with no key. The public add arm now projects `staying`, so
            // a live observer folds the operator's value here (additive-compat).
            signalReport: event.payload.signalReport ?? null,
            staying: event.payload.staying ?? "in-and-out",
            // Name/location are captured at add time from the per-net roster
            // memory prefill; an absent key (older, public or callsign-only)
            // folds to null. The CAS version starts at 1;
            // corrections accrue only via updates.
            name: event.payload.name ?? null,
            location: event.payload.location ?? null,
            // An absent key (older, public or grid-less add) folds
            // to null, the same additive-compat rule name/location follow.
            grid: event.payload.grid ?? null,
            // Precedence/traffic are edit-only — the conservative
            // routine default, no declared traffic, at add.
            precedence: "routine",
            traffic: null,
            // Both notes are edit-only; worked starts
            // false (not yet worked-and-left).
            notes: null,
            publicNote: null,
            // The way in is captured AT ADD, unlike the eight
            // edit-only fields; an absent key (older, or a check-in nobody
            // recorded a way in for) folds to null.
            via: event.payload.via ?? null,
            // Set AT ADD like `via`. A public delta never carries
            // the key, so this `?? null` is both the redacted-wire path
            // and the genuine "not relayed" one — which are the same fact here.
            relayedBy: event.payload.relayedBy ?? null,
            worked: false,
            version: 1,
            corrections: [],
          },
        ],
      };
    }
    case "checkin.updated": {
      // Find the entry by its stable id; a no-op when absent (the add-dedupe
      // posture — total, mirroring the Rust fold).
      const index = state.roster.findIndex(
        (entry) => entry.checkInId === event.payload.checkInId,
      );
      if (index === -1) {
        return advanced;
      }
      const prev = state.roster[index];
      const nextCallsign = event.payload.callsign;
      const nextName = event.payload.name ?? null;
      const nextLocation = event.payload.location ?? null;
      const nextGrid = event.payload.grid ?? null;
      const nextReport = event.payload.signalReport ?? null;
      const nextStaying = event.payload.staying ?? "in-and-out";
      // Do not assume the account-less `checkin.updated` payload omits
      // `precedence`: it does not. The public `checkin.updated` arm projects
      // `checkInId`, `callsign`, `staying`, `precedence` and — when present —
      // `traffic` and `publicNote`. A live public delta CARRIES the precedence
      // the operator set, and that value, not this fallback, is what an
      // observer sees.
      //
      // The fallback stays and its reason narrows to HISTORICAL payloads, exactly
      // as `nextPublicNote`'s does below: an older `checkin.updated`
      // event in the append-only log has no `precedence` key at all, and without
      // this `??` it would fold to `undefined`, corrupting the entry's type and
      // deriving a bogus correction on replay. It is never the source of a live
      // observer's value.
      const nextPrecedence = event.payload.precedence ?? "routine";
      const nextTraffic = event.payload.traffic ?? null;
      // Notes REPLACE last-write-wins with NO correction derived:
      // a note is running commentary, not a corrected mis-entry. `?? null` for the
      // same omit-optional / redacted-public-wire discipline every optional uses.
      const nextNotes = event.payload.notes ?? null;
      // The PUBLIC note rides the public delta, so unlike `notes`
      // this `?? null` is the HISTORICAL-payload path only — a live public
      // delta carries the key. The fallback stays because an older event in
      // the log has no key at all.
      const nextPublicNote = event.payload.publicNote ?? null;
      // `via` rides the public delta too, so this `?? null` is the
      // historical-payload path AND the genuine "cleared it" path — an explicit
      // clear omits the key, exactly as every other optional does.
      const nextVia = event.payload.via ?? null;
      // The historical-payload path AND the genuine "cleared it"
      // path — an explicit clear omits the key, as every other optional does.
      const nextRelayedBy = event.payload.relayedBy ?? null;
      // Derive a Correction per CHANGED field, in the SAME order as the Rust
      // fold (callsign, name, location, grid, signal-report, staying,
      // precedence, traffic) so the derived annotations reconstruct
      // byte-identically.
      const corrections: Correction[] = [...prev.corrections];
      const at = event.at;
      if (prev.callsign !== nextCallsign) {
        corrections.push({ field: "callsign", from: prev.callsign, to: nextCallsign, at });
      }
      if (prev.name !== nextName) {
        corrections.push({ field: "name", from: prev.name, to: nextName, at });
      }
      if (prev.location !== nextLocation) {
        corrections.push({ field: "location", from: prev.location, to: nextLocation, at });
      }
      if (prev.grid !== nextGrid) {
        corrections.push({ field: "grid", from: prev.grid, to: nextGrid, at });
      }
      if (prev.signalReport !== nextReport) {
        corrections.push({ field: "signal-report", from: prev.signalReport, to: nextReport, at });
      }
      if (prev.staying !== nextStaying) {
        corrections.push({ field: "staying", from: prev.staying, to: nextStaying, at });
      }
      if (prev.precedence !== nextPrecedence) {
        corrections.push({ field: "precedence", from: prev.precedence, to: nextPrecedence, at });
      }
      if (prev.traffic !== nextTraffic) {
        corrections.push({
          field: "traffic",
          // `from`/`to` are display strings; a null count renders as absent.
          from: prev.traffic === null ? null : String(prev.traffic),
          to: nextTraffic === null ? null : String(nextTraffic),
          at,
        });
      }
      // `via` annotates LAST, after the eight fields that shipped
      // before it, mirroring the Rust fold's order byte-for-byte.
      //
      // The STRUCTURED value rides along and is what a surface resolves. The
      // labels below are a fold-time snapshot for any consumer that only reads
      // strings; `buildRoster` re-resolves from `fromVia`/`toVia` against the
      // connection set as it stands AT RENDER. Freezing the label here made the
      // live socket and a reload disagree about the same past event after a
      // mid-session QSY — the server resolves at projection time, so it always
      // showed the current label and the browser showed a stale one.
      if (viaKey(prev.via) !== viaKey(nextVia)) {
        corrections.push({
          field: "via",
          from: viaLabel(prev.via, state.connections),
          to: viaLabel(nextVia, state.connections),
          fromVia: prev.via,
          toVia: nextVia,
          at,
        });
      }
      // Annotates LAST, after `via`, mirroring the Rust fold's
      // order byte-for-byte. `from`/`to` are the callsigns themselves — there is
      // no structured sibling to carry, because there is nothing to re-resolve.
      if (prev.relayedBy !== nextRelayedBy) {
        corrections.push({
          field: "relayedBy",
          from: prev.relayedBy,
          to: nextRelayedBy,
          at,
        });
      }
      const roster = [...state.roster];
      roster[index] = {
        ...prev,
        callsign: nextCallsign,
        name: nextName,
        location: nextLocation,
        grid: nextGrid,
        signalReport: nextReport,
        staying: nextStaying,
        precedence: nextPrecedence,
        traffic: nextTraffic,
        // Notes are replaced with no correction pushed above.
        notes: nextNotes,
        publicNote: nextPublicNote,
        via: nextVia,
        relayedBy: nextRelayedBy,
        version: prev.version + 1,
        corrections,
      };
      return { ...advanced, roster };
    }
    case "checkin.removed":
      // Tombstone: drop the row from the projected roster (the log retains it).
      // A removed entry must not remain the phantom cursor target:
      // clear the cursor if it pointed at the removed row.
      return {
        ...advanced,
        roster: state.roster.filter(
          (entry) => entry.checkInId !== event.payload.checkInId,
        ),
        workingCheckInId:
          state.workingCheckInId === event.payload.checkInId
            ? null
            : state.workingCheckInId,
      };
    case "station.worked-set": {
      // Move the SINGLE working-cursor pointer, mirroring the
      // Rust fold byte-for-byte. THIS CASE never reorders, bumps a version, or
      // derives corrections. An off-roster / unknown target is a total no-op.
      //
      // The second half is qualified: under the worked-sink ordering mode the
      // SERVER'S command
      // boundary appends a roster.reordered alongside the worked-set, so the
      // roster order DOES change when a station is marked worked — it just
      // arrives as its own delta, applied by the reorder case below.
      const target = event.payload.checkInId;
      if (target !== null) {
        const onRoster = state.roster.some((e) => e.checkInId === target);
        if (!onRoster) {
          return advanced;
        }
      }
      const prev = state.workingCheckInId;
      // Mark any DIFFERENT prior-working entry worked — the cursor has left it.
      const roster =
        prev !== null && prev !== target
          ? state.roster.map((e) => (e.checkInId === prev ? { ...e, worked: true } : e))
          : state.roster;
      return { ...advanced, roster, workingCheckInId: target };
    }
    case "session.note-set":
      // REPLACE the net-level note last-write-wins; an omitted
      // note key (cleared) folds to null via the same `?? null` discipline.
      return { ...advanced, netNote: event.payload.note ?? null };
    case "roster.order-mode-set":
      // REPLACE the standing ordering mode last-write-wins,
      // mirroring the Rust fold. An absent key or a token from a newer server
      // folds to the legacy default — the reducer is total. A wrong ORDER is
      // still impossible either way, because order comes only from
      // roster.reordered; but the public page's your-turn
      // selector READS this field, so a wrong MODE is a wrong answer. That is
      // why the public delta now carries the token (it used to project `{}`) —
      // the fallback here is defensive, not an expected shape.
      return {
        ...advanced,
        rosterOrderMode:
          event.payload.mode === "worked-sink" ? "worked-sink" : "manual",
      };
    case "roster.reordered": {
      // A dumb projection: reorder to the given checkInId
      // sequence, then append any entry NOT named in `order` in its current
      // relative order. Unknown ids in `order` are ignored. Mirrors the Rust
      // fold — no version bump, no corrections.
      const remaining = [...state.roster];
      const reordered: RosterEntry[] = [];
      for (const id of event.payload.order) {
        const pos = remaining.findIndex((entry) => entry.checkInId === id);
        if (pos !== -1) {
          reordered.push(remaining.splice(pos, 1)[0]);
        }
      }
      return { ...advanced, roster: [...reordered, ...remaining] };
    }
    case "session.closed":
      // A closed session accepts no further station.worked-set events
      // (review finding): clear a lingering working cursor the same way
      // checkin.removed does, so "currently working" never renders as a
      // permanent artifact on a closed roster. There is no closedBy.
      return {
        ...advanced,
        lifecycle: "closed",
        closedAt: event.at,
        workingCheckInId: null,
        // A closed net is neither active nor stalled — reset the
        // control axis to its inert defaults so a closed session never renders
        // net-paused.
        controlState: "active",
        stalledAt: null,
      };
    case "ncs.stalled":
      // The active NCS's presence dropped. The lifecycle is
      // UNTOUCHED (still live) — this is the orthogonal control axis.
      return { ...advanced, controlState: "stalled", stalledAt: event.at };
    case "ncs.resumed":
      // The active NCS returned before auto-close — back to
      // active under the SAME NCS; clear the stall instant.
      return { ...advanced, controlState: "active", stalledAt: null };
    case "control.handed-off":
      // A handoff or claim moves the active NCS and always
      // brings the net back to active under the new controller. On the redacted
      // public wire `newNcsAccountId` is omitted → null (the fold still derives
      // controlState = active from the kind).
      return {
        ...advanced,
        activeNcsAccountId: event.payload.newNcsAccountId ?? null,
        controlState: "active",
        stalledAt: null,
      };
    case "station.blocked":
      // An NCS blocked a disruptive account for this
      // session. The block is enforced SERVER-SIDE (a blocked account's
      // self-check-in returns 403) and its roster effect rides the accompanying
      // `checkin.removed` — so the client needs NO blocklist state. Fold it as an
      // advancing no-op (mirrors the Rust fold's dedup push, which is invisible to
      // the projection the UI renders). The public wire redacts `accountId` to
      // `{}` anyway, so there is nothing identity-bearing to record here.
      return advanced;
    default:
      // Kind tokens are an additive-only server contract (sessionWire.ts):
      // an old client can receive a `kind` newer than its own union before it
      // has redeployed. Total/panic-free means absorbing it as a no-op that
      // still advances the cursor, not throwing or returning `undefined`.
      return advanced;
  }
}

/**
 * Reconstructs session state by folding the events with `seq > since` from the
 * default state. `replay(log, 0)` is the full reconstruction; `replay(log,
 * since)` is the delta a resuming client applies. Events MUST already be in
 * non-decreasing `seq` order (this filters and folds; it does not sort).
 * Mirrors `netroll_domain::fold::replay`.
 */
export function replay(events: readonly WireEvent[], since: number): SessionState {
  return events
    .filter((event) => event.seq > since)
    .reduce(fold, DEFAULT_SESSION_STATE);
}
