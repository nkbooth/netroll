// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
/**
 * Fetch wrapper for net-definition CRUD. Cookie-authenticated
 * like the other account endpoints; failures arrive as problem+json and
 * surface as `ProblemError` (reusing `authApi`'s `throwProblem`).
 */

import { throwProblem } from "../auth/authApi";
import type { BotMitigationFields } from "../botMitigation/botMitigation";

/** One owner of a net, for display in the owner panel. The
 * callsign is `null` if the account has not reserved one. */
export interface Owner {
  accountId: string;
  callsign: string | null;
}

/** One way to reach a net, as returned by the API.
 *
 * A `NetConnection` is a way to reach a net. It is unrelated to
 * `ConnectionState`/`ConnectionStatus` in `features/session/`, which mean the
 * WebSocket transport's state — never shorten the name to `Connection`.
 *
 * Flat with `null`s: the backend enum guarantees a kind carries only its own
 * properties, and a reader keys on `kind`. `node` is EchoLink's OR AllStar's
 * and `reflector` is D-Star's, YSF's or URF's — `kind` is the only
 * disambiguator in both cases. */
export interface NetConnection {
  id: string;
  /** Zero-based place in the owner's order; dense across the list. Position 0
   * is the connection an ADIF export describes a QSO with. */
  position: number;
  kind: string;
  /** Exact Hz, on the two RF kinds only. On a SESSION body's top-level
   * `connections` array this is the frequency in force — current while live,
   * final at close — never the one the definition planned. The
   * planned value is not lost: it is still readable one level down, at
   * `definition.connections[n].plannedFrequencyHz` on the same session body,
   * untouched by any move. The name stays "planned" because there is ONE
   * connection shape for the definition read and both connections arrays here
   * — an `operatingFrequencyHz` sibling would be a second serializer, which is
   * forbidden. */
  plannedFrequencyHz: number | null;
  band: string | null;
  mode: string | null;
  repeaterOffsetHz: number | null;
  toneMode: string | null;
  toneValue: string | null;
  node: string | null;
  reflector: string | null;
  /** The DMR network a talkgroup lives on, as the operator wrote it. `null` on
   * every other kind, and on a DMR connection whose network nobody recorded. */
  network: string | null;
  talkgroup: string | null;
  /** `other`'s short, normalized name — the thing that is counted. A label in
   * the `unclassified-` namespace is machine-minted, never owner-authored. */
  label: string | null;
  detail: string | null;
}

/** The net definition as returned by the API (camelCase wire, mirroring the
 * backend `NetDefinitionBody`). Hz frequencies are integers; enums are their
 * kebab tokens. */
export interface NetDefinition {
  id: string;
  definitionVersion: number;
  title: string;
  description: string | null;
  country: string | null;
  state: string | null;
  grid: string | null;
  /** Every way to reach this net, in the owner's order — and,
   * the only place a connection fact appears: frequency,
   * band, mode, offset, tone, node and reflector each belong to a connection. */
  connections: NetConnection[];
  netCategory: string;
  netType: string;
  expectedDurationMinutes: number | null;
  visibility: string;
  /** The plaintext permalink token — the owner uses it to build the share URL. */
  linkToken: string;
  ownerAccountIds: string[];
  /** Owners paired with their callsign for display. */
  owners: Owner[];
  createdAt: string;
  updatedAt: string;
  /** RFC 3339 archival instant, or `null` for an active net. */
  archivedAt: string | null;
}

/** The PUBLIC token-read projection (backend `PublicNetBody`): the same net
 * fields MINUS `ownerAccountIds` and `linkToken` — the world-reachable
 * surface must not leak owner identities, and the caller already holds the
 * token. */
export interface PublicNetView {
  id: string;
  title: string;
  description: string | null;
  country: string | null;
  state: string | null;
  grid: string | null;
  /** Every way to reach this net, in the owner's order. */
  connections: NetConnection[];
  netCategory: string;
  netType: string;
  expectedDurationMinutes: number | null;
  visibility: string;
  createdAt: string;
  updatedAt: string;
  /** RFC 3339 archival instant, or `null` for an active net.
   * When set, the client renders "this net was archived" — the permalink stays
   * resolvable rather than 404-ing a shared link. */
  archivedAt: string | null;
}

/** A net schedule write shape. `kind` is `one-off` or `recurring`;
 * the other fields are the kind-specific rule. Maps 1:1 to the backend
 * `ScheduleRequest`/domain `RawSchedule`; the server validates authoritatively. */
