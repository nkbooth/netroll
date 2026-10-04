// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
/**
 * Hand-authored TypeScript mirror of the shipped Rust serde wire shapes for the
 * live-session event spine. There is no codegen: every type here
 * matches the camelCase JSON the backend actually emits, byte-for-byte, so the
 * client folds exactly what the server sends.
 *
 * TWO DISTINCT optionality conventions coexist and MUST NOT be conflated:
 * - OMITTED-when-absent (`#[serde(skip_serializing_if = "Option::is_none")]`):
 * the key is not present at all → modeled as `?: T` (`undefined`). Applies to
 * `WireEvent.actorId`, `CheckinAddedPayload.clientEventId`, and every
 * optional field of `DefinitionSnapshot`.
 * - NULL-when-absent (plain `Option`, no skip): the key is present valued
 * `null` → modeled as `T | null`. Applies to `SessionSummaryBody.startedAt`
 * / `closedAt` / `durationSeconds` and `RosterEntryBody.addedBy`.
 *
 * All timestamps (`WireEvent.at`, the summary/roster timestamps) are RFC 3339
 * UTC strings on the wire, even though the domain stores epoch millis. The kind
 * (4) and lifecycle (3) tokens are additive-only server contracts.
 *
 * [protocol.rs:29-85; net_sessions.rs:76-105; session_events.rs:39-63;
 * pg/net_sessions.rs DefinitionSnapshot]
 */

import type { NetConnection } from "../nets/netsApi";
import type { ViaWire } from "../nets/connectionPresentation";

/** The session-event kind tokens the fold applies (`SessionEventBody::kind`). */
export type SessionEventKind =
  | "session.started"
  | "frequency.changed"
  | "checkin.added"
  | "checkin.updated"
  | "checkin.removed"
  | "roster.reordered"
  | "station.worked-set"
  | "session.note-set"
  | "roster.order-mode-set"
  | "session.closed"
  | "ncs.stalled"
  | "ncs.resumed"
  | "control.handed-off"
  | "station.blocked";

/** The three projected lifecycle tokens (`net_sessions.lifecycle`). */
export type SessionLifecycle = "scheduled" | "live" | "closed";

/**
 * The projected control-status tokens — the ORTHOGONAL
 * second axis on top of lifecycle. A `live` session is independently `active`
 * (its net control station is present) or `stalled` (that NCS's presence dropped;
 * the net is paused pending resume/claim/auto-close). The TS mirror of the Rust
 * `ControlState`; defaults to `active`.
 */
export type ControlState = "active" | "stalled";

/**
 * The standing roster ordering mode — the TS mirror of the Rust
 * `RosterOrderMode`. `manual` is the shipped behaviour (order changes only on an
 * explicit reorder command); `worked-sink` sinks worked stations below unworked
 * ones and keeps them there. Defaults to `manual` for every session that
 * predates the mode and for the redacted public view, which never carries it.
 */
export type RosterOrderMode = "manual" | "worked-sink";

/**
 * The viewer's own resolved per-net role on the STAFF summary —
 * the lowercase-kebab mirror of the domain `Role`. Server-computed via
 * `resolve_role`; the console reads it to gate role-aware affordances (UX
 * only). REDACTED from the public summary — an account-less viewer has no role.
 */
export type ViewerRole =
  | "owner"
  | "net-control"
  | "logger"
  | "relay"
  | "participant";

/**
 * The binary staying-status wire tokens — the TS mirror of
 * the Rust `StayingStatus` kebab tokens. Additive-only server contract.
 */
export type StayingStatus = "staying-for-comments" | "in-and-out";

/**
 * The three traffic/emergency precedence tokens — the TS
 * mirror of the Rust `Precedence` kebab tokens. Additive-only server contract;
 * defaults to `routine`.
 */
export type Precedence = "routine" | "priority" | "emergency";

