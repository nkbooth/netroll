// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
/**
 * Plain-`fetch` HTTP client for the net-session REST surface. Same-origin
 * cookies carry the owner session; failures arrive as RFC 9457 problem+json and
 * surface as `ProblemError` via the shared `throwProblem` (no bespoke error
 * handling, no apiClient class — the house convention, one `*Api.ts` per
 * feature). The WS stream is handled separately in `sessionSocket.ts`.
 */

import { throwProblem } from "../auth/authApi";
import type { ViaWire } from "../nets/connectionPresentation";
import type {
  Precedence,
  PublicSessionSummary,
  RosterOrderMode,
  SessionSummaryBody,
  StayingStatus,
  ViewerRole,
  WireEvent,
} from "./sessionWire";

const JSON_HEADERS = { "content-type": "application/json" };

/** The two synchronous export representations (`?format=`). */
export type ExportFormat = "csv" | "adif";

/**
 * Builds the same-origin export download URL (`GET /api/net-sessions/{id}/export
 * ?format=`). A plain `<a download href>` navigation to this URL
 * carries the session cookie automatically, so there is no `fetch`/Blob dance —
 * the server streams the CSV/ADIF file with an `attachment` disposition. The
 * server still enforces NCS/owner authorization; this only composes the
 * link the closed-session UI points at.
 */
export function exportUrl(id: string, format: ExportFormat): string {
  return `/api/net-sessions/${id}/export?format=${format}`;
}

/**
 * The start-a-session request body (`POST /api/net-sessions`).
 *
 * There is no `operatingFrequency`: an internet-only net has no
 * frequency to give, and the session freezes the definition's whole connection
 * set instead. Retuning one of them mid-run is `changeFrequency` below.
 */
export interface StartSessionRequest {
  readonly definitionId: string;
}

/**
 * Fetches the folded session summary (`GET /api/net-sessions/{id}`). A 401 means
 * signed-out and resolves to `null` (mirroring `fetchCurrentAccount`); any other
 * failure throws `ProblemError`.
 */
export async function getSession(id: string): Promise<SessionSummaryBody | null> {
  const response = await fetch(`/api/net-sessions/${id}`, {
    credentials: "same-origin",
  });
  if (response.status === 401) {
    return null;
  }
  if (!response.ok) {
    await throwProblem(response);
  }
  return (await response.json()) as SessionSummaryBody;
}

/**
 * Fetches the catch-up event gap (`GET /api/net-sessions/{id}/events?since=`) —
 * the stateless HTTP resume seam. Returns the ordered `WireEvent[]`
 * (empty when caught up); throws `ProblemError` on failure.
 */
export async function getEventsSince(id: string, since: number): Promise<WireEvent[]> {
  const response = await fetch(`/api/net-sessions/${id}/events?since=${since}`, {
    credentials: "same-origin",
  });
  if (!response.ok) {
    await throwProblem(response);
  }
  return (await response.json()) as WireEvent[];
}

/** Starts a session (`POST /api/net-sessions`) and returns the folded summary. */
export async function startSession(
  body: StartSessionRequest,
): Promise<SessionSummaryBody> {
  const response = await fetch("/api/net-sessions", {
    method: "POST",
    credentials: "same-origin",
    headers: JSON_HEADERS,
    body: JSON.stringify(body),
  });
  if (!response.ok) {
    await throwProblem(response);
  }
  return (await response.json()) as SessionSummaryBody;
}

/**
 * Retunes ONE of a live session's connections
 * (`POST /api/net-sessions/{id}/frequency`) and returns the folded summary.
 * `connectionId` names which way to reach the net moved — a net with three of
 * them has three frequencies. The
 * authoritative `frequency.changed` event also streams over the WS and folds the
 * pill last-write-wins; callers let that delta drive the display rather than
 * hand-setting store state. Throws `ProblemError` on failure (e.g. a 409 if the
 * session closed mid-edit).
 */