export interface Schedule {
  kind: string;
  timezone: string;
  oneOffStartAt?: string;
  frequency?: string;
  timeOfDay?: string;
  weekday?: string;
  dayOfMonth?: string;
}

/** A planned occurrence: server id + the absolute start as an RFC 3339 UTC
 * string. The client renders it in the viewer's local time. */
export interface Occurrence {
  id: string;
  scheduledStartAt: string;
}

/** A net's delivery config as read back (backend `DeliveryConfigBody`).
 * Zero-or-more delivery emails and an optional webhook. The webhook HMAC
 * secret is NEVER in this read shape — only `webhookSecretSet` tells whether
 * one is stored. */
export interface DeliveryConfig {
  emails: string[];
  webhookUrl: string | null;
  /** The configured Discord channel-webhook URL, or `null`. Read
   * back in the clear exactly like `webhookUrl` — the owner supplied it and only
   * the owner can reach this endpoint. It IS a credential: a Discord webhook
   * carries its bearer token in the URL path. */
  discordWebhookUrl: string | null;
  /** Whether a webhook URL is configured. */
  webhookConfigured: boolean;
  /** Whether a webhook HMAC secret is stored — never the secret value itself. */
  webhookSecretSet: boolean;
}

/** The result of setting a delivery config. Extends the read shape with the
 * freshly minted `webhookSecret`, which the server reveals EXACTLY once — in
 * the PUT response that mints it. Absent on every later read
 * and on any save that did not mint. */
export interface DeliveryConfigResult extends DeliveryConfig {
  /** The reveal-once plaintext HMAC secret, present only on a fresh mint. */
  webhookSecret?: string;
}

/** The write shape for a delivery-config PUT. A PUT REPLACES: `webhookUrl` or
 * `discordWebhookUrl` of `null` (or empty) clears that target, and an empty
 * `emails` array clears the email targets. */
export interface DeliveryConfigInput {
  emails: string[];
  webhookUrl: string | null;
  discordWebhookUrl: string | null;
}

/** The occurrence-list response envelope. */
interface OccurrencesResponse {
  occurrences: Occurrence[];
}

/** The inbound SCALAR write shape: every field as its raw wire form (enums as
 * kebab tokens). The server owns the version, so it is never sent. No
 * connection fact rides here: the create body carries the
 * connection list beside these fields ([`NetDefinitionCreateFields`]), and
 * every later change to the list goes through [`setNetConnections`]. */
export interface NetDefinitionFields {
  title?: string;
  description?: string;
  country?: string;
  state?: string;
  grid?: string;
  netCategory?: string;
  netType?: string;
  expectedDuration?: string;
  visibility?: string;
}

/** The create body: the scalar fields plus the ways in the net is born with,
 * in the SAME per-connection shape `PUT .../connections` accepts.
 * A net cannot be created with an empty list — the server refuses it — so
 * create and edit speak one vocabulary and there is no second flat form. */
export interface NetDefinitionCreateFields extends NetDefinitionFields {
  connections: NetConnectionInput[];
}

/**
 * One net the signed-in account owns (My Nets' "Owned" tab). Same fields as
 * [`NetDefinition`] plus a live-session pointer and a next-occurrence pointer,
 * both populated server-side from the live session store and the schedule.
 */
export interface OwnedNet extends NetDefinition {
  /** The currently-live net-session id, or `null` when not live. */
  liveSessionId: string | null;
  /** RFC 3339 UTC start of the soonest upcoming occurrence, or `null` when
   * unscheduled. */
  nextOccurrenceAt: string | null;
}

/** One page of the signed-in account's owned nets, newest-created first.
 * `nextCursor` is `null` on the last page — the caller offers "Load more" only
 * while it is a string. */
export interface OwnedNetsPage {
  items: OwnedNet[];
  nextCursor: string | null;
}

/**
 * Loads one page of the signed-in account's ACTIVE (non-archived) owned net
 * definitions, newest first at the server's default page size, for the "My
 * Nets" Owned tab. `cursor` must be the `nextCursor` THIS read
 * returned, echoed back as received — it is opaque. A malformed cursor is
 * refused with a 400 rather than silently restarting at page 1; but the server
 * does not verify which read issued a well-formed one, so a `nextCursor` from
 * another paged read (the Favorites tab's, say — the same page holds both) is
 * accepted and anchors this list at an unrelated position, yielding a silently
 * truncated list rather than an error. A problem response throws
 * `ProblemError`.
 */
