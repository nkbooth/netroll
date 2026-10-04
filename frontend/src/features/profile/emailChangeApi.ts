// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
/**
 * Fetch wrappers for the identifying-email change flow. The
 * request half is cookie-authenticated like the other account endpoints;
 * the confirm half is token-scoped and public (opened from the new mailbox,
 * possibly with no session). Failures arrive as problem+json.
 */

import { throwProblem } from "../auth/authApi";

/**
 * Requests a change of the identifying email to `email`. Resolves on 202
 * (the confirmation link has been mailed to the new address); non-OK
 * responses throw `ProblemError`.
 */
export async function requestEmailChange(email: string): Promise<void> {
  const response = await fetch("/api/accounts/me/email-change", {
    method: "POST",
    credentials: "same-origin",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({ email }),
  });
  if (!response.ok) {
    await throwProblem(response);
  }
}

/**
 * Consumes an email-change confirmation token, completing the move.
 * Resolves with the account's new identifying email on 200; non-OK
 * responses throw `ProblemError`.
 */
export async function confirmEmailChange(
  token: string,
): Promise<{ email: string }> {
  const response = await fetch("/api/email-changes", {
    method: "POST",
    credentials: "same-origin",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({ token }),
  });
  if (!response.ok) {
    await throwProblem(response);
  }
  return (await response.json()) as { email: string };
}