export async function changeFrequency(
  id: string,
  connectionId: string,
  operatingFrequency: string,
): Promise<SessionSummaryBody> {
  const response = await fetch(`/api/net-sessions/${id}/frequency`, {
    method: "POST",
    credentials: "same-origin",
    headers: JSON_HEADERS,
    body: JSON.stringify({ connectionId, operatingFrequency }),
  });
  if (!response.ok) {
    await throwProblem(response);
  }
  return (await response.json()) as SessionSummaryBody;
}

/** The add-a-check-in request options (an options object so the
 * growing field set stays self-documenting). Every field but `callsign` is
 * optional; `undefined` fields are dropped by `JSON.stringify`, so a
 * callsign-only add still POSTs `{ callsign }` byte-for-byte. */
export interface AddCheckInOptions {
  readonly callsign: string;
  /** The optimistic-write echo id the store reconciles on. */
  readonly clientEventId?: string;
  /** The mode-shaped signal report — staff-only server-side. */
  readonly signalReport?: string;
  /** The staying status; omitted → server defaults in-and-out. */
  readonly staying?: StayingStatus;
  /** The prefilled (or operator-typed) operator name; omitted
   * → the server stores no name. Dropped from the body by `JSON.stringify` when
   * `undefined`, so a callsign-only add is byte-identical to before. */
  readonly name?: string;
  /** The prefilled free-text location, mirroring `name`. */
  readonly location?: string;
  /** The Maidenhead grid; omitted → the server stores none.
   * A blank field must be sent as `undefined`, not `""`, so a callsign-only add
   * stays byte-identical to before this story. */
  readonly grid?: string;
  /** WHICH way in this station arrived on — the operator's
   * session stamp, or the per-station override. Structured, matching the key
   * {@link EditCheckInOptions.via} already carries: a `connection` id must be
   * one of THIS session's frozen snapshot or the server answers 404
   * `/errors/net-connection-not-found`, and free text is bounded (64 chars,
   * single line, blank is a 400 `/errors/via-invalid` — NOT "absent").
   *
   * There is no `null` on this path: the add contract has nothing to clear, so
   * "nobody recorded a way in" is the key being OMITTED, and a caller with an
   * empty free-text box must pass `undefined` rather than a value the server
   * will refuse — a rejected `via` fails the whole POST and loses the station. */
  readonly via?: ViaWire;
  /** WHICH STATION passed this station's traffic — a
   * callsign, validated server-side by the same `parse_callsign` the check-in's
   * own callsign uses and normalized to upper case on the way in.
   *
   * As with `via` there is no `null` on this path: "not relayed" is the key
   * being OMITTED. A caller whose box holds something the server would refuse
   * must pass `undefined` — this field rides the SAME POST as the callsign, so a
   * 400 on it loses the station too. */
  readonly relayedBy?: string;
}

/**
 * Adds a check-in to a live session (`POST /api/net-sessions/{id}/check-ins`)
 * and returns the folded summary. `clientEventId` is the optimistic
 * echo id the server threads into `checkin.added` so the store's
 * `reconcilePending` drops the matching pending row on echo. `signalReport` and
 * `staying` are additive fields captured at check-in. Throws
 * `ProblemError` on failure (a 400 callsign/report/staying-invalid, a 403 if a
 * non-staff actor set a report, or a 409 if the session closed mid-add).
 */
export async function addCheckIn(
  id: string,
  options: AddCheckInOptions,
): Promise<SessionSummaryBody> {
  const {
    callsign,
    clientEventId,
    signalReport,
    staying,
    name,
    location,
    grid,
    via,
    relayedBy,
  } = options;
  const response = await fetch(`/api/net-sessions/${id}/check-ins`, {
    method: "POST",
    credentials: "same-origin",
    headers: JSON_HEADERS,
    // `undefined` fields are dropped by JSON.stringify, so a callsign-only add
    // still POSTs `{ callsign }` byte-for-byte.
    body: JSON.stringify({
      callsign,
      clientEventId,
      signalReport,
      staying,
      name,
      location,
      grid,
      via,
      relayedBy,
    }),
  });
  if (!response.ok) {
    await throwProblem(response);
  }
  return (await response.json()) as SessionSummaryBody;
}

