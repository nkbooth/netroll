// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
/**
 * Fetch wrapper for the PUBLIC discovery landing. A plain `fetch`
 * read (the architecture's "Discovery/read data: plain fetch" guidance — no
 * TanStack Query, which is not a dependency). No auth header or cookie is
 * required; failures arrive as RFC 9457 problem+json and surface as
 * `ProblemError` (reusing `authApi`'s `throwProblem`).
 */

import { throwProblem } from "../auth/authApi";
import type { NetConnection } from "../nets/netsApi";

/** One discoverable net occurrence (backend `DiscoveryNetBody`): the redacted
 * public projection — the same descriptive fields as `PublicNetView` PLUS the
 * occurrence pointer and the net's permalink token, and
 * deliberately NO `ownerAccountIds` or `visibility`. */
export interface DiscoveryNet {
  /** The net's definition id. */
  id: string;
  definitionVersion: number;
  /** The specific upcoming occurrence's id. */
  occurrenceId: string;
  /** RFC 3339 UTC instant; rendered in local/relative time by the client. */
  scheduledStartAt: string;
  title: string;
  description: string | null;
  country: string | null;
  state: string | null;
  grid: string | null;
  netCategory: string;
  netType: string;
  expectedDurationMinutes: number | null;
  /** The permalink token — build `/nets/t/{linkToken}` from it. Every net on
   * this read is Listed, so the token of a net already published here is not a
   * capability anyone lacks. */
  linkToken: string;
  /** Every way this net can be reached, in the owner's order —
   * and the only place a connection fact appears on the
   * card; the flat `band`/`mode`/`plannedFrequencyHz` mirror that used to sit
   * beside it went stale the moment a net dropped its last RF way. Typed with
   * the SAME `NetConnection` the connection presenters take — a second local
   * interface here would compile, satisfy the tests, and quietly become another
   * hand-written twin of a shape this repo already has one twin of. */
  connections: NetConnection[];
  /** The connection that satisfied the band or mode filter, or `null` when
   * neither was applied.
   *
   * The SERVER decides this — it is the id its own filter predicate matched on.
   * Re-deriving it here would be a hand-written twin of that predicate, and a
   * twin that drifts marks a connection the server did not match on. An
   * IDENTITY, never rendered: the card marks the connection it names and shows
   * that connection's own description. */
  matchedConnectionId: string | null;
}

/** The filters and ordering the SERVER actually applied to `upcoming`, keyed
 * by query-param name.
 *
 * `/api/discovery` is lenient about unrecognised keys, so a misspelled filter
 * is dropped without a 400; this is how a client can tell what survived. A
 * dimension that was not filtered is ABSENT rather than null, so the key set
 * itself reads as "what was applied".
 *
 * Read it as a POSITIVE statement and nothing more. A key absent from it was
 * not applied — but that covers a known key sent blank (`?band=`, `?q=%20%20`)
 * just as much as a misspelled one, and `?bnad=40m` and no `bnad` at all
 * produce byte-identical echoes. So the echo cannot, on its own, tell a typo
 * from a parameter that was never sent; it is not a rejection list, and the
 * difference against what a client sent is NOT "exactly what was not
 * honoured": the PAGE
 * answers the typo question separately, from its own closed vocabulary
 * (`FILTER_KEYS`), never by diffing this object.
 *
 * `sort` is always present, and `sortUnavailable` is how the fall-back it
 * enables is STATED. `band` and `mode` were retired as sort options; a
 * URL carrying one was valid when somebody shared it, so it degrades to the
 * default ordering rather than 400ing, and the server names the token it could
 * not honour. Absent whenever the sort was honoured — a statement that is
 * always there states nothing.
 *
 * The filters and the ordering describe `upcoming` only — `activeNow` is never
 * filtered. The truncation statement, `truncated`, names the collections it
 * applies to and can name either. (This note used to say the whole object
 * "describes `upcoming` only", which was falsified and corrected.) */
export interface AppliedDiscoveryQuery {
  q?: string;
  band?: string;
  mode?: string;
  /** The connection kind applied, as its wire token. */
  kind?: string;
  country?: string;
  state?: string;
  grid?: string;
  category?: string;
  type?: string;
  sort: string;
  sortUnavailable?: string;
  /** The collections the server CUT, by their wire names (`"activeNow"`,
   * `"upcoming"`) in envelope order — ABSENT when nothing was cut. A collection
   * matching exactly its bound is complete and is not
   * named. There is no cursor and no `?limit=`: the cut is stated, not paged.
   *
   * Absence is meaningful here, so the `searchObjects` rule — throw on a
   * missing signal, never `?? []` — does NOT transfer to this key: a missing
   * `truncated` IS the statement that nothing was cut, consistent with this
   * object's "a key absent was not applied". The fence against a backend that
   * stops sending it is the backend's own truncation tests, not a client
   * throw. */
  truncated?: readonly string[];
}

