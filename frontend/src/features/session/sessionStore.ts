// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
/**
 * The single event-reducer store for a live session (Zustand 5). All
 * business logic lives in the pure `sessionReducer` fold; this store only
 * holds the folded state, the connection state, and the optimistic-write pending
 * map, routing every mutation through the reducer.
 *
 * A vanilla store FACTORY (`createSessionStore`) rather than a module-level hook:
 * the live-session page needs one store per session id, and a factory keeps each
 * instance isolated and unit-testable without React. The `useSessionStream` hook
 * binds an instance to a component via `useStore`.
 *
 * Optimistic writes are built and proven here as a STORE MECHANISM — there
 * is no live check-in send yet. The originating client inserts
 * a local pending entry keyed by `clientEventId`; when the authoritative
 * `checkin.added` echoes that same id, the pending entry is dropped and the
 * folded roster row renders (no duplicate). A viewer (empty pending map) renders
 * the authoritative roster only — it never shows another client's pending write.
 */

import { createStore } from "zustand/vanilla";
import type { StoreApi } from "zustand/vanilla";

import { viaLabel, type ViaWire } from "../nets/connectionPresentation";
import type { NetConnection } from "../nets/netsApi";
import { DEFAULT_SESSION_STATE, fold } from "./sessionReducer";
import type { Correction, RosterEntry, SessionState } from "./sessionReducer";
import type {
  CheckInSource,
  ControlState,
  DefinitionSnapshot,
  Precedence,
  PublicSessionSummary,
  RosterOrderMode,
  SessionSummaryBody,
  StayingStatus,
  ViewerRole,
  WireEvent,
} from "./sessionWire";

/**
 * The fixed connection-state vocabulary. No ad-hoc
 * booleans. `catching-up` is the initial state (establishing the stream) and
 * the reconnect state; `live` once a live frame flows; `out-of-sync` after
 * persistent recovery failure. `net-paused` is renderable but has NO server
 * trigger yet — nothing here ever sets it.
 */
export type ConnectionState =
  | "live"
  | "catching-up"
  | "out-of-sync"
  | "net-paused";

/** An un-echoed optimistic check-in the originating client inserted locally. */
export interface PendingEntry {
  readonly clientEventId: string;
  readonly callsign: string;
  /** The mode-shaped signal report the operator typed, or `null`. */
  readonly signalReport: string | null;
  /** The staying status the operator selected. */
  readonly staying: StayingStatus;
  /** Whether this optimistic row is a staff quick-add or a participant
   * self-check-in — drives the pending row's source badge. */
  readonly source: CheckInSource;
  /** WHICH way in the operator recorded for this station — the
   * session stamp or the per-station override, carried so the dimmed row shows
   * it at once instead of waiting for the echo to say what was already chosen. */
  readonly via: ViaWire | null;
  /** WHICH STATION the operator said passed this station's traffic, carried
   * so the dimmed row shows their answer at once rather than
   * waiting for the echo to confirm what they already typed. */
  readonly relayedBy: string | null;
}

/** Frozen per-session metadata carried by the snapshot but not by the fold. */
export interface SessionMeta {
  readonly id: string;
  readonly definition: DefinitionSnapshot;
  /**
   * The viewer's OWN resolved role, or `null` for the redacted
   * public snapshot (which carries no role). The console reads this to gate
   * role-aware affordances (UX only — the server stays authoritative).
   */
  readonly viewerRole: ViewerRole | null;
}

/**
 * One ephemeral soft-lock lease held on a check-in, keyed
 * in the store's `locks` slice by `checkInId`. Advisory UI state ONLY — NOT part
 * of `SessionState`, NOT folded, NOT seeded from the snapshot.
 */
export interface LockEntry {
  readonly holderCallsign: string;
  readonly expiresAt: string;
}

/** The `lock` frame delta the socket routes to `applyLock`. */
export interface LockDelta {
  readonly checkInId: string;
  readonly holderCallsign: string | null;
  readonly expiresAt: string | null;
}