/**
 * The remembered station identity returned by the roster-memory lookup —
 * name/location only (never operational fields). Both nullable: a miss
 * is `{ name: null, location: null }`.
 */
export interface RosterMemory {
  readonly name: string | null;
  readonly location: string | null;
}

/**
 * Looks up the per-net roster memory for a callsign
 * (`GET /api/net-sessions/{id}/roster-memory?callsign=`) — the LOCAL
 * prefill the quick-add fires on callsign blur. BEST-EFFORT and NON-BLOCKING:
 * any failure (a miss, a 4xx/5xx, or a network error) resolves to `null` rather
 * than throwing, so a lookup can never block the callsign field or the commit
 * This is the SAME seam QRZ autofill layers onto — the
 * caller seeds any empty Name/Location field from the result and never clobbers
 * operator-typed text. A `null` result (or null fields) is simply not applied.
 */
export async function lookupRosterMemory(
  id: string,
  callsign: string,
): Promise<RosterMemory | null> {
  try {
    const response = await fetch(
      `/api/net-sessions/${id}/roster-memory?callsign=${encodeURIComponent(callsign)}`,
      { credentials: "same-origin" },
    );
    if (!response.ok) {
      return null;
    }
    return (await response.json()) as RosterMemory;
  } catch {
    // A network failure is a silent no-op — manual entry is never blocked.
    return null;
  }
}

/**
 * Looks up the MERGED best-effort check-in autofill for a callsign
 * (`GET /api/net-sessions/{id}/check-in-autofill?callsign=`) — the
 * server merges all three prefill sources (the callsign owner's NetRoll
 * profile, this net's roster-memory, and the external callbook) per-field and
 * returns the same editable `{ name, location }`
 * shape as {@link lookupRosterMemory}. This is the quick-add's default prefill
 * source (it SUPERSEDES the direct roster-memory call): merging server-side
 * keeps the acting operator's QRZ credentials off the client and owns the
 * precedence rule in one place. BEST-EFFORT and NON-BLOCKING, identical to
 * {@link lookupRosterMemory}: any failure (a miss, a 4xx/5xx, or a network
 * error) resolves to `null` rather than throwing, so a lookup can never block
 * the callsign field or the commit. The caller seeds only empty Name/Location
 * fields and never clobbers operator-typed text.
 */
export async function lookupCheckInAutofill(
  id: string,
  callsign: string,
): Promise<RosterMemory | null> {
  try {
    const response = await fetch(
      `/api/net-sessions/${id}/check-in-autofill?callsign=${encodeURIComponent(callsign)}`,
      { credentials: "same-origin" },
    );
    if (!response.ok) {
      return null;
    }
    return (await response.json()) as RosterMemory;
  } catch {
    // A network failure is a silent no-op — manual entry is never blocked.
    return null;
  }
}

/**
 * Fetches the REDACTED public session view (`GET /api/net-sessions/{id}/live`)
 * — the account-less read. A 404 means the session does not exist
 * and resolves to `null` (an account-less visitor is NEVER redirected to
 * sign-in — the public view renders a not-found state instead); any other
 * failure throws `ProblemError` (e.g. a 429 when IP-rate-governed). Unlike
 * `getSession` there is no 401→null branch: this read carries no cookie.
 */
export async function getPublicSession(
  id: string,
): Promise<PublicSessionSummary | null> {
  const response = await fetch(`/api/net-sessions/${id}/live`, {
    credentials: "same-origin",
  });
  if (response.status === 404) {
    return null;
  }
  if (!response.ok) {
    await throwProblem(response);
  }
  return (await response.json()) as PublicSessionSummary;
}

/**
 * Fetches the REDACTED public catch-up gap
 * (`GET /api/net-sessions/{id}/live/events?since=`) — the
 * account-less resume seam. Returns the ordered redacted `WireEvent[]` (no
 * `actorId`, no internal net ids); throws `ProblemError` on failure.
 */
export async function getPublicEventsSince(
  id: string,
  since: number,
): Promise<WireEvent[]> {
  const response = await fetch(`/api/net-sessions/${id}/live/events?since=${since}`, {
    credentials: "same-origin",
  });
  if (!response.ok) {
    await throwProblem(response);
  }
  return (await response.json()) as WireEvent[];
}

