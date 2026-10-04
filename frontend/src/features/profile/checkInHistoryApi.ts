// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
/**
 * Fetch wrapper for the signed-in account's own check-in history. Cookie
 * authenticated like the other account endpoints; failures
 * arrive as problem+json.
 *
 * This is the profile *widget's* read, not the personal-data export: a keyset
 * page of six stable fields per row rather than the whole account as one
 * download. It carries only check-ins the account made ITSELF — a check-in an
 * operator logged for you from the radio has no account link in the record and
 * is not returned.
 */

import { throwProblem } from "../auth/authApi";

/** One entry in the signed-in account's own check-in history. */
export interface CheckInHistoryEntry {
  /** The session checked into — the stable key for a rendered row. */
  readonly netSessionId: string;
  /** The net's title, frozen onto the session at start. */
  readonly netTitle: string;
  /**
   * Band token, rendered raw (`40m`) — or `null`. Band and mode live on the
   * session snapshot's connection set rather than its top level, so
   * these are the FIRST connection's, and an internet-only net has neither.
   * The server omits nothing: it sends `null`, and a renderer must not print a
   * chip or a separator for a token that is not there.
   */
  readonly band: string | null;
  /** Mode token, rendered raw (`ssb`) — `null` on an internet-only net. */
  readonly mode: string | null;
  /**
   * WHICH way in this check-in arrived on, as its LABEL — or `null` when nobody
   * recorded one.
   *
   * `band`/`mode` are now THIS check-in's way in, not the net's first
   * connection's: on a cross-mode net the old read told every EchoLink
   * participant they had been on 20m. A way in that carries no band — an
   * EchoLink node, a talkgroup, a reflector — sends `null` for both, and this
   * is what such a row actually has to say.
   */
  readonly via: string | null;
  /** The callsign the account was logged under at the time. */
  readonly callsign: string;
  /** RFC 3339 instant; formatted for display in the component. */
  readonly checkedInAt: string;
}

/**
 * One page of history. `nextCursor` is `null` on the last page — the caller
 * offers "Load more" only while it is a string.
 */
export interface CheckInHistoryPage {
  items: CheckInHistoryEntry[];
  nextCursor: string | null;
}

/**
 * How many entries the profile widget asks for per page. Lives here, in the
 * caller, rather than as a server constant: the endpoint's own default and
 * maximum bound the *endpoint*, this bounds the *widget*.
 */
const WIDGET_PAGE_SIZE = 10;

/**
 * Fetches one page of the signed-in account's own check-in history, newest
 * first.
 *
 * `cursor` must be a `nextCursor` this server previously returned — it is
 * opaque, and the server refuses anything it did not issue rather than
 * silently restarting the list.
 */
export async function fetchRecentCheckIns(
  cursor?: string,
): Promise<CheckInHistoryPage> {
  const search = new URLSearchParams({ limit: String(WIDGET_PAGE_SIZE) });
  if (cursor !== undefined) {
    search.set("cursor", cursor);
  }
  const response = await fetch(`/api/accounts/me/check-ins?${search}`, {
    credentials: "same-origin",
  });
  if (!response.ok) {
    await throwProblem(response);
  }
  return (await response.json()) as CheckInHistoryPage;
}