export async function getOwnedNets(cursor?: string): Promise<OwnedNetsPage> {
  const search = new URLSearchParams();
  if (cursor !== undefined) {
    search.set("cursor", cursor);
  }
  const query = search.toString();
  const response = await fetch(
    query === "" ? "/api/net-definitions" : `/api/net-definitions?${query}`,
    { credentials: "same-origin" },
  );
  if (!response.ok) {
    await throwProblem(response);
  }
  const body = (await response.json()) as {
    items?: unknown;
    nextCursor?: string | null;
  };
  // Our own server always sends both keys (`nextCursor` is `null` on the last
  // page), so a missing one is a contract break. It surfaces here rather than
  // being smoothed: `?? null` would read every page as the last one, and an
  // `items` that is not an array would reach `rows.map(...)` inside render,
  // outside every catch, and unmount the page.
  if (!Array.isArray(body.items)) {
    throw new Error("owned nets page carries no items array");
  }
  if (body.nextCursor === undefined) {
    throw new Error("owned nets page carries no nextCursor");
  }
  return { items: body.items as OwnedNet[], nextCursor: body.nextCursor };
}

const HZ_PER_MHZ = 1_000_000;
const HZ_FRACTIONAL_DIGITS = 6;

/**
 * Why a decimal-MHz string cannot become integer Hz. The client-side names for
 * the faults that live client-side (`NotNumeric`, `TooPrecise`) plus
 * the one the wire itself imposes (`too-large`), kept apart so the owner is
 * told what is actually wrong rather than "enter a number" for a value that is
 * one.
 */
export type MhzFault = "not-a-number" | "too-precise" | "too-large";

/** The outcome of [`parseMhz`]: exact Hz, or the one fault that stopped it. */
export type MhzParse =
  | { hz: number; fault: null }
  | { hz: null; fault: MhzFault };

/**
 * Parses a decimal-MHz string to exact integer Hz using integer math (no
 * float drift), naming the fault when it cannot.
 *
 * The client's half of the wire conversion: the editor's
 * inputs hold MHz strings and the write body carries Hz integers, so what the
 * owner typed goes through here on its way out (`rowToInput`), and a string
 * this faults is refused against its row — with the fault — before anything is
 * sent.
 *
 * A returned `hz` is always a SAFE integer (`Number.isSafeInteger`): exactly
 * representable, finite, and inside the server's `i64`. Whether it is a
 * frequency the domain admits (the band-plan span, a negative frequency) is
 * still the server's to judge — that refusal comes back with the row's index.
 * What cannot be left to the server is a number that does not survive the
 * wire: past 2^53 it is not the integer the owner typed, past `i64::MAX` the
 * body fails to deserialize and the problem carries no `connectionIndex`, and
 * an `Infinity` is written by `JSON.stringify` as `null` — a filled-in field
 * reported as missing. All three are `too-large` here so they stay row-scoped.
 */
export function parseMhz(input: string): MhzParse {
  const trimmed = input.trim();
  if (trimmed === "") {
    return { hz: null, fault: "not-a-number" };
  }
  // A leading "-" is stripped and re-applied after parsing the magnitude —
  // needed so this round-trips a signed repeater offset (e.g. "-0.6") back
  // to the same negative Hz value `hzToMhz` produced.
  const negative = trimmed.startsWith("-");
  const unsigned = negative ? trimmed.slice(1) : trimmed;
  const [whole, frac = "", ...rest] = unsigned.split(".");
  if (rest.length > 0) {
    return { hz: null, fault: "not-a-number" };
  }
  if (!/^\d*$/.test(whole) || !/^\d*$/.test(frac) || whole + frac === "") {
    return { hz: null, fault: "not-a-number" };
  }
  if (frac.length > HZ_FRACTIONAL_DIGITS) {
    return { hz: null, fault: "too-precise" };
  }
  const wholeHz = (whole === "" ? 0 : Number(whole)) * HZ_PER_MHZ;
  const fracHz = Number(frac.padEnd(HZ_FRACTIONAL_DIGITS, "0") || "0");
  const magnitudeHz = wholeHz + fracHz;
  if (!Number.isSafeInteger(magnitudeHz)) {
    return { hz: null, fault: "too-large" };
  }
  return { hz: negative ? -magnitudeHz : magnitudeHz, fault: null };
}