/** The edit-a-check-in request options — the post-edit editable
 * field set plus the optimistic-concurrency `expectedVersion`.
 *
 * Every editable field is three-state on the wire, and the three
 * states mean different things to the server:
 *
 * - **omitted** (`undefined`, dropped by `JSON.stringify`) → keep the stored
 * value. This is how the public staying toggle changes one field without
 * disturbing what staff logged.
 * - **`null` or `""`** → clear the field (PUT-replace).
 * - **a value** → replace the field.
 *
 * Passing `undefined` for a field the user deliberately emptied is therefore a
 * bug: it reads as "leave it alone", not as "clear it". */
export interface EditCheckInOptions {
  readonly callsign: string;
  readonly name?: string | null;
  readonly location?: string | null;
  /** The Maidenhead grid; `""` or `null` clears it (PUT-replace),
   * like `name`/`location`. */
  readonly grid?: string | null;
  /** The mode-shaped signal report; `null` clears it. */
  readonly signalReport?: string | null;
  readonly staying?: StayingStatus;
  /** The traffic/emergency precedence; `null` resets to routine. */
  readonly precedence?: Precedence | null;
  /** The optional traffic count; `null`/0 → no traffic. */
  readonly traffic?: number | null;
  /** The per-station STAFF note; `""` or `null` clears it
   * (PUT-replace). Operator-private — it crosses no public surface. */
  readonly notes?: string | null;
  /** The per-station PUBLIC note; `""` or `null` clears it
   * (PUT-replace), exactly like the staff note. Rides the same `EditCheckIn`
   * (Logger+) gate; a participant on the self path is refused it server-side. */
  readonly publicNote?: string | null;
  /** WHICH way in this station arrived on; `null` clears it
   * (PUT-replace), an omitted key keeps the stored value. Structured — a
   * `connection` id must be one of THIS session's frozen snapshot, or the
   * server answers 404 `/errors/net-connection-not-found` and appends nothing. */
  readonly via?: ViaWire | null;
  /** WHICH STATION passed this station's traffic; `null` clears it
   * (PUT-replace), an omitted key keeps the stored value. A rejected callsign is
   * a 400 `/errors/relayed-by-invalid` — its OWN slug, so the operator is sent
   * to the control that is actually at fault. */
  readonly relayedBy?: string | null;
  /** The version the client last saw; a mismatch surfaces `stale-version` (409). */
  readonly expectedVersion: number;
}

/**
 * Edits an already-logged check-in
 * (`PUT /api/net-sessions/{id}/check-ins/{checkInId}`). Returns the folded summary. Throws `ProblemError` on failure: a
 * 400 (invalid callsign/report/staying), a 403 (non-Logger), a 409
 * `stale-version` (the `expectedVersion` no longer matches — reload), or a 409
 * `lock-held` (another operator is editing).
 */
export async function updateCheckIn(
  id: string,
  checkInId: string,
  options: EditCheckInOptions,
): Promise<SessionSummaryBody> {
  const {
    callsign,
    name,
    location,
    grid,
    signalReport,
    staying,
    precedence,
    traffic,
    notes,
    publicNote,
    via,
    relayedBy,
    expectedVersion,
  } = options;
  const response = await fetch(`/api/net-sessions/${id}/check-ins/${checkInId}`, {
    method: "PUT",
    credentials: "same-origin",
    headers: JSON_HEADERS,
    // `undefined` fields drop out of JSON.stringify, and the server reads an
    // ABSENT key as "keep the stored value". A cleared field must
    // therefore be SENT: name/location/grid/notes as "" and report/traffic as
    // an explicit null. Dropping a key the operator emptied would silently
    // preserve the old value instead of erasing it.
    body: JSON.stringify({
      callsign,
      name,
      location,
      grid,
      signalReport,
      staying,
      precedence,
      traffic,
      notes,
      publicNote,
      via,
      relayedBy,
      expectedVersion,
    }),
  });
  if (!response.ok) {
    await throwProblem(response);
  }
  return (await response.json()) as SessionSummaryBody;
}

