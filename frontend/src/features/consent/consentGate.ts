// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
/**
 * Pure gate routing decisions: where a session-aware view sends
 * the user, given what the server said about consent. The server-enforced
 * guard is the control; these decisions are UX only.
 */

import type { Account } from "../auth/authApi";

/** Route verdict for a signed-in-or-not account state. */
export type GateDecision = "gate" | "through" | "signed-out";

/**
 * Decides the gate route from a `/me`-shaped account (or `null` when
 * signed out). Signed-out browsing is never gated — browse/view is public.
 */
export function gateDecision(
  account: Pick<Account, "consentRequired"> | null,
): GateDecision {
  if (account === null) {
    return "signed-out";
  }
  return account.consentRequired ? "gate" : "through";
}

/**
 * Post-sign-in landing: the consent gate comes first when required; the
 * intended destination is reached only through it (gate-and-return).
 */
export function postSignInDestination(
  consentRequired: boolean,
  intended: string = "/",
): string {
  return consentRequired ? "/consent" : intended;
}