/**
 * [`parseMhz`] for a caller that only needs the value: exact integer Hz, or
 * `null` when the string cannot become one for any reason. Everything
 * `parseMhz`'s docstring guarantees of `hz` holds of a non-null result here.
 */
export function mhzToHz(input: string): number | null {
  return parseMhz(input).hz;
}

/**
 * Formats integer Hz back to a minimal decimal-MHz string (trailing zeros
 * trimmed) that round-trips through `mhzToHz`. Seeds the editor's frequency
 * inputs from a served connection (`connectionToRow`); `mhzToHz ∘ hzToMhz` is
 * the identity on every integer the server can serve, which is what lets an
 * untouched row go back on exactly the Hz it arrived with.
 */
export function hzToMhz(hz: number): string {
  // Sign is tracked separately from `whole` because a magnitude under 1 MHz
  // (e.g. the standard -0.600 MHz repeater offset) truncates to -0, and
  // `${-0}` stringifies as "0" — silently flipping the offset's direction.
  const sign = hz < 0 ? "-" : "";
  const magnitude = Math.abs(hz);
  const whole = Math.trunc(magnitude / HZ_PER_MHZ);
  const frac = (magnitude % HZ_PER_MHZ)
    .toString()
    .padStart(HZ_FRACTIONAL_DIGITS, "0")
    .replace(/0+$/, "");
  return frac === "" ? `${sign}${whole}` : `${sign}${whole}.${frac}`;
}

/** Decimal-MHz digits in the kHz group — the `.200` of `7.200`. */
const KHZ_GROUP_DIGITS = 3;

/** Decimal-MHz digits in the sub-kHz group — the `.1250` of `448.670.1250`,
 * i.e. the remaining Hz expressed in tenths of a Hz. */
const SUB_KHZ_GROUP_DIGITS = 4;

/**
 * Formats integer Hz the way a frequency is written and read on the air:
 * always `X.XXX` (`7.200`, `14.275`, `146.520`), extended to `X.XXX.XXXX`
 * (`448.670.1250`) when the frequency carries precision below 1 kHz.
 *
 * DISPLAY ONLY, and deliberately NOT the transport format: the second dot
 * makes it unparseable by `mhzToHz`, so anything sent to the server or seeded
 * into a form field must keep using `hzToMhz`, whose minimized output
 * round-trips. Padding is the whole point here — `hzToMhz(7_200_000)` is
 * `"7.2"`, which no operator would ever write.
 */
export function formatFrequencyMhz(hz: number): string {
  // Sign is tracked separately from `whole` because a magnitude under 1 MHz
  // (the standard -0.600 MHz repeater offset) truncates to -0, and `${-0}`
  // stringifies as "0" — silently flipping the offset's direction.
  const sign = hz < 0 ? "-" : "";
  const magnitude = Math.abs(hz);
  const whole = Math.trunc(magnitude / HZ_PER_MHZ);
  const withinMhz = magnitude % HZ_PER_MHZ;
  const khz = Math.trunc(withinMhz / 1000)
    .toString()
    .padStart(KHZ_GROUP_DIGITS, "0");
  const subKhzHz = withinMhz % 1000;
  if (subKhzHz === 0) {
    return `${sign}${whole}.${khz}`;
  }
  // Tenths of a Hz: 125 Hz reads as "1250", giving 448.670.1250.
  const subKhz = (subKhzHz * 10).toString().padStart(SUB_KHZ_GROUP_DIGITS, "0");
  return `${sign}${whole}.${khz}.${subKhz}`;
}

async function readDefinition(response: Response): Promise<NetDefinition> {
  if (!response.ok) {
    await throwProblem(response);
  }
  return (await response.json()) as NetDefinition;
}

/** Creates a net definition, born with the connection list `fields` names.
 * Resolves with the created definition on 201. */
export async function createNetDefinition(
  fields: NetDefinitionCreateFields,
  bot?: BotMitigationFields,
): Promise<NetDefinition> {
  const response = await fetch("/api/net-definitions", {
    method: "POST",
    credentials: "same-origin",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({ ...fields, ...bot }),
  });
  return readDefinition(response);
}

/** Loads a net definition the signed-in account owns. */
export async function getNetDefinition(id: string): Promise<NetDefinition> {
  const response = await fetch(`/api/net-definitions/${id}`, {
    credentials: "same-origin",
  });
  return readDefinition(response);
}