/** One roster row for display — authoritative or optimistic. */
export interface DisplayRosterEntry {
  /** Stable React key: the `checkInId` when authoritative, `clientEventId` when pending. */
  readonly key: string;
  readonly callsign: string;
  /** True for a not-yet-echoed local optimistic entry. */
  readonly optimistic: boolean;
  /**
   * When this station was added — the RFC 3339 wire string the "Heard" roster
   * column renders as a relative time (`humanizeTime`). An authoritative row
   * carries the fold's own `addedAt`; a still-pending optimistic row has no
   * server timestamp yet, so it is stamped with the render instant it was
   * built at (`buildRoster`'s `nowMillis`) — it was, after all, just added.
   */
  readonly addedAt: string;
  /**
   * Who created this check-in — the account id, or `null` on a still-pending
   * optimistic row (no server identity yet) or a historical/redacted entry
   * that carries none. Lets a consumer derive e.g. the post-net summary's NCS
   * callsign by matching this against `session.activeNcsAccountId`.
   */
  readonly addedBy: string | null;
  /**
   * Who created this check-in, for the source badge. Read
   * from the folded `entry.source` rather than a hard-coded `"staff"`: a
   * staff `LogCheckIn` add is `"staff"`, a participant self-check-in is
   * `"self"`. Rendered on BOTH the operator console AND the account-less public
   * roster via `RosterEntry`'s `showSource`.
   */
  readonly source: CheckInSource;
  /** The mode-shaped signal report to display verbatim, or `null`. */
  readonly signalReport: string | null;
  /** The staying status the roster indicator renders. */
  readonly staying: StayingStatus;
  /** The operator name, or `null` (edit-only). */
  readonly name: string | null;
  /** The free-text location, or `null` (edit-only). */
  readonly location: string | null;
  /** The Maidenhead grid, or `null`. Rendered in the detail modal;
   * deliberately NOT a roster column — `location` has none either. */
  readonly grid: string | null;
  /** The traffic/emergency precedence chip to render. */
  readonly precedence: Precedence;
  /** The declared traffic count, or `null` when none. */
  readonly traffic: number | null;
  /** The per-station STAFF note, or `null`. Operator-only: rendered
   * in the detail modal, never on a roster row and never on a public surface. */
  readonly notes: string | null;
  /** The per-station PUBLIC note, or `null`. Rendered on the roster
   * row for EVERY observer — the operator console, a signed-in participant and
   * an account-less visitor alike. */
  readonly publicNote: string | null;
  /** WHICH way in this station arrived on, structured — kept
   * beside the label so a later editor has the value to send back. */
  readonly via: ViaWire | null;
  /** The way in as a person reads it, or `null` when nobody recorded one.
   * Resolved HERE, against the session's own connections, so a component never
   * has to hold the set — and so a mid-net frequency move re-labels the row. */
  readonly viaLabel: string | null;
  /** WHICH STATION passed this station's traffic, or `null` when
   * nobody relayed it. No label sibling: a callsign is already what a person
   * reads, so there is nothing to resolve it against. */
  readonly relayedBy: string | null;
  /** True when this entry currently holds the working cursor —
   * derived from the session's `workingCheckInId`. The WorkingCursor render takes
   * visual precedence over `worked`. */
  readonly working: boolean;
  /** True when the cursor has already left this entry — drives the
   * 0.55-opacity + green-tick worked treatment. */
  readonly worked: boolean;
  /** The fold-derived CAS version the detail modal sends as `expectedVersion`. */
  readonly version: number;
  /** The derived per-field corrections for the amber annotations. */
  readonly corrections: readonly Correction[];
  /**
   * The callsign of ANOTHER operator currently editing this entry, or `null`. When set, the row renders read-only with "{callsign} is
   * editing…" and a disabled edit control. The entry THIS operator has open in
   * the modal is never flagged as locked-by-other.
   */
  readonly lockedBy: string | null;
}