/**
 * The check-in provenance tokens — the TS mirror of the
 * Rust `CheckInSource` kebab tokens. `staff` is an operator-logged entry;
 * `self` is a participant self-check-in. Additive-only server contract; a
 * historical/omitted value folds to `staff`.
 */
export type CheckInSource = "staff" | "self";

/**
 * `session.started` payload — the definition provenance.
 *
 * There is no session-level start frequency: a net has a SET of ways
 * to reach it, each with its own frequency or none, and the frozen
 * `DefinitionSnapshot.connections` carries them. The PUBLIC projection of this
 * event is `{}` — the two ids here are internal provenance and never cross the
 * public wire.
 */
export interface SessionStartedPayload {
  readonly definitionId: string;
  readonly definitionVersion: number;
}

/**
 * `frequency.changed` payload — WHICH connection moved, and to what.
 *
 * `connectionId` is an id from the session's own frozen connection set. Without
 * it a net with three ways to reach it could not say which of
 * them the operator retuned.
 */
export interface FrequencyChangedPayload {
  readonly connectionId: string;
  readonly operatingFrequencyHz: number;
}

/**
 * `checkin.added` payload. `clientEventId` is OMITTED when absent (never
 * `null`) — it is the optimistic-write echo id the store reconciles on,
 * never a fold input.
 */
export interface CheckinAddedPayload {
  readonly checkInId: string;
  readonly callsign: string;
  readonly clientEventId?: string;
  /**
   * The mode-shaped signal report. OMITTED when absent (the
   * `skip_serializing_if` wire rule, like `clientEventId`) → `?: string`.
   */
  readonly signalReport?: string;
  /**
   * The staying status. The server always serializes it on a
   * `checkin.added` payload (legacy events default to `in-and-out` server-side
   * before serialization), but it is modeled optional so an older delta or a
   * malformed frame folds to the default rather than `undefined` (additive-compat).
   */
  readonly staying?: StayingStatus;
  /**
   * The prefilled operator name captured at check-in. OMITTED
   * when absent (the `skip_serializing_if` wire rule, like `signalReport`) →
   * `?: string`. A committed prefill (from the per-net roster memory) persists
   * here; an older or callsign-only add carries no key and folds to `null`.
   */
  readonly name?: string;
  /** The prefilled free-text location captured at check-in, mirroring `name`. */
  readonly location?: string;
  /**
   * The Maidenhead grid captured at check-in. A SEPARATE
   * field from `location` — one is a place name, one is a locator. OMITTED when
   * absent (the `skip_serializing_if` wire rule) → `?: string`; an older event
   * or a redacted public delta carries no key and folds to `null`.
   */
  readonly grid?: string;
  /**
   * Who created this entry — `staff`/`self`. Public radio
   * data: it rides BOTH the owner and the REDACTED public `checkin.added` delta
   * (`addedBy` stays redacted). OMITTED on an older event → folds to `staff`.
   */
  readonly source?: CheckInSource;
  /**
   * WHICH way in this station arrived on. OMITTED when
   * absent (the `skip_serializing_if` wire rule) → `?: ViaWire`; absent means
   * *nobody recorded it*, which is never the same as *the net's first
   * connection*.
   *
   * STRUCTURED, not a label: the browser holds `state.connections` and resolves
   * the label itself, so the wire never carries a second, staler answer to what
   * a connection is called. It rides the PUBLIC add delta too — see
   * `ws/protocol.rs`'s add arm for why this one field is set at add where the
   * four other widened fields are edit-only.
   */
  readonly via?: ViaWire;
  /**
   * WHICH STATION passed this check-in's traffic. OMITTED
   * when nobody relayed it → `?: string`; absent means *not relayed*.
   *
   * A DISTINCT fact from `via` beside it — one records how the traffic
   * travelled, the other who passed it — and a plain callsign rather than a
   * structured object, because there is only one kind of answer and a callsign
   * is already the text a person reads.
   *
   * **It does NOT ride the public delta.** Unlike `via`, which names a
   * connection the net owner published and this page already carries, this names
   * a third-party station that never checked in, so the public arm `_`-binds it.
   * A public frame therefore never carries the key at all.
   */
  readonly relayedBy?: string;
}