/** Edits a net definition's scalar fields; the server increments the version.
 * The connection list is not this request's to change — see
 * [`setNetConnections`]. */
export async function updateNetDefinition(
  id: string,
  fields: NetDefinitionFields,
): Promise<NetDefinition> {
  const response = await fetch(`/api/net-definitions/${id}`, {
    method: "PUT",
    credentials: "same-origin",
    headers: { "content-type": "application/json" },
    body: JSON.stringify(fields),
  });
  return readDefinition(response);
}

/**
 * Reads a net by its public link token — the UNAUTHENTICATED share surface
 * A missing/wrong token resolves as a `ProblemError`
 * carrying `/errors/net-definition-not-found` (the uniform, existence-hiding
 * 404). No session is required, though same-origin cookies are harmless.
 */
export async function getNetByToken(token: string): Promise<PublicNetView> {
  const response = await fetch(
    `/api/net-definitions/by-token/${token}`,
    { credentials: "same-origin" },
  );
  if (!response.ok) {
    await throwProblem(response);
  }
  return (await response.json()) as PublicNetView;
}

/**
 * What THIS editor sends for ONE connection on `PUT .../connections` and on
 * the create body.
 *
 * Two facts, kept apart. What the SERVER accepts (its `ConnectionRequest`) is
 * a verbatim echo of a [`NetConnection`]: the write speaks the
 * read's vocabulary, `plannedFrequencyHz` and `repeaterOffsetHz` are the same
 * exact-Hz integers they arrive as, and `position` is accepted when it agrees
 * with the entry's place in the array. What this TYPE is, is narrower: the
 * entry `rowToInput` builds, which never carries `position` — the array order
 * is the owner's order and the server numbers it, so a client that also sends
 * positions has two statements of one fact to keep in step. That narrowing is
 * deliberate, so a served `NetConnection` does not type-check as an input
 * without dropping `position` first; the fence is the point, not a gap. The
 * decimal-MHz string spellings the write used to take are refused by the
 * server with the entry's index — not accepted beside these. Convert what the
 * owner typed with [`mhzToHz`]; never send the output of
 * [`formatFrequencyMhz`] anywhere.
 *
 * Every property but `kind` is optional because a kind carries only its own:
 * an entry must hold no key belonging to another kind, or the value the owner
 * typed is discarded with a `200` and nothing tells them. `id` is echoed back
 * from a read so a connection keeps its identity across an edit; a connection
 * the owner just added omits it and the server mints one.
 */
export interface NetConnectionInput {
  id?: string;
  kind: string;
  plannedFrequencyHz?: number;
  band?: string;
  mode?: string;
  repeaterOffsetHz?: number;
  toneMode?: string;
  toneValue?: string;
  node?: string;
  reflector?: string;
  network?: string;
  talkgroup?: string;
  label?: string;
  detail?: string;
}

/**
 * Replaces a net's ordered connection list. A full replace:
 * the array order IS the owner's order, and position 0 is the connection an
 * ADIF export describes a QSO with.
 *
 * `expectedDefinitionVersion` is CAS-checked and must be the version the
 * caller's most recent write or read returned — a stale one is refused with
 * `/errors/stale-version` (409). The edit page aims it at the version the page
 * LOADED, which is what makes the check catch a co-owner's edit made while the
 * page was open; the scalar `PUT /api/net-definitions/{id}` never touches the
 * list, so it can safely follow this write rather than precede it.
 */
export async function setNetConnections(
  id: string,
  expectedDefinitionVersion: number,
  connections: NetConnectionInput[],
): Promise<NetDefinition> {
  const response = await fetch(`/api/net-definitions/${id}/connections`, {
    method: "PUT",
    credentials: "same-origin",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({ expectedDefinitionVersion, connections }),
  });
  return readDefinition(response);
}

/** Deletes a net definition. Resolves on 204. */
export async function deleteNetDefinition(id: string): Promise<void> {
  const response = await fetch(`/api/net-definitions/${id}`, {
    method: "DELETE",
    credentials: "same-origin",
  });
  if (!response.ok) {
    await throwProblem(response);
  }
}

/**
 * Adds a co-owner to a net by callsign. Resolves with the updated
 * definition (owner set now including the target); a problem response —
 * `/errors/owner-not-found` for an unknown callsign — throws `ProblemError`.
 */
