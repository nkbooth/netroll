// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
/**
 * Fetch wrapper for account self-deletion. Cookie-authenticated
 * like the other account endpoints. Because the delete revokes every session,
 * a 401 means the session is already gone — treated as already-signed-out and
 * resolved rather than thrown (the `deleteCurrentSession` precedent). Other
 * failures arrive as problem+json.
 */

import { throwProblem } from "../auth/authApi";

/**
 * Soft-deletes the signed-in account into the undelete window. Resolves on a
 * 204 (or a 401 — already signed out); any other non-OK response throws
 * `ProblemError`.
 */
export async function deleteAccount(): Promise<void> {
  const response = await fetch("/api/accounts/me", {
    method: "DELETE",
    credentials: "same-origin",
  });
  if (!response.ok && response.status !== 401) {
    await throwProblem(response);
  }
}