/**
 * Reorders the shared roster by precedence
 * (`POST /api/net-sessions/{id}/reorder`). NCS-only server-side
 * (`ReorderRoster`); the authoritative `roster.reordered` delta also streams
 * over the WS and folds for every console. Returns the folded summary; throws
 * `ProblemError` on failure (a 403 if the actor is not NCS, a 409 if the session
 * closed mid-reorder).
 */
export async function reorderRoster(
  id: string,
  by: "precedence" = "precedence",
): Promise<SessionSummaryBody> {
  const response = await fetch(`/api/net-sessions/${id}/reorder`, {
    method: "POST",
    credentials: "same-origin",
    headers: JSON_HEADERS,
    body: JSON.stringify({ by }),
  });
  if (!response.ok) {
    await throwProblem(response);
  }
  return (await response.json()) as SessionSummaryBody;
}

/**
 * Sets the session's standing roster ordering mode
 * (`POST /api/net-sessions/{id}/roster-order-mode`). NCS-only
 * server-side (`SetRosterOrderMode`); the authoritative `roster.order-mode-set`
 * delta — and the `roster.reordered` that may follow it in the same transaction
 * — also stream over the WS and fold for every console. Returns the folded
 * summary; throws `ProblemError` on failure (403 for a non-NCS, 409 if the
 * session closed, 400 for a mode outside the vocabulary).
 */
export async function setRosterOrderMode(
  id: string,
  mode: RosterOrderMode,
): Promise<SessionSummaryBody> {
  const response = await fetch(`/api/net-sessions/${id}/roster-order-mode`, {
    method: "POST",
    credentials: "same-origin",
    headers: JSON_HEADERS,
    body: JSON.stringify({ mode }),
  });
  if (!response.ok) {
    await throwProblem(response);
  }
  return (await response.json()) as SessionSummaryBody;
}

/**
 * Sets (or clears) the single working-station cursor
 * (`POST /api/net-sessions/{id}/worked-station`). Pass a `checkInId`
 * to designate the station being worked, or `null` to clear the cursor
 * (complete the current station). NCS-only server-side (`SetWorkedStation`); the
 * authoritative `station.worked-set` delta also streams over the WS and folds
 * for every console. Returns the folded summary; throws `ProblemError` on
 * failure (a 403 if not NCS, a 404 if the target is off-roster, a 409 if closed).
 */
export async function setWorkedStation(
  id: string,
  checkInId: string | null,
): Promise<SessionSummaryBody> {
  const response = await fetch(`/api/net-sessions/${id}/worked-station`, {
    method: "POST",
    credentials: "same-origin",
    headers: JSON_HEADERS,
    body: JSON.stringify({ checkInId }),
  });
  if (!response.ok) {
    await throwProblem(response);
  }
  return (await response.json()) as SessionSummaryBody;
}

/**
 * Sets (or clears) the net-level note (`PUT /api/net-sessions/{id}/net-note`).
 * Pass the note text, or `null`/`""` to clear it. Logger+ server-side
 * (`AnnotateSession`). Returns the folded summary; throws `ProblemError` on
 * failure (a 403 if below Logger, a 400 `note-invalid` if over-bound, a 409 if
 * closed).
 */
export async function setNetNote(
  id: string,
  note: string | null,
): Promise<SessionSummaryBody> {
  const response = await fetch(`/api/net-sessions/${id}/net-note`, {
    method: "PUT",
    credentials: "same-origin",
    headers: JSON_HEADERS,
    body: JSON.stringify({ note }),
  });
  if (!response.ok) {
    await throwProblem(response);
  }
  return (await response.json()) as SessionSummaryBody;
}

/**
 * Removes (tombstones) a check-in
 * (`DELETE /api/net-sessions/{id}/check-ins/{checkInId}`). The `expectedVersion` CAS guards it exactly as the edit does.
 */