/** The store's state + actions. */
export interface SessionStore {
  readonly session: SessionState;
  readonly connection: ConnectionState;
  readonly meta: SessionMeta | null;
  readonly pending: ReadonlyMap<string, PendingEntry>;
  /**
   * The ephemeral soft-lock slice, keyed by `checkInId` — a
   * SEPARATE store slice, NOT part of `SessionState`, NOT folded, NOT seeded
   * from the snapshot. Updated only by the `lock` WS frame via `applyLock`.
   */
  readonly locks: ReadonlyMap<string, LockEntry>;
  /**
   * `clientEventId` → `checkInId` for every `checkin.added` echo this store has
   * folded. The fold drops `clientEventId`, so this is the only record of which
   * authoritative row one of THIS client's own commits produced — what lets the
   * quick-add's live duplicate warning keep excluding its own row after the
   * echo replaces the pending entry. Survives `seedFromSnapshot`: these are
   * server-confirmed ids, not optimistic state.
   */
  readonly echoed: ReadonlyMap<string, string>;
  /**
   * WHICH way in this operator is taking check-ins on right now — the
   * quick-add's default `via`, or `null` for "nobody has said".
   *
   * PER OPERATOR, IN MEMORY, and deliberately none of the three alternatives.
   * It is NOT a session event: a session-level fact is SHARED, and a Logger
   * monitoring EchoLink beside an NCS on HF would then have whichever of them
   * stamped last decide what the OTHER one's check-ins claim — a confidently
   * wrong band written into an append-only log that reaches LoTW and QRZ. It is
   * NOT persisted per session either: a stamp restored after a reload is a value
   * whose age the operator cannot see, which is the "stale sticky value" the
   * capture ruling exists to prevent. And it is NOT inside `session`, because
   * `seedFromSnapshot` replaces that wholesale on every authoritative reset.
   *
   * A reload therefore returns it to `null`, and the check-ins that follow
   * record no way in until it is set again. That is the truthful answer, and
   * it is only acceptable because the control always says what is stamped.
   *
   * It survives a reseed, with ONE exception: a stamp
   * naming a connection the incoming snapshot no longer lists is dropped, so
   * the control can never claim a stamp is set while `sendableVia` refuses it.
   * See `survivingViaStamp`.
   */
  readonly viaStamp: ViaWire | null;
  /**
   * Seed the folded state + frozen metadata from a snapshot summary. Accepts
   * BOTH the owner `SessionSummaryBody` and the REDACTED `PublicSessionSummary`
   * The redacted view omits `definitionId`/`definitionVersion` and
   * each roster entry omits `addedBy`, all of which project to `null` here — the
   * public live view needs none of them (it renders roster + frequency only).
   */
  seedFromSnapshot(summary: SessionSummaryBody | PublicSessionSummary): void;
  /** Fold one live delta, reconciling any matching optimistic pending entry. */
  applyEvent(event: WireEvent): void;
  /** Fold a catch-up batch onto the current state, reconciling as it goes. */
  applyCatchUp(events: readonly WireEvent[]): void;
  /**
   * Insert a local optimistic pending entry keyed by `clientEventId`, carrying
   * the report/staying the operator entered so the dimmed row reflects them
   * Omitted report → `null`; omitted staying → `in-and-out`.
   */
  addPending(
    clientEventId: string,
    callsign: string,
    signalReport?: string | null,
    staying?: StayingStatus,
    source?: CheckInSource,
    via?: ViaWire | null,
    relayedBy?: string | null,
  ): void;
  /**
   * Roll back a local optimistic pending entry — the companion `addPending` has
   * always needed. Called on a failed send, and by
   * the quick-add's bounded timeout when an echo never arrives, so a dimmed row
   * cannot render forever. Symmetric to `addPending`: an immutable clone-and-
   * delete that returns the SAME map reference when the key is absent (mirroring
   * `reconcilePending`'s no-op preservation) so subscribers are not needlessly
   * re-notified. Reconciliation of an echoed entry stays automatic — this is
   * only the failure/timeout path.
   */
  removePending(clientEventId: string): void;
  /**
   * Apply a `lock` frame to the ephemeral `locks` slice: a
   * `holderCallsign` sets/renews the lease entry; a `null` holder (release)
   * drops it. Returns the SAME map reference when nothing changed so subscribers
   * are not needlessly re-notified. NEVER touches `session`/the fold.
   */
  applyLock(delta: LockDelta): void;
  /** Move to a new connection state (the fixed vocabulary only). */
  setConnection(connection: ConnectionState): void;
  /**
   * Set (or clear, with `null`) the way in this operator is taking check-ins on.
   * Survives `seedFromSnapshot` by design, except when that snapshot takes the
   * connection it names away — see {@link SessionStore.viaStamp}.
   */
  setViaStamp(via: ViaWire | null): void;
}

/**
 * Projects the folded-state fields carried by a snapshot summary. Tolerant of
 * the REDACTED public view: a missing `definitionId` /
 * `definitionVersion` / roster `addedBy` projects to `null` (the owner summary
 * always carries them; the public one never does).
 */
function stateFromSummary(
  summary: SessionSummaryBody | PublicSessionSummary,
): SessionState {
  const redactable = summary as Partial<SessionSummaryBody>;
  return {
    lifecycle: summary.lifecycle,
    startedAt: summary.startedAt,
    closedAt: summary.closedAt,
    // `?? []` like every neighbour here: a summary without the key would make
    // `viaLabel` call `.find` on `undefined` and throw the whole roster render.
    connections: summary.connections ?? [],
    definitionId: redactable.definitionId ?? null,
    definitionVersion: redactable.definitionVersion ?? null,
    roster: summary.roster.map((entry) => ({
      checkInId: entry.checkInId,
      callsign: entry.callsign,
      addedAt: entry.addedAt,
      addedBy: (entry as { addedBy?: string | null }).addedBy ?? null,
      // `source` is projected on BOTH the owner and the public roster
      // An older snapshot entry without the key folds to `staff`.
      source: (entry as { source?: CheckInSource }).source ?? "staff",
      // The public roster omits both; the owner roster carries them. Default a
      // missing report to null and a missing staying to in-and-out.
      signalReport: (entry as { signalReport?: string }).signalReport ?? null,
      staying: (entry as { staying?: StayingStatus }).staying ?? "in-and-out",
      // Owner-summary-only fields, absent on the public
      // view (which never renders name/location/corrections). Version defaults
      // to 1 (a public snapshot's entries are effectively version-1 adds).
      name: (entry as { name?: string }).name ?? null,
      location: (entry as { location?: string }).location ?? null,
      // Present on the owner summary, absent on the public view.
      grid: (entry as { grid?: string }).grid ?? null,
      // Precedence and traffic — present on the owner summary, absent on the public
      // view. Default a missing precedence to routine and traffic to null.
      precedence: (entry as { precedence?: Precedence }).precedence ?? "routine",
      traffic: (entry as { traffic?: number }).traffic ?? null,
      // The STAFF note is present on the owner summary and
      // absent on the public view; `worked` likewise. Default a missing note to
      // null and worked to false so a public snapshot folds cleanly.
      notes: (entry as { notes?: string }).notes ?? null,
      // The PUBLIC note is present on BOTH views (omitted only when
      // the operator has not written one), so this `?? null` is the
      // absent-value path, not a redaction fallback.
      publicNote: (entry as { publicNote?: string }).publicNote ?? null,
      // `via` is present on BOTH the owner and the public roster
      // (omitted only when nobody recorded one), so this `?? null` is the
      // absent-value path and not a redaction fallback.
      via: (entry as { via?: ViaWire }).via ?? null,
      // Present on the OWNER summary only — the public view refuses
      // it, so this `?? null` IS a redaction fallback as well as the
      // absent-value path, and the two are the same fact for a reader.
      relayedBy: (entry as { relayedBy?: string }).relayedBy ?? null,
      worked: (entry as { worked?: boolean }).worked ?? false,
      version: (entry as { version?: number }).version ?? 1,
      corrections: (entry as { corrections?: readonly Correction[] }).corrections ?? [],
    })),
    // The worked-station cursor is public radio data (present on both views);
    // the net note is owner-only (absent on the public view → null).
    workingCheckInId: summary.workingCheckInId ?? null,
    netNote: (summary as Partial<SessionSummaryBody>).netNote ?? null,
    // BOTH summaries carry the mode — the public page's your-turn selector
    // reads it — so the `?? "manual"` is a fallback for an older snapshot, not a
    // redaction. The public page still renders neither the toggle nor the
    // collapse.
    rosterOrderMode:
      (summary as Partial<SessionSummaryBody>).rosterOrderMode ?? "manual",
    // controlState is public radio data (present on BOTH views;
    // default active for an older snapshot). activeNcsAccountId is owner-only —
    // a public snapshot omits it → null (redacted).
    controlState: summary.controlState ?? "active",
    activeNcsAccountId: redactable.activeNcsAccountId ?? null,
    stalledAt: null,
    lastSeq: summary.latestSeq,
  };
}