export async function addNetOwner(
  id: string,
  callsign: string,
): Promise<NetDefinition> {
  const response = await fetch(`/api/net-definitions/${id}/owners`, {
    method: "POST",
    credentials: "same-origin",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({ callsign }),
  });
  return readDefinition(response);
}

/**
 * Removes a co-owner by account id. Resolves on 204; a problem
 * response — `/errors/last-owner` when it would orphan the net — throws
 * `ProblemError`.
 */
export async function removeNetOwner(
  id: string,
  accountId: string,
): Promise<void> {
  const response = await fetch(
    `/api/net-definitions/${id}/owners/${accountId}`,
    { method: "DELETE", credentials: "same-origin" },
  );
  if (!response.ok) {
    await throwProblem(response);
  }
}

/**
 * Sets or replaces a net's schedule. Resolves with the
 * materialized upcoming occurrences on 200; a `/errors/schedule-invalid`
 * problem throws `ProblemError` carrying the field-level detail.
 */
export async function setNetSchedule(
  id: string,
  schedule: Schedule,
): Promise<Occurrence[]> {
  const response = await fetch(`/api/net-definitions/${id}/schedule`, {
    method: "PUT",
    credentials: "same-origin",
    headers: { "content-type": "application/json" },
    body: JSON.stringify(schedule),
  });
  if (!response.ok) {
    await throwProblem(response);
  }
  return ((await response.json()) as OccurrencesResponse).occurrences;
}

/** Loads a net's STORED schedule rule — the seam the edit
 * form hydrates its schedule panel from. Resolves with `null` on the `204` a
 * net with no schedule returns; a schedule has no meaningful empty shape, so
 * the absent body IS the "unscheduled" signal and must not be read as JSON. */
export async function getNetSchedule(id: string): Promise<Schedule | null> {
  const response = await fetch(`/api/net-definitions/${id}/schedule`, {
    credentials: "same-origin",
  });
  if (!response.ok) {
    await throwProblem(response);
  }
  if (response.status === 204) {
    return null;
  }
  return (await response.json()) as Schedule;
}

/** Clears a net's schedule and its future occurrences.
 * Resolves on 204. */
export async function clearNetSchedule(id: string): Promise<void> {
  const response = await fetch(`/api/net-definitions/${id}/schedule`, {
    method: "DELETE",
    credentials: "same-origin",
  });
  if (!response.ok) {
    await throwProblem(response);
  }
}

/** Loads a net's upcoming planned occurrences, ascending. */
export async function getNetOccurrences(id: string): Promise<Occurrence[]> {
  const response = await fetch(`/api/net-definitions/${id}/occurrences`, {
    credentials: "same-origin",
  });
  if (!response.ok) {
    await throwProblem(response);
  }
  return ((await response.json()) as OccurrencesResponse).occurrences;
}

/** Loads a net's delivery config. A net with no config yet
 * resolves with the empty/off shape (200), not an error. The webhook secret
 * plaintext is never present here — only `webhookSecretSet`. */
export async function getDeliveryConfig(id: string): Promise<DeliveryConfig> {
  const response = await fetch(`/api/net-definitions/${id}/delivery-config`, {
    credentials: "same-origin",
  });
  if (!response.ok) {
    await throwProblem(response);
  }
  return (await response.json()) as DeliveryConfig;
}

/**
 * Sets or replaces a net's delivery targets. Resolves with
 * the stored config; when this call MINTS a webhook secret, the result carries
 * `webhookSecret` in plaintext — the ONLY time it is ever returned, so the owner
 * must copy it now. A `/errors/delivery-config-invalid` problem (e.g. a
 * non-https or blocked webhook URL) throws `ProblemError` with the field-level
 * detail.
 */
export async function setDeliveryConfig(
  id: string,
  input: DeliveryConfigInput,
): Promise<DeliveryConfigResult> {
  const response = await fetch(`/api/net-definitions/${id}/delivery-config`, {
    method: "PUT",
    credentials: "same-origin",
    headers: { "content-type": "application/json" },
    body: JSON.stringify(input),
  });
  if (!response.ok) {
    await throwProblem(response);
  }
  return (await response.json()) as DeliveryConfigResult;
}

/** Clears a net's delivery config — the "delivery off" state.
 * Resolves on 204. */
export async function clearDeliveryConfig(id: string): Promise<void> {
  const response = await fetch(`/api/net-definitions/${id}/delivery-config`, {
    method: "DELETE",
    credentials: "same-origin",
  });
  if (!response.ok) {
    await throwProblem(response);
  }
}