/**
 * `checkin.updated` payload — the FULL post-edit editable field
 * set (last-write-wins REPLACE). `name`/`location`/`signalReport` are OMITTED
 * when absent; `staying` is always present. Corrections are NEVER on the wire —
 * the fold derives them, matching the backend.
 */
export interface CheckinUpdatedPayload {
  readonly checkInId: string;
  readonly callsign: string;
  readonly name?: string;
  readonly location?: string;
  /** The post-edit Maidenhead grid; OMITTED when absent → `?: string`. */
  readonly grid?: string;
  readonly signalReport?: string;
  readonly staying?: StayingStatus;
  /** The traffic/emergency precedence — always present; `routine`
   * by default. */
  readonly precedence: Precedence;
  /** The optional traffic count; OMITTED when absent → `?: number`. */
  readonly traffic?: number;
  /** The per-station STAFF note; OMITTED when absent → `?: string`.
   * REPLACE semantics on the fold — derives NO correction (running commentary).
   * Operator-only: it never crosses the public delta. */
  readonly notes?: string;
  /** The per-station PUBLIC note; OMITTED when absent. Same REPLACE
   * semantics and same no-correction rule as `notes` — but this one DOES cross
   * the public delta, so an observer sees it change mid-net. */
  readonly publicNote?: string;
  /** The post-edit way in; OMITTED when absent. Rides the public
   * delta too, so a corrected `via` reaches an observer live. */
  readonly via?: ViaWire;
  /** The post-edit relaying station; OMITTED when absent. Owner
   * delta only — the public arm `_`-binds it. */
  readonly relayedBy?: string;
}

/** `checkin.removed` payload — a bare tombstone. */
export interface CheckinRemovedPayload {
  readonly checkInId: string;
}

/**
 * `station.worked-set` payload — the single working-cursor
 * target, or `null` to clear it. `checkInId` is PRESENT valued `null` on a clear
 * (NOT omitted). This is public radio data: the SAME shape crosses the
 * public wire (the redacted arm keeps only this field).
 */
export interface StationWorkedSetPayload {
  readonly checkInId: string | null;
}

/**
 * `session.note-set` payload — the net-level note. `note` is
 * OMITTED when cleared (omit-optional → `?: string`). The note TEXT is
 * operator-only: the PUBLIC wire's payload is `{}` (this field never appears).
 */
export interface SessionNoteSetPayload {
  readonly note?: string;
}

/**
 * `roster.reordered` payload — the EXPLICIT ordered list of
 * `checkInId`s (a permutation) the reducer applies as a dumb projection. Not a
 * sort-strategy token; list position is public radio data, so the public
 * wire carries this same shape.
 */
export interface RosterReorderedPayload {
  readonly order: readonly string[];
}

/**
 * `roster.order-mode-set` payload — the standing ordering mode.
 * Carried on BOTH the owner and the public wire: the public page's your-turn
 * selector reads the mode, so the delta must carry it or the next toggle
 * reverts the page to the default. Still modeled OPTIONAL: a TS type is an assertion about untrusted
 * JSON and catches nothing at runtime — the fence that the key is present is the
 * backend's own projection test. An absent or unrecognized value folds to
 * `manual`.
 */
export interface RosterOrderModeSetPayload {
  readonly mode?: RosterOrderMode;
}

/** The event envelope fields shared by every kind. `actorId` is OMITTED when absent. */
interface WireEventEnvelope {
  readonly seq: number;
  readonly actorId?: string;
  readonly at: string;
}

/** A `session.started` delta. */
export interface SessionStartedEvent extends WireEventEnvelope {
  readonly kind: "session.started";
  readonly payload: SessionStartedPayload;
}

/** A `frequency.changed` delta. */
export interface FrequencyChangedEvent extends WireEventEnvelope {
  readonly kind: "frequency.changed";
  readonly payload: FrequencyChangedPayload;
}