/**
 * Drops a pending entry when its `clientEventId` is echoed by an applied
 * `checkin.added` event. Returns the same map reference when nothing changed so
 * subscribers are not needlessly re-notified.
 */
function reconcilePending(
  pending: ReadonlyMap<string, PendingEntry>,
  event: WireEvent,
): ReadonlyMap<string, PendingEntry> {
  if (event.kind !== "checkin.added") {
    return pending;
  }
  const echoed = event.payload.clientEventId;
  if (echoed === undefined || !pending.has(echoed)) {
    return pending;
  }
  const next = new Map(pending);
  next.delete(echoed);
  return next;
}

/**
 * Remembers which `checkInId` a `checkin.added` echo's `clientEventId` became.
 * Returns the same map reference when the event carries no `clientEventId`.
 *
 * Recorded whether or not a pending entry was still there to reconcile: an echo
 * arriving after the timeout rollback is exactly the case where the pending key
 * alone can no longer identify this client's own row.
 */
function recordEcho(
  echoed: ReadonlyMap<string, string>,
  event: WireEvent,
): ReadonlyMap<string, string> {
  if (event.kind !== "checkin.added" || event.payload.clientEventId === undefined) {
    return echoed;
  }
  const next = new Map(echoed);
  next.set(event.payload.clientEventId, event.payload.checkInId);
  return next;
}

/** Creates an isolated live-session store instance. */
/**
 * The way-in stamp that should still be in force after an authoritative reseed.
 *
 * The stamp survives `seedFromSnapshot` on purpose, so a mid-net resync does not
 * silently stop recording what the operator set. The cost of that: when the
 * owner edits the connection list mid-net, the
 * surviving stamp can name a connection the new snapshot no longer lists —
 * `sendableVia` then refuses the dangling id and EVERY subsequent check-in
 * records no way in, while the control still reports a stamp is set. The
 * stranded state is not worded better; it is made impossible.
 *
 * Narrower than "clear on reseed", deliberately. Free text names no connection
 * so no snapshot can strand it, and a still-listed connection is still a valid
 * answer; wiping either would take back the very thing the stamp's exclusion
 * from the reset exists to protect. This is also the ONLY place that sees both
 * the surviving stamp and the incoming connection set — doing it in a component
 * would be a render-time side effect that the quick-add's own picker races.
 */
function survivingViaStamp(
  stamp: ViaWire | null,
  connections: readonly NetConnection[],
): ViaWire | null {
  if (stamp === null || stamp.kind !== "connection") {
    return stamp;
  }
  return connections.some((c) => c.id === stamp.connectionId) ? stamp : null;
}

