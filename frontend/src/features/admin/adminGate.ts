// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
/**
 * Admin dashboard routing decision. A pure function over the account tri-state,
 * mirroring `profileGate.ts` — the codebase gates per page, not with a router
 * wrapper.
 *
 * This is a RENDER gate only. The server's `AdminAccount` extractor is the
 * authority; every admin endpoint 403s a non-admin regardless of what the SPA
 * decided, so a tampered `isAdmin` buys nothing but a page of failed requests.
 */

import type { Account } from "../auth/authApi";

/**
 * Where the admin page sends this viewer, or `null` to render in place.
 *
 * `undefined` means the session check has not answered yet — hold still rather
 * than bouncing the viewer away and back on every load.
 */
export function adminDestination(
  account: Account | null | undefined,
): string | null {
  if (account === undefined) {
    return null;
  }
  if (account === null) {
    return "/sign-in";
  }
  return account.isAdmin ? null : "/";
}