/** A `checkin.added` delta. */
export interface CheckinAddedEvent extends WireEventEnvelope {
  readonly kind: "checkin.added";
  readonly payload: CheckinAddedPayload;
}

/** A `checkin.updated` delta. */
export interface CheckinUpdatedEvent extends WireEventEnvelope {
  readonly kind: "checkin.updated";
  readonly payload: CheckinUpdatedPayload;
}

/** A `checkin.removed` delta. */
export interface CheckinRemovedEvent extends WireEventEnvelope {
  readonly kind: "checkin.removed";
  readonly payload: CheckinRemovedPayload;
}

/** A `roster.reordered` delta. */
export interface RosterReorderedEvent extends WireEventEnvelope {
  readonly kind: "roster.reordered";
  readonly payload: RosterReorderedPayload;
}

/** A `station.worked-set` delta. */
export interface StationWorkedSetEvent extends WireEventEnvelope {
  readonly kind: "station.worked-set";
  readonly payload: StationWorkedSetPayload;
}

/** A `session.note-set` delta. */
export interface SessionNoteSetEvent extends WireEventEnvelope {
  readonly kind: "session.note-set";
  readonly payload: SessionNoteSetPayload;
}

/** A `roster.order-mode-set` delta. */
export interface RosterOrderModeSetEvent extends WireEventEnvelope {
  readonly kind: "roster.order-mode-set";
  readonly payload: RosterOrderModeSetPayload;
}

/** A `session.closed` delta — an empty payload object on the wire. */
export interface SessionClosedEvent extends WireEventEnvelope {
  readonly kind: "session.closed";
  readonly payload: Record<string, never>;
}

/**
 * `control.handed-off` payload — the OWNER wire carries the
 * new active NCS's account id; the public wire redacts it to `{}` (an operator
 * id never crosses the public surface), so the field is OPTIONAL here (a
 * public viewer's payload has no key). The fold folds `controlState → active`
 * from the KIND regardless.
 */
export interface ControlHandedOffPayload {
  readonly newNcsAccountId?: string;
}

/** `ncs.stalled` event — a payload-free control transition (the fact IS the
 * transition; the fold derives `controlState = stalled` from the kind). */
export interface NcsStalledEvent extends WireEventEnvelope {
  readonly kind: "ncs.stalled";
  readonly payload: Record<string, never>;
}

/** `ncs.resumed` event — payload-free; the fold derives `controlState = active`. */
export interface NcsResumedEvent extends WireEventEnvelope {
  readonly kind: "ncs.resumed";
  readonly payload: Record<string, never>;
}

/** `control.handed-off` event — both the voluntary-handoff and involuntary-claim
 * paths mint this one kind. */
export interface ControlHandedOffEvent extends WireEventEnvelope {
  readonly kind: "control.handed-off";
  readonly payload: ControlHandedOffPayload;
}

/**
 * `station.blocked` payload — the OWNER wire carries the
 * blocked account id; the public wire REDACTS it to `{}` (a participant/operator
 * id never crosses the public surface), so the field is OPTIONAL here (a
 * public viewer's payload has no key). The block is enforced SERVER-SIDE (the
 * self-check-in 403); its roster effect rides the accompanying `checkin.removed`.
 */
export interface StationBlockedPayload {
  readonly accountId?: string;
}

/** `station.blocked` event — the NCS blocked a disruptive account for the session.
 * The fold tolerates it as an advancing no-op (the client needs no
 * blocklist state; the block is enforced server-side). */
export interface StationBlockedEvent extends WireEventEnvelope {
  readonly kind: "station.blocked";
  readonly payload: StationBlockedPayload;
}

/**
 * One appended event delta — the SHARED wire element serialized identically by
 * the WS `event` frame and the HTTP catch-up array. A discriminated union
 * on `kind`, so narrowing on `kind` exposes the matching payload.
 */