export async function removeCheckIn(
  id: string,
  checkInId: string,
  expectedVersion: number,
): Promise<SessionSummaryBody> {
  const response = await fetch(`/api/net-sessions/${id}/check-ins/${checkInId}`, {
    method: "DELETE",
    credentials: "same-origin",
    headers: JSON_HEADERS,
    body: JSON.stringify({ expectedVersion }),
  });
  if (!response.ok) {
    await throwProblem(response);
  }
  return (await response.json()) as SessionSummaryBody;
}

/**
 * Moderates a check-in
 * (`POST /api/net-sessions/{id}/check-ins/{checkInId}/moderate`) — the NCS disciplinary remove, optionally blocking the
 * target's account from re-checking-in for the rest of the session. DISTINCT from
 * {@link removeCheckIn} (the Logger-floor correction remove): this is
 * `Capability::Moderate` (NetControl floor), gated server-side. The
 * `expectedVersion` CAS guards it exactly as remove does. Returns the folded
 * summary; throws `ProblemError` on failure — a 403 (`forbidden` for a non-NCS,
 * or `account-blocked` on the enforced side), a 409 `stale-version`, or a 422
 * `nothing-to-block` when `block` was requested against an account-less entry.
 */
export async function moderateCheckIn(
  id: string,
  checkInId: string,
  options: { readonly block: boolean; readonly expectedVersion: number },
): Promise<SessionSummaryBody> {
  const response = await fetch(
    `/api/net-sessions/${id}/check-ins/${checkInId}/moderate`,
    {
      method: "POST",
      credentials: "same-origin",
      headers: JSON_HEADERS,
      body: JSON.stringify({
        block: options.block,
        expectedVersion: options.expectedVersion,
      }),
    },
  );
  if (!response.ok) {
    await throwProblem(response);
  }
  return (await response.json()) as SessionSummaryBody;
}

/** The soft-lock lease the acquire endpoint returns. */
export interface LockLease {
  readonly holderCallsign: string;
  readonly expiresAt: string;
}

/**
 * Acquires (or renews) the soft-lock lease on a check-in
 * (`POST /api/net-sessions/{id}/check-ins/{checkInId}/lock`). Returns
 * the lease; throws `ProblemError` with `lock-held` (409) when a DIFFERENT
 * operator holds it.
 */
export async function acquireLock(
  id: string,
  checkInId: string,
): Promise<LockLease> {
  const response = await fetch(`/api/net-sessions/${id}/check-ins/${checkInId}/lock`, {
    method: "POST",
    credentials: "same-origin",
  });
  if (!response.ok) {
    await throwProblem(response);
  }
  return (await response.json()) as LockLease;
}

/**
 * Releases the soft-lock lease (`DELETE …/lock`). Best-effort and
 * idempotent (a 204 either way); never throws on the expected paths so a modal
 * close / unload cleanup can fire and forget. `keepalive: true` lets the
 * browser complete this request
 * even when it is issued from a `pagehide` handler during navigation/tab-close
 * — without it, an in-flight fetch is liable to be aborted mid-unload, so the
 * documented "release on close" path would silently degrade to the 15s TTL
 * wait every time.
 */
export async function releaseLock(id: string, checkInId: string): Promise<void> {
  const response = await fetch(`/api/net-sessions/${id}/check-ins/${checkInId}/lock`, {
    method: "DELETE",
    credentials: "same-origin",
    keepalive: true,
  });
  if (!response.ok && response.status !== 401 && response.status !== 404) {
    await throwProblem(response);
  }
}

/**
 * One explicit per-net role grant on the roles-list wire. The
 * console renders `callsign` + `role`; `callsign`/`grantedBy` are `null` when
 * absent (a grantee without a reserved callsign; a deleted grantor).
 */
export interface RoleGrant {
  readonly accountId: string;
  readonly callsign: string | null;
  readonly role: ViewerRole;
  readonly grantedBy: string | null;
  readonly grantedAt: string;
}

/**
 * The roles an operator may GRANT through the console: only the
 * staff tiers strictly below NCS. `owner`/`net-control` are never offered — the
 * server's `can_manage_role` ceiling is the real authority and refuses them
 * regardless; owner-set changes go through the net-definition owner endpoints.
 */
export type GrantableRole = "relay" | "logger";