export function createSessionStore(): StoreApi<SessionStore> {
  return createStore<SessionStore>((set) => ({
    session: DEFAULT_SESSION_STATE,
    connection: "catching-up",
    meta: null,
    pending: new Map<string, PendingEntry>(),
    locks: new Map<string, LockEntry>(),
    echoed: new Map<string, string>(),
    // Seeded EMPTY and never derived from the snapshot: defaulting it to the
    // net's only connection (or its first, or its RF one) turns every check-in
    // nobody recorded a way in for into a confident wrong answer.
    viaStamp: null,

    seedFromSnapshot: (snapshotSummary) =>
      // A snapshot is an authoritative full reset (initial load, or the
      // post-1008 re-snapshot), not an incremental fold — any not-yet-echoed
      // optimistic entry from before this reset cannot be reconciled against
      // it and must not survive as an orphaned roster row. Locks are ephemeral
      // and NOT carried by the snapshot, so they reset here too.
      set((state) => {
        const seeded = stateFromSummary(snapshotSummary);
        return {
          session: seeded,
          meta: {
            id: snapshotSummary.id,
            definition: snapshotSummary.definition,
            // Present on the staff summary; absent on the redacted public view →
            // null (a public viewer holds no role).
            viewerRole:
              (snapshotSummary as Partial<SessionSummaryBody>).viewerRole ?? null,
          },
          pending: new Map<string, PendingEntry>(),
          locks: new Map<string, LockEntry>(),
          // `viaStamp` deliberately SURVIVES this reset — it is the operator's
          // own input default, not a fact the snapshot carries, and a mid-net
          // resync must not silently stop recording what they set. It is
          // dropped ONLY when this snapshot has taken the connection it names
          // away; see `survivingViaStamp`.
          viaStamp: survivingViaStamp(state.viaStamp, seeded.connections),
        };
      }),

    applyEvent: (event) =>
      set((state) => ({
        session: fold(state.session, event),
        pending: reconcilePending(state.pending, event),
        echoed: recordEcho(state.echoed, event),
      })),

    applyCatchUp: (events) =>
      set((state) => {
        let session = state.session;
        let pending = state.pending;
        let echoed = state.echoed;
        for (const event of events) {
          session = fold(session, event);
          pending = reconcilePending(pending, event);
          echoed = recordEcho(echoed, event);
        }
        return { session, pending, echoed };
      }),

    addPending: (
      clientEventId,
      callsign,
      signalReport = null,
      staying = "in-and-out",
      source = "staff",
      via = null,
      relayedBy = null,
    ) =>
      set((state) => {
        const next = new Map(state.pending);
        next.set(clientEventId, {
          clientEventId,
          callsign,
          signalReport,
          staying,
          source,
          via,
          relayedBy,
        });
        return { pending: next };
      }),

    removePending: (clientEventId) =>
      set((state) => {
        if (!state.pending.has(clientEventId)) {
          // Same-ref no-op: the entry already reconciled or was removed, so
          // there is nothing to roll back and no reason to re-notify.
          return { pending: state.pending };
        }
        const next = new Map(state.pending);
        next.delete(clientEventId);
        return { pending: next };
      }),

    applyLock: (delta) =>
      set((state) => {
        if (delta.holderCallsign === null || delta.expiresAt === null) {
          // Release: drop the entry. Same-ref no-op when it was never held.
          if (!state.locks.has(delta.checkInId)) {
            return { locks: state.locks };
          }
          const next = new Map(state.locks);
          next.delete(delta.checkInId);
          return { locks: next };
        }
        const next = new Map(state.locks);
        next.set(delta.checkInId, {
          holderCallsign: delta.holderCallsign,
          expiresAt: delta.expiresAt,
        });
        return { locks: next };
      }),

    setConnection: (connection) => set({ connection }),

    setViaStamp: (via) => set({ viaStamp: via }),
  }));
}

/**
 * True when a lock entry's own `expiresAt` (the TTL the server already
 * broadcast) has passed `nowMillis`. An unparseable/placeholder
 * timestamp (`Date.parse` → `NaN`) is treated as NOT expired rather than
 * silently dropped — this only ever fires on a real ISO timestamp from the
 * server.
 *
 * This is the client-side self-heal for the lock slice's known staleness
 * gap: locks are ephemeral/never event-sourced, so a resumed WS
 * connection (`since>0`, no snapshot) or a broadcast-channel `Lagged` drop can
 * leave a stale entry in the `locks` map with no future frame guaranteed to
 * ever correct it (a release frame lost in the same gap never arrives). Since
 * every `LockEntry` already carries the server's own expiry, honoring it
 * client-side bounds the worst-case staleness to the lease's own TTL instead
 * of "indefinitely".
 */
function isLockExpired(expiresAt: string, nowMillis: number): boolean {
  const parsed = Date.parse(expiresAt);
  return !Number.isNaN(parsed) && parsed <= nowMillis;
}

/**
 * The roster for render from its two stable inputs — the authoritative folded
 * roster followed by the originating client's own un-echoed optimistic entries.
 * Kept separate from `selectRoster` so React callers can memoize on the stable
 * `roster` / `pending` references (a Zustand selector must NOT return a fresh
 * array each call — that loops `useSyncExternalStore`).
 *
 * `nowMillis` (defaults to `Date.now()`) is the wall-clock instant used to
 * self-expire a stale `locks` entry (see `isLockExpired`) — callers that want
 * this to actually re-derive as time passes (rather than once per unrelated
 * re-render) should re-invoke this on a periodic tick, as `LiveSessionPage`
 * does.
 */