export type WireEvent =
  | SessionStartedEvent
  | FrequencyChangedEvent
  | CheckinAddedEvent
  | CheckinUpdatedEvent
  | CheckinRemovedEvent
  | RosterReorderedEvent
  | StationWorkedSetEvent
  | SessionNoteSetEvent
  | RosterOrderModeSetEvent
  | SessionClosedEvent
  | NcsStalledEvent
  | NcsResumedEvent
  | ControlHandedOffEvent
  | StationBlockedEvent;

/**
 * The wire form of a derived field correction — the amber
 * annotation source carried on the OWNER summary roster (redacted out of the
 * public view). `field` is the lowercase-kebab token; `from`/`to` are display
 * strings (`null` when the field was absent on that side); `at` is RFC 3339.
 */
export interface CorrectionWire {
  readonly field:
    | "callsign"
    | "name"
    | "location"
    | "grid"
    | "signal-report"
    | "staying"
    | "precedence"
    | "traffic";
  readonly from: string | null;
  readonly to: string | null;
  readonly at: string;
}

/**
 * One roster station on the wire, projected from the folded log. `addedBy` is
 * NULL when absent (a present key valued `null`), NOT omitted.
 */
export interface RosterEntryBody {
  readonly checkInId: string;
  readonly callsign: string;
  readonly addedAt: string;
  readonly addedBy: string | null;
  /** Who created this entry — `staff`/`self`. Always projected
   * by the fold (defaults `staff`), so a required wire field on the owner body. */
  readonly source: CheckInSource;
  /** The mode-shaped signal report; OMITTED when absent → `?: string`. */
  readonly signalReport?: string;
  /** The staying status — always projected by the fold, required here. */
  readonly staying: StayingStatus;
  /** The operator name (edit-only); OMITTED when absent. */
  readonly name?: string;
  /** The free-text location (edit-only); OMITTED when absent. */
  readonly location?: string;
  /** The Maidenhead grid; OMITTED when absent. Staff-console only —
   * the redacted public roster entry carries neither this nor `location`. */
  readonly grid?: string;
  /** The traffic/emergency precedence — always projected by the
   * fold, required here. */
  readonly precedence: Precedence;
  /** The optional traffic count; OMITTED when absent. */
  readonly traffic?: number;
  /** The per-station STAFF note (edit-only); OMITTED when absent.
   * Operator-only — it does not ride `PublicRosterEntry`. */
  readonly notes?: string;
  /** The per-station PUBLIC note (edit-only); OMITTED when absent. */
  readonly publicNote?: string;
  /** WHICH way in this station arrived on; OMITTED when absent.
   * Structured — the console resolves its own label against `connections`. */
  readonly via?: ViaWire;
  /** WHICH STATION passed this check-in's traffic; OMITTED when
   * absent. Owner-console only — `PublicRosterEntry` deliberately has no
   * counterpart, and adding one there would publish a third-party callsign on an
   * unauthenticated page. */
  readonly relayedBy?: string;
  /** Whether the working cursor has left this entry — always
   * projected by the fold (defaults `false`), so a required wire field. */
  readonly worked: boolean;
  /** The fold-derived CAS version the modal sends back as `expectedVersion`. */
  readonly version: number;
  /** The derived per-field corrections for the amber annotations. */
  readonly corrections: readonly CorrectionWire[];
}

/**
 * The frozen definition snapshot captured at session start. Every optional
 * field is OMITTED when absent (`skip_serializing_if`) → `?: T`, matching the
 * shipped `DefinitionSnapshot` serde.
 */
export interface DefinitionSnapshot {
  readonly title: string;
  readonly description?: string;
  /**
   * Every way to reach the net, frozen BY VALUE at start.
   *
   * Required — a snapshot without it does not decode server-side and the read
   * is refused with `/errors/unreplayable-log`, so this is never absent on a
   * body that arrived at all. The flat `plannedFrequencyHz`/`band`/`mode` that
   * used to sit here MOVED into `connections[0]`, and the six residual
   * definition-copied optionals (offset, tone, node, reflector) retired with
   * A historical snapshot may still carry them, and a reader
   * ignores keys it does not model.
   */
  readonly connections: readonly NetConnection[];
  readonly netCategory: string;
  readonly netType: string;
  readonly country?: string;
  readonly state?: string;
  readonly grid?: string;
}

