// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
/**
 * Fetch wrapper for callsign reservation & change. Cookie
 * authenticated like the other account endpoints; failures arrive as
 * problem+json.
 */

import type { Account } from "../auth/authApi";
import { throwProblem } from "../auth/authApi";

/**
 * Reserves or changes the signed-in account's callsign. Resolves with the
 * full account body (the server-normalized callsign included) on 200;
 * non-OK responses throw `ProblemError`.
 */
export async function setCallsign(callsign: string): Promise<Account> {
  const response = await fetch("/api/accounts/me/callsign", {
    method: "PUT",
    credentials: "same-origin",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({ callsign }),
  });
  if (!response.ok) {
    await throwProblem(response);
  }
  return (await response.json()) as Account;
}