export function buildRoster(
  roster: readonly RosterEntry[],
  pending: ReadonlyMap<string, PendingEntry>,
  locks: ReadonlyMap<string, LockEntry> = new Map(),
  editingCheckInId: string | null = null,
  nowMillis: number = Date.now(),
  workingCheckInId: string | null = null,
  // REQUIRED, unlike the six before it. A `[]` default would be wrong, because
  // an empty connection set is not "no way in shown" — it resolves every
  // RECORDED `via` to `UNRESOLVABLE_VIA_LABEL`, so every row claims the net has
  // lost the way it came in on. All three production call sites already pass it;
  // a fourth that forgets should not compile.
  connections: readonly NetConnection[],
): DisplayRosterEntry[] {
  // Each row's `source` is read from the folded entry: a staff
  // `LogCheckIn` add is `staff`, a participant self-check-in is `self`. It drives
  // the source badge on BOTH the operator console and the account-less public
  // roster. The ephemeral lock slice merges in here: an
  // entry another
  // operator is editing renders read-only — EXCEPT the one THIS operator has
  // open in the modal (`editingCheckInId`), which is never flagged self-locked.
  const authoritative: DisplayRosterEntry[] = roster.map((entry) => {
    const lock = locks.get(entry.checkInId);
    const lockActive = lock !== undefined && !isLockExpired(lock.expiresAt, nowMillis);
    const lockedBy =
      lockActive && entry.checkInId !== editingCheckInId ? lock.holderCallsign : null;
    return {
      key: entry.checkInId,
      callsign: entry.callsign,
      optimistic: false,
      addedAt: entry.addedAt,
      addedBy: entry.addedBy,
      // Read the folded provenance (retiring the hard-coded
      // "staff") — a self-check-in row now advertises "self".
      source: entry.source,
      signalReport: entry.signalReport,
      staying: entry.staying,
      name: entry.name,
      location: entry.location,
      grid: entry.grid,
      precedence: entry.precedence,
      traffic: entry.traffic,
      notes: entry.notes,
      publicNote: entry.publicNote,
      via: entry.via,
      viaLabel: viaLabel(entry.via, connections),
      relayedBy: entry.relayedBy,
      // The working cursor is derived from the session's single cursor id; the
      // worked-dim flag comes from the fold. Working takes visual precedence.
      working: entry.checkInId === workingCheckInId,
      worked: entry.worked,
      version: entry.version,
      // A `via` correction's two sides are resolved HERE,
      // against the connection set as it stands right now, not at fold time. A
      // mid-session QSY re-labels a way in, and this memo re-runs on
      // `connections`, so the annotation moves with it. A correction that came
      // from the server snapshot carries no structured value and keeps the
      // strings the server already resolved.
      corrections: entry.corrections.map((correction) =>
        correction.fromVia === undefined && correction.toVia === undefined
          ? correction
          : {
              ...correction,
              from: viaLabel(correction.fromVia ?? null, connections),
              to: viaLabel(correction.toVia ?? null, connections),
            },
      ),
      lockedBy,
    };
  });
  const optimistic: DisplayRosterEntry[] = [...pending.values()].map((entry) => ({
    key: entry.clientEventId,
    callsign: entry.callsign,
    optimistic: true,
    addedAt: new Date(nowMillis).toISOString(),
    addedBy: null,
    source: entry.source,
    signalReport: entry.signalReport,
    staying: entry.staying,
    // An un-echoed optimistic row has no server identity yet — no name/location/
    // corrections, version 0, and it can never be locked by another operator.
    // Precedence/traffic are edit-only, so a fresh pending row is routine/none.
    name: null,
    location: null,
    grid: null,
    precedence: "routine",
    traffic: null,
    // Notes/worked/working are edit-or-cursor state a pending row cannot have yet.
    notes: null,
    publicNote: null,
    // The operator chooses the way in BEFORE the POST, so the dimmed row
    // carries their choice from the first paint; the echo still replaces the
    // whole row with the server's own answer. Resolved against the live
    // connection set exactly as an authoritative row is, so a mid-net QSY
    // re-labels a pending row too.
    via: entry.via,
    viaLabel: viaLabel(entry.via, connections),
    // The operator typed it before the POST, so the dimmed row
    // carries it from the first paint; the echo still replaces the whole row.
    relayedBy: entry.relayedBy,
    working: false,
    worked: false,
    version: 0,
    corrections: [],
    lockedBy: null,
  }));
  return [...authoritative, ...optimistic];
}

