// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
/**
 * Fetch wrapper for favorites / "My Nets". Cookie-authenticated
 * like the other account endpoints; failures arrive as RFC 9457 problem+json
 * and surface as `ProblemError` (reusing `authApi`'s `throwProblem`).
 *
 * The list read is keyset-paginated: `getMyNets` serves one
 * page for the My Nets tab. The two surfaces that ask "is THIS net favorited?"
 * — a question page 1 cannot answer — ask `fetchFavoriteMembership` about the
 * nets actually on screen instead of walking the whole collection.
 */

import { throwProblem } from "../auth/authApi";
import type { NetConnection } from "./netsApi";

/** One favorited net (backend `FavoriteNetBody`): the PRIVATE, account-scoped
 * projection. It OMITS `ownerAccountIds` and `visibility`, but DOES carry
 * `linkToken` (the favoriter's return-link to `/nets/t/{linkToken}`) and
 * `archivedAt` (the archived indicator). */
export interface FavoriteNet {
  id: string;
  title: string;
  description: string | null;
  /** Every way to reach this net, in the owner's order. It
   * replaces the flat `band`/`mode` pair the card used to render, which went
   * stale the moment a net dropped its last RF way; the card now shows the
   * same connection summary the Owned tab and the discovery cards do. */
  connections: NetConnection[];
  country: string | null;
  state: string | null;
  grid: string | null;
  netCategory: string;
  netType: string;
  expectedDurationMinutes: number | null;
  /** The permalink token — build `/nets/t/{linkToken}` from it. */
  linkToken: string;
  /** RFC 3339 archival instant, or `null` for an active net (the indicator). */
  archivedAt: string | null;
  /** When the account favorited this net, RFC 3339 UTC. */
  favoritedAt: string;
}

/** One page of the signed-in account's favorites, newest-favorited first.
 * `nextCursor` is `null` on the last page — the caller offers "Load more" only
 * while it is a string. */
export interface FavoritesPage {
  items: FavoriteNet[];
  nextCursor: string | null;
}

/**
 * The most ids one membership read may ask about (the server's
 * `MAX_PAGE_LIMIT`). A longer list is a 400, never a partial answer — the server
 * refuses to report "not favorited" for the ids it silently dropped — so a
 * caller with more nets on screen sends them in batches of this size.
 */
const MEMBERSHIP_BATCH_SIZE = 200;

/** Favorites a net for the signed-in account (idempotent PUT). Resolves on
 * 204; a problem response (`/errors/net-definition-not-found`,
 * `/errors/rate-limited`) throws `ProblemError`. */
export async function favoriteNet(netId: string): Promise<void> {
  const response = await fetch(`/api/favorites/${netId}`, {
    method: "PUT",
    credentials: "same-origin",
  });
  if (!response.ok) {
    await throwProblem(response);
  }
}

/** Unfavorites a net for the signed-in account (idempotent DELETE). Resolves
 * on 204; a problem response throws `ProblemError`. */
export async function unfavoriteNet(netId: string): Promise<void> {
  const response = await fetch(`/api/favorites/${netId}`, {
    method: "DELETE",
    credentials: "same-origin",
  });
  if (!response.ok) {
    await throwProblem(response);
  }
}

/** One page of `GET /api/favorites` under `search`. */
async function fetchFavoritesPage(search: URLSearchParams): Promise<FavoritesPage> {
  const query = search.toString();
  const response = await fetch(query === "" ? "/api/favorites" : `/api/favorites?${query}`, {
    credentials: "same-origin",
  });
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
    throw new Error("favorites page carries no items array");
  }
  if (body.nextCursor === undefined) {
    throw new Error("favorites page carries no nextCursor");
  }
  return { items: body.items as FavoriteNet[], nextCursor: body.nextCursor };
}

/**
 * Loads one page of the signed-in account's "My Nets" (its favorited nets),
 * newest first, at the server's default page size. `cursor` must be the
 * `nextCursor` THIS read returned, echoed back as received — it is opaque. A
 * malformed cursor is refused with a 400 rather than silently restarting at
 * page 1; but the server does not verify which read issued a well-formed one,
 * so a `nextCursor` from another paged read (the Owned tab's, say — the same
 * page holds both) is accepted and anchors this list at an unrelated position,
 * yielding a silently truncated list rather than an error. A problem response
 * (`/errors/unauthenticated`) throws `ProblemError`.
 */
export async function getMyNets(cursor?: string): Promise<FavoritesPage> {
  const search = new URLSearchParams();
  if (cursor !== undefined) {
    search.set("cursor", cursor);
  }
  return fetchFavoritesPage(search);
}

/** One batch of `GET /api/favorites/membership?ids=…`: the subset of `ids` the
 * account has favorited. */
async function fetchMembershipBatch(ids: readonly string[]): Promise<string[]> {
  const search = new URLSearchParams({ ids: ids.join(",") });
  const response = await fetch(`/api/favorites/membership?${search.toString()}`, {
    credentials: "same-origin",
  });
  if (!response.ok) {
    await throwProblem(response);
  }
  const body = (await response.json()) as { favorited?: unknown };
  // A dropped `favorited` key would read as "none favorited" for every net
  // asked about; the server is strict about the question for the same reason,
  // so the answer is held to the same standard here.
  if (!Array.isArray(body.favorited)) {
    throw new Error("favorites membership carries no favorited array");
  }
  return body.favorited as string[];
}

/**
 * Which of `ids` the signed-in account has favorited, as a set — the answer
 * behind every star on a discovery card or a public net page. One request per
 * {@link MEMBERSHIP_BATCH_SIZE} ids (so one, for any ordinary screen) whatever
 * the account's favorite count; it replaced walking every page of the account's
 * favorites, whose request count grew with that count and could drain the read
 * budget the My Nets tab shares. An empty `ids` makes no request. A problem
 * response (`/errors/unauthenticated`, `/errors/rate-limited`) on any batch
 * throws `ProblemError`.
 */
export async function fetchFavoriteMembership(ids: readonly string[]): Promise<Set<string>> {
  const favorited = new Set<string>();
  for (let start = 0; start < ids.length; start += MEMBERSHIP_BATCH_SIZE) {
    const batch = ids.slice(start, start + MEMBERSHIP_BATCH_SIZE);
    for (const id of await fetchMembershipBatch(batch)) {
      favorited.add(id);
    }
  }
  return favorited;
}