/** The grant response (`{ accountId, role }`) from `POST …/roles`. */
export interface GrantedRole {
  readonly accountId: string;
  readonly role: ViewerRole;
}

/**
 * Lists the explicit role grants on a session
 * (`GET /api/net-sessions/{id}/roles`). Staff-gated server-side
 * (`ManageRoles`); returns the grants with callsigns for display. Throws
 * `ProblemError` on failure (a 403 for a non-manager, a 404 for a missing
 * session).
 */
export async function listRoles(id: string): Promise<RoleGrant[]> {
  const response = await fetch(`/api/net-sessions/${id}/roles`, {
    credentials: "same-origin",
  });
  if (!response.ok) {
    await throwProblem(response);
  }
  return (await response.json()) as RoleGrant[];
}

/**
 * Grants a per-net role to a station by callsign
 * (`POST /api/net-sessions/{id}/roles`).
 * Returns the granted `{ accountId, role }`. Throws `ProblemError` on failure: a
 * 404 `/errors/owner-not-found` (no account with that callsign), a 400
 * `/errors/role-invalid` (unknown role), or a 403 `/errors/forbidden` (the
 * actor may not grant that role — over its `can_manage_role` ceiling).
 */
export async function grantRole(
  id: string,
  body: { readonly callsign: string; readonly role: GrantableRole },
): Promise<GrantedRole> {
  const response = await fetch(`/api/net-sessions/${id}/roles`, {
    method: "POST",
    credentials: "same-origin",
    headers: JSON_HEADERS,
    body: JSON.stringify(body),
  });
  if (!response.ok) {
    await throwProblem(response);
  }
  return (await response.json()) as GrantedRole;
}

/**
 * Revokes a per-net role grant
 * (`DELETE /api/net-sessions/{id}/roles/{accountId}`). A 204 on success. Throws `ProblemError` on failure: a 404
 * `/errors/role-grant-not-found` (no such grant), or a 403 `/errors/forbidden`
 * (the actor may not revoke that role).
 */
export async function revokeRole(id: string, accountId: string): Promise<void> {
  const response = await fetch(`/api/net-sessions/${id}/roles/${accountId}`, {
    method: "DELETE",
    credentials: "same-origin",
  });
  if (!response.ok) {
    await throwProblem(response);
  }
}

/** Closes a session (`POST /api/net-sessions/{id}/close`) and returns the summary. */
export async function closeSession(id: string): Promise<SessionSummaryBody> {
  const response = await fetch(`/api/net-sessions/${id}/close`, {
    method: "POST",
    credentials: "same-origin",
  });
  if (!response.ok) {
    await throwProblem(response);
  }
  return (await response.json()) as SessionSummaryBody;
}

/**
 * VOLUNTARY handoff of a HEALTHY session to a qualified target. Returns the
 * folded summary. Throws `ProblemError` on failure: a 403
 * if the caller is not the current active NCS, a 422 `handoff-target-unqualified`
 * if the target is below the NetControl tier, a 409 if the session is not
 * active/live at write time.
 */
export async function handOffControl(
  id: string,
  targetAccountId: string,
): Promise<SessionSummaryBody> {
  const response = await fetch(`/api/net-sessions/${id}/handoff`, {
    method: "POST",
    credentials: "same-origin",
    headers: JSON_HEADERS,
    body: JSON.stringify({ targetAccountId }),
  });
  if (!response.ok) {
    await throwProblem(response);
  }
  return (await response.json()) as SessionSummaryBody;
}

/**
 * INVOLUNTARY claim of a STALLED session. The claimer
 * becomes the active NCS and the net returns to active. No body — the claimer IS
 * the target. Throws `ProblemError` on failure: a 403 if the caller lacks the
 * `ClaimControl` capability (Relay/Participant), a 409 `control-not-stalled` if
 * the session is not stalled.
 */
export async function claimControl(id: string): Promise<SessionSummaryBody> {
  const response = await fetch(`/api/net-sessions/${id}/claim-control`, {
    method: "POST",
    credentials: "same-origin",
  });
  if (!response.ok) {
    await throwProblem(response);
  }
  return (await response.json()) as SessionSummaryBody;
}