/**
 * The folded session summary — the body of `GET /api/net-sessions/{id}`, start
 * (201), close (200), and the WS `snapshot` frame's `session`. `startedAt`,
 * `closedAt`, and `durationSeconds` are NULL when absent (present keys valued
 * `null`), NOT omitted. `latestSeq` is the resume cursor.
 */
export interface SessionSummaryBody {
  readonly id: string;
  readonly definitionId: string;
  readonly definitionVersion: number;
  readonly lifecycle: SessionLifecycle;
  /**
   * Every way this session is reachable RIGHT NOW: the frozen
   * snapshot's set with each `frequency.changed` overlaid onto the connection it
   * named.
   *
   * Beside `definition` rather than instead of it, exactly as
   * `operatingFrequencyHz` used to sit beside `definition.plannedFrequencyHz`:
   * one is what the net is doing now, the other what it was set up to do.
   */
  readonly connections: readonly NetConnection[];
  readonly startedAt: string | null;
  readonly closedAt: string | null;
  readonly durationSeconds: number | null;
  readonly latestSeq: number;
  readonly participantCount: number;
  /** The single working-station cursor, or `null`. NULL when absent
   * (present key valued null), NOT omitted. */
  readonly workingCheckInId: string | null;
  /** The net-level note, or `null`. Operator-only — never on the
   * public view. */
  readonly netNote: string | null;
  /**
   * The standing roster ordering mode. On BOTH the owner summary
   * and the public view — the public page's your-turn selector
   * needs it, though the public page still renders neither the toggle nor the
   * collapse and follows the shared ORDER automatically. Modeled optional
   * because a TS type catches nothing at runtime; the backend asserts the key is
   * present on both bodies. An absent value seeds the legacy `manual`.
   */
  readonly rosterOrderMode?: RosterOrderMode;
  /**
   * The viewer's OWN resolved role — server-computed via
   * `resolve_role`. Present on every STAFF summary (the console gates role-aware
   * affordances from it, UX only); ABSENT from the redacted `PublicSessionSummary`.
   * Modeled optional so the shared `seedFromSnapshot` accepts both shapes; the
   * store defaults an absent value to `null` (a public viewer holds no role).
   */
  readonly viewerRole?: ViewerRole;
  /**
   * The projected control status: `active`/`stalled`. Public
   * radio data — a paused net is visibly frozen — so it rides BOTH this owner
   * summary and the public view. The client derives the `net-paused` connection
   * render from it (`displayedConnection`).
   */
  readonly controlState: ControlState;
  /**
   * The single active NCS's account id, or `null`. Operator id —
   * OWNER summary ONLY; REDACTED from `PublicSessionSummary`. Modeled
   * optional so the shared seed accepts both shapes (public → `null`).
   */
  readonly activeNcsAccountId?: string | null;
  readonly roster: readonly RosterEntryBody[];
  readonly definition: DefinitionSnapshot;
}

/**
 * One roster station on the PUBLIC (account-less) wire — the REDACTED projection
 * of `RosterEntryBody`. It OMITS `addedBy` entirely: the
 * operator account id never crosses a public surface.
 *
 * `staying`, `precedence`, `traffic` and `publicNote` cross too, so an
 * observer can follow a net without being an operator.
 *
 * Every other roster-entry field did NOT — `addedBy` first among them, then the
 * staff-only and fold-internal fields. The authority on WHICH is the Rust
 * `build_public_view`'s `_`-bound destructure arms, and how many there are is
 * asserted by `roster_projection_sites.rs`'s register, not restated here: no
 * gate can check a TypeScript comment against a Rust destructure, so this
 * comment carries no number to go stale. Three SESSION-level keys
 * are likewise absent from the public view — `definitionId`,
 * `definitionVersion` and the net-level note — but they belong to
 * `PublicSessionView`, not to this entry, and are kept separate so the two
 * sets never merge into one unattributed number.
 *
 * The default answer for a NEW field here is still no.
 */
