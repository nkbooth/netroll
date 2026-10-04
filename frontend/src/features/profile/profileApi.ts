// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
/**
 * Fetch wrapper for profile management. Cookie authenticated
 * like the other account endpoints; failures arrive as problem+json.
 */

import type { Account } from "../auth/authApi";
import { throwProblem } from "../auth/authApi";

/** PUT full-replace body: `null` (or an omitted key) clears a field. */
export interface ProfileFields {
  displayName?: string | null;
  location?: string | null;
  grid?: string | null;
  avatarUrl?: string | null;
}

/**
 * Replaces the signed-in account's profile fields. Resolves with the full
 * account body (the server-normalized grid included) on 200; non-OK
 * responses throw `ProblemError`.
 */
export async function updateProfile(fields: ProfileFields): Promise<Account> {
  const response = await fetch("/api/accounts/me/profile", {
    method: "PUT",
    credentials: "same-origin",
    headers: { "content-type": "application/json" },
    body: JSON.stringify(fields),
  });
  if (!response.ok) {
    await throwProblem(response);
  }
  return (await response.json()) as Account;
}

/**
 * Uploads `file` as the signed-in account's avatar, resolving with the updated
 * account (its `avatarUrl` now the stored same-origin path).
 *
 * No `content-type` header is set on purpose: the browser must generate one
 * with the multipart boundary, and hand-setting `multipart/form-data` omits
 * that boundary — the server then cannot parse the body at all.
 */
export async function uploadAvatar(file: File): Promise<Account> {
  const body = new FormData();
  body.append("file", file);
  const response = await fetch("/api/accounts/me/avatar", {
    method: "POST",
    credentials: "same-origin",
    body,
  });
  if (!response.ok) {
    await throwProblem(response);
  }
  return (await response.json()) as Account;
}

/**
 * Removes an uploaded avatar, falling the account back to its Gravatar.
 * Leaves an externally-hosted `avatarUrl` untouched (the server decides).
 */
export async function removeAvatar(): Promise<Account> {
  const response = await fetch("/api/accounts/me/avatar", {
    method: "DELETE",
    credentials: "same-origin",
  });
  if (!response.ok) {
    await throwProblem(response);
  }
  return (await response.json()) as Account;
}

/**
 * The client half of the avatar default rule: the custom avatar wins,
 * else the server-derived Gravatar. Named (not inlined JSX) so the rule
 * stays testable.
 */
export function effectiveAvatarUrl(
  account: Pick<Account, "avatarUrl" | "gravatarUrl">,
): string {
  return account.avatarUrl ?? account.gravatarUrl;
}