/**
 * The `checkInId` of the entry that is "next up" AND belongs to the viewer — the
 * pure selector driving the YourTurnIndicator. Returns `null`
 * unless the viewer's OWN row is the next station the NCS will work.
 *
 * `roster` is already in the shared server-authored DISPLAY order the fold's
 * `roster.reordered` deltas produced — precedence order, and under worked-sink
 * the partition too. What "next up" means depends on which ordering is in
 * force, which is why `orderMode` is REQUIRED: a call site
 * that forgets it must not compile, because a defaulted mode would silently
 * fall back to the wrong reading below.
 *
 * - **`manual`**: the first non-`worked` entry strictly AFTER the cursor. Under
 * manual ordering position IS the running order — a station above the cursor
 * was deliberately passed, and promoting it would start telling skipped
 * stations they are next.
 * - **`worked-sink`**: the first entry in display order that is not `worked`
 * and is not the cursor entry — the top of the unworked group, which may sit
 * ABOVE the cursor. `worked`
 * is monotonic, and from round 2 on the cursor entry is itself worked
 * but exempted from the sink, not promoted, so stations still awaiting their
 * turn can sit above it. The forward scan then either finds only worked rows
 * and returns `null` (no indicator), or names the wrong station — a viewer
 * below the cursor told "You're next up" while the NCS calls the one above.
 * Both are real; an earlier fix documented and covered only the first.
 * The cursor exemption matters in round 1, where the cursor entry is still
 * unworked and heads the roster: `!worked` alone would name the station being
 * worked RIGHT NOW.
 *
 * The viewer's own row is identified by callsign match (case-insensitive) —
 * this works on the redacted public roster, which carries `callsign` but not
 * `addedBy`. No server state, no event: a pure derivation over `roster` +
 * `workingCheckInId` + `orderMode` + the viewer's own callsign.
 *
 * Returns `null` when: the cursor is unset (nobody is being worked yet, so there
 * is no "next"); the viewer has no callsign; the cursor points off-roster; or the
 * computed next-up entry is not the viewer's own. The currently-worked entry is
 * never "next up" (it is being worked NOW) under either mode.
 */
export function selectYourTurnCheckInId(
  roster: readonly RosterEntry[],
  workingCheckInId: string | null,
  ownCallsign: string | null,
  orderMode: RosterOrderMode,
): string | null {
  if (workingCheckInId === null || ownCallsign === null) {
    return null;
  }
  const cursorIndex = roster.findIndex((entry) => entry.checkInId === workingCheckInId);
  if (cursorIndex === -1) {
    return null;
  }
  const nextUp =
    orderMode === "worked-sink"
      ? roster.find((entry) => !entry.worked && entry.checkInId !== workingCheckInId)
      : roster.slice(cursorIndex + 1).find((entry) => !entry.worked);
  if (nextUp === undefined) {
    return null;
  }
  return nextUp.callsign.toUpperCase() === ownCallsign.toUpperCase()
    ? nextUp.checkInId
    : null;
}

/** Whether a typed callsign is already on this session's roster. */
export type DuplicateCheckInState = "none" | "unworked" | "worked";

/**
 * Whether the callsign being typed is already on this session's roster, and if
 * so whether that station still awaits its turn — the pure derivation behind the
 * quick-add's non-blocking duplicate warning.
 *
 * `"unworked"` when ANY matching entry has not been worked, `"worked"` when
 * every match has, `"none"` when the callsign is blank or absent. The
 * unworked-wins rule is deliberate: with the same station on the roster twice,
 * the operationally relevant fact is the outstanding entry, and reporting the
 * worked one would tell the NCS a station is finished while another awaits.
 *
 * Local and synchronous by design: every fact it needs is already in the
 * browser and no endpoint carries this answer, so it never waits on the network.
 * The roster it reads is not uniformly authoritative, though — `selectRoster`
 * folds in this client's own un-echoed optimistic rows, which is exactly why
 * BOTH sides are normalized below rather than only the typed one.
 */
export function selectDuplicateCheckIn(
  roster: readonly DisplayRosterEntry[],
  callsign: string,
): DuplicateCheckInState {
  const typed = normalizedCallsignSegments(callsign);
  if (typed.length === 0) {
    return "none";
  }
  const matches = roster.filter((entry) =>
    sharesABaseSegment(typed, normalizedCallsignSegments(entry.callsign)),
  );
  if (matches.length === 0) {
    return "none";
  }
  return matches.some((entry) => !entry.worked) ? "unworked" : "worked";
}

/**
 * Whether two normalized callsigns name the same station: they share a segment,
 * and that shared segment is shaped like an assigned base call rather than a
 * designator.
 *
 * The base-shape test is what keeps the symmetry honest in both directions. A
 * bare intersection would match `K2ABC/P` against `W1AW/P` on the `/P` they
 * both carry, and a pending `DL` against `DL/N1CCK` — neither of which is the
 * same station.
 */
function sharesABaseSegment(a: readonly string[], b: readonly string[]): boolean {
  return a.some((segment) => isBaseShapedSegment(segment) && b.includes(segment));
}

/**
 * One callsign, trimmed, ASCII-uppercased and split into its `/`-segments —
 * applied to BOTH sides of the comparison.
 *
 * Normalizing the typed side alone was wrong. `parse_callsign` does strip every
 * portable/compound designator (`W1AW/P` and `DL/W1AW/P` both land as `W1AW`),
 * and the guarantee is stronger than "the write path enforces it": the event
 * DECODER re-parses through `parse_callsign` too
 * (`netroll-adapters/src/pg/session_events.rs:415`), so a folded roster entry is
 * a bare base at READ time, not merely at write time.
 *
 * But the roster this selector reads is `selectRoster`'s, which deliberately
 * includes this client's own optimistic pending rows — and those never touch
 * the server. `addPending` stores the operator's RAW typed text: arbitrary
 * case, designators intact, not necessarily even a valid callsign. Comparing a
 * normalized left side against an unnormalized right side missed a portable
 * station double-entered inside the echo window, including a byte-identical
 * repeat.
 *
 * Segments are compared individually rather than reduced to one base: which
 * segment `parse_callsign` PICKS, and its refusal to guess between two valid
 * bases, are not mirrored here. See `isBaseShapedSegment`.
 */