export interface PublicRosterEntry {
  readonly checkInId: string;
  readonly callsign: string;
  readonly addedAt: string;
  /** Who created this entry — `staff`/`self`. Public PROVENANCE
   * (not PII, like `workingCheckInId`) driving the Self badge on the account-less
   * roster; `addedBy` stays redacted. Always projected by the fold. */
  readonly source: CheckInSource;
  /** The staying status — always projected, like the owner body. */
  readonly staying: StayingStatus;
  /** The traffic/emergency precedence — always projected. */
  readonly precedence: Precedence;
  /** The declared traffic count; OMITTED when absent. */
  readonly traffic?: number;
  /** The per-station PUBLIC note; OMITTED when absent. The STAFF
   * note is deliberately not here. */
  readonly publicNote?: string;
  /** WHICH way in this station arrived on — another field carried on this DTO,
   * joining the ones named above. The connection id is a
   * snapshot-local identifier this same view already publishes in
   * `connections`, and it is neither an account nor an operator identity; the
   * roster's `via` is the visible payoff of the epic on the public page.
   * OMITTED when absent. */
  readonly via?: ViaWire;
}

/**
 * The REDACTED public session view — the body of the account-less
 * `GET /api/net-sessions/{id}/live` and the public WS `snapshot` frame.
 * Distinct from `SessionSummaryBody`: it OMITS `definitionId` /
 * `definitionVersion` (internal net ids — an Unlisted net's id is itself the
 * access capability) and each roster entry omits `addedBy`. The `definition`
 * snapshot (title/band/mode/geography) is KEPT — already-public net data.
 */
export interface PublicSessionSummary {
  readonly id: string;
  readonly lifecycle: SessionLifecycle;
  /** Every way this session is reachable right now — see
   * `SessionSummaryBody.connections`. Public radio data: how to get on a net is
   * exactly what an account-less viewer came for. */
  readonly connections: readonly NetConnection[];
  readonly startedAt: string | null;
  readonly closedAt: string | null;
  readonly durationSeconds: number | null;
  readonly latestSeq: number;
  readonly participantCount: number;
  /** The single working-station cursor, or `null`. This IS public
   * radio data — unlike `netNote`, which never crosses the public wire.
   * The public RENDER of this cursor came later. */
  readonly workingCheckInId: string | null;
  /** The projected control status: `active`/`stalled`. IS public
   * radio data (a paused net is visibly frozen) — so it crosses the public wire,
   * unlike `activeNcsAccountId` (the operator id, redacted). All viewers derive
   * `net-paused` from this. */
  readonly controlState: ControlState;
  readonly roster: readonly PublicRosterEntry[];
  readonly definition: DefinitionSnapshot;
}

/** The WS `snapshot` frame — the folded summary, sent once on a fresh connect. */
export interface SnapshotMessage {
  readonly type: "snapshot";
  readonly session: SessionSummaryBody;
}

/** The WS `event` frame — a `WireEvent` flattened under a `type` discriminator. */
export type EventMessage = { readonly type: "event" } & WireEvent;

/**
 * The WS `lock` frame — a NON-event advisory soft-lock signal.
 * It carries NO `seq`, is NEVER folded into `SessionState`, and updates the
 * store's separate ephemeral `locks` slice. `holderCallsign`/`expiresAt` are
 * `null` on release (the entry is now free). Owner console only — the public WS
 * never emits it.
 */
export interface LockMessage {
  readonly type: "lock";
  readonly checkInId: string;
  readonly holderCallsign: string | null;
  readonly expiresAt: string | null;
}

/** A server-to-client WebSocket frame, discriminated by `type`. */
export type ServerMessage = SnapshotMessage | EventMessage | LockMessage;