/** The discovery envelope. `applied` is the server's statement of what it did
 * with the query. */
export interface DiscoveryResponse {
  activeNow: DiscoveryNet[];
  upcoming: DiscoveryNet[];
  applied: AppliedDiscoveryQuery;
}

/** The filter/sort selections; each maps 1:1 to a discovery query param. All
 * optional — blank values are omitted from the request (never sent empty). */
export interface DiscoveryFilters {
  /** Free-text title substring (the `q` param). */
  q?: string;
  band?: string;
  mode?: string;
  /** Connection kind, as its wire token. Matched across a net's
   * connections inside the same predicate as band and mode. */
  kind?: string;
  country?: string;
  state?: string;
  grid?: string;
  category?: string;
  type?: string;
  sort?: string;
}

/** Every query param this page speaks, in the order it serialises them — the
 * single vocabulary behind four uses: the request's query string, the browser's
 * query string, the applied-echo render order, and the set-difference that names
 * the URL keys this page ignored. */
export const FILTER_KEYS = [
  "q",
  "band",
  "mode",
  "kind",
  "country",
  "state",
  "grid",
  "category",
  "type",
  "sort",
] as const satisfies readonly (keyof DiscoveryFilters)[];

/** Compile-time tripwire: adding a field to `DiscoveryFilters` without adding it
 * to `FILTER_KEYS` makes this assignment fail to type-check. `satisfies` above
 * only catches keys that do not belong; this catches the ones that are missing,
 * which is the direction drift actually goes. It is an exported const rather
 * than a type alias because `noUnusedLocals` (tsconfig.app.json) would break the
 * build on an unreferenced alias whether or not the list was exhaustive. */
export const FILTER_KEYS_ARE_EXHAUSTIVE: Exclude<
  keyof DiscoveryFilters,
  (typeof FILTER_KEYS)[number]
> extends never
  ? true
  : never = true;

/** Serialises filters into query params: `FILTER_KEYS` order, blank and
 * undefined values omitted (the server reads absent as "not filtered"). Iterating
 * the key list rather than `Object.entries` is what makes the output depend on
 * the filter VALUES and never on the order they were set in. */
export function discoverySearchParams(
  filters: DiscoveryFilters,
): URLSearchParams {
  const params = new URLSearchParams();
  for (const key of FILTER_KEYS) {
    const value = filters[key];
    if (value !== undefined && value.trim() !== "") {
      params.set(key, value);
    }
  }
  return params;
}

/** Reads filters back out of a URL: the arrival whitelist. Keys outside
 * `FILTER_KEYS` are simply never read, which is how a `fbclid` on a shared link
 * stays out of the request. Values are taken VERBATIM — no trimming, no
 * coercion, no checking against the options this page happens to offer. A value
 * the server does not recognise is the server's call to make (a 400, or an echo
 * stating a fall-back); sanitising here would mask that answer.
 *
 * So a shared `?sort=band` link KEEPS its `sort=band` in the address bar and
 * goes on re-sending it, getting the stated fall-back each time, until the
 * viewer picks a sort. That is deliberate: scrubbing it
 * here would replace the server's answer with a silent default. */
export function filtersFromSearchParams(
  params: URLSearchParams,
): DiscoveryFilters {
  const filters: DiscoveryFilters = {};
  for (const key of FILTER_KEYS) {
    const value = params.get(key);
    if (value !== null) {
      filters[key] = value;
    }
  }
  return filters;
}

/** Builds the `/api/discovery` URL from the SAME serialiser the browser's
 * address bar gets, so the request's query string and the shareable link cannot
 * drift apart. */
function discoveryUrl(filters: DiscoveryFilters): string {
  const query = discoverySearchParams(filters).toString();
  return query === "" ? "/api/discovery" : `/api/discovery?${query}`;
}

/**
 * Loads the public discovery landing data. A problem response (e.g.
 * `/errors/discovery-query-invalid` for a bad filter) throws `ProblemError`.
 */
export async function getDiscovery(
  filters: DiscoveryFilters = {},
): Promise<DiscoveryResponse> {
  const response = await fetch(discoveryUrl(filters), {
    credentials: "same-origin",
  });
  if (!response.ok) {
    await throwProblem(response);
  }
  return (await response.json()) as DiscoveryResponse;
}