function normalizedCallsignSegments(callsign: string): readonly string[] {
  const upper = callsign.trim().toUpperCase();
  if (upper === "") {
    return [];
  }
  return upper.split("/").filter((segment) => segment !== "");
}

/**
 * Whether a segment is shaped like an assigned base call — the browser-side
 * mirror of the domain's `is_base`/`validate_base` (`callsign.rs:132-146`),
 * which is the only part of `parse_callsign` this file reproduces.
 *
 * Base grammar: prefix (1–3 alphanumerics, at least one letter) + one separator
 * digit + suffix (1–4 alphanumerics ending in a letter), 3–7 characters total.
 * Every designator form fails it — `P`, `MM`, `QRP`, `4`, and the prefixes `DL`
 * and `KP4` — which is the whole point.
 *
 * Deliberately NOT mirrored: `parse_callsign`'s base-PICKING and its
 * `AmbiguousBase` refusal. A typed `W1AW/K2ABC` (two valid bases, which the
 * server rejects outright) therefore still warns against a roster holding
 * `K2ABC`. That residual is bounded to one submit the server 400s anyway.
 */
function isBaseShapedSegment(segment: string): boolean {
  if (segment.length > 7) {
    return false;
  }
  // Every digit is tried as the separator, as `validate_base` does — `2E0ABC`
  // splits on its second digit, not its first.
  for (let i = 0; i < segment.length; i += 1) {
    if (!/[0-9]/.test(segment[i] ?? "")) {
      continue;
    }
    const prefix = segment.slice(0, i);
    const suffix = segment.slice(i + 1);
    const prefixOk = /^[A-Z0-9]{1,3}$/.test(prefix) && /[A-Z]/.test(prefix);
    if (prefixOk && /^[A-Z0-9]{0,3}[A-Z]$/.test(suffix)) {
      return true;
    }
  }
  return false;
}

/**
 * The roster for render from a full store state (the unit-test/non-React entry
 * point). A viewer with an empty pending map sees only the authoritative roster
 * Locks merge in from the ephemeral slice, self-expired against
 * `nowMillis` (see `isLockExpired`).
 *
 * Every row here is available synchronously, with no fetch — but they are not
 * all authoritative. The pending rows are this client's own un-echoed adds,
 * carrying raw operator input the server has not seen or normalized. Anything
 * deriving identity from this roster must normalize it (see
 * `selectDuplicateCheckIn`).
 */
export function selectRoster(
  state: SessionStore,
  nowMillis: number = Date.now(),
): DisplayRosterEntry[] {
  return buildRoster(
    state.session.roster,
    state.pending,
    state.locks,
    null,
    nowMillis,
    state.session.workingCheckInId,
    state.session.connections,
  );
}

/**
 * The connection status to RENDER, derived from the pure TRANSPORT state and the
 * folded control status. This is the FIRST producer of the
 * long-reserved `net-paused` render, which had no trigger until now:
 *
 * - My own TRANSPORT degradation dominates: if I'm not seeing live data
 * (`catching-up`/`out-of-sync`), that wins — a paused net I can't even see
 * live is moot, so my degraded transport is the honest signal.
 * - Else, a `stalled` control status surfaces as `net-paused` (the net's NCS
 * dropped; the roster is frozen), for ALL viewers including account-less.
 * - Else `live`.
 *
 * Pure over its two inputs — the socket controller's `connection` slice stays the
 * pure transport state (it sets `live` on every good frame), so `net-paused` is
 * DERIVED here, never stored, and can never be clobbered by the next live frame.
 */
export function displayedConnection(
  transport: ConnectionState,
  controlState: ControlState,
): ConnectionState {
  if (transport !== "live") {
    return transport;
  }
  return controlState === "stalled" ? "net-paused" : "live";
}

/**
 * The post-net summary's "States / provinces" stat: a count of distinct
 * states/provinces represented across the roster. There is no structured
 * per-check-in state field (only the net DEFINITION carries one, and it
 * describes the net, not each operator) — this is a best-effort parse of each
 * entry's free-text `location` ("Atlanta GA", "Ottawa, ON"), reading the
 * trailing two-letter token as the code. A `null` location, or one with no
 * such trailing token, contributes nothing.
 */
export function countDistinctStates(locations: readonly (string | null)[]): number {
  const codes = new Set<string>();
  for (const location of locations) {
    if (location === null) {
      continue;
    }
    const match = /\b([A-Za-z]{2})$/.exec(location.trim());
    if (match !== null) {
      codes.add(match[1].toUpperCase());
    }
  }
  return codes.size;
}
