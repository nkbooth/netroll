// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
/**
 * Pure gate routing decision for participant self-check-in: where
 * to send a viewer who taps "Check in" but is not yet allowed to (signed out,
 * unconsented, email-unverified, or callsign-less). The SERVER re-checks every
 * one of these on the write — this decision is UX only, the "gate-and-
 * return" affordance that carries the viewer back to the action afterward.
 *
 * Adds the callsign leg the prior consent and profile gates never needed:
 * self-check-in is the first action that requires BOTH a verified email AND a
 * claimed callsign.
 */

import type { Account } from "../auth/authApi";
import { gateDecision } from "../consent/consentGate";

/** A self-check-in gate verdict: proceed with the write, or route elsewhere first. */
export type SelfCheckInGate =
  | { readonly kind: "check-in" }
  | { readonly kind: "route"; readonly to: string };

/**
 * Decides the self-check-in gate from a `/me`-shaped account (or `null` when
 * signed out). The caller attaches `returnTo` (the current `/live/:id` path) to
 * the navigation so the routed page returns the viewer to the action.
 *
 * Reuses the shipped `gateDecision` for the signed-out/consent legs rather
 * than re-deriving them, so a future change to consent-gate semantics only
 * needs to happen in one place:
 * - signed out → `/sign-in`
 * - consent required → `/consent`
 * - email unverified → `/sign-in` (re-auth via magic link re-verifies)
 * - no callsign → `/profile` (callsign setup)
 * - otherwise → `check-in` (proceed; the server still re-checks)
 */
export function selfCheckInGate(
  account: Pick<
    Account,
    "consentRequired" | "emailVerifiedAt" | "callsign"
  > | null,
): SelfCheckInGate {
  if (account === null) {
    // gateDecision(null) === "signed-out" — checked explicitly (rather than
    // trusting the switch below alone) so the callsign/email legs further down
    // can rely on a narrowed, non-null `account` without an assertion.
    return { kind: "route", to: "/sign-in" };
  }
  if (gateDecision(account) === "gate") {
    return { kind: "route", to: "/consent" };
  }
  if (account.emailVerifiedAt === null) {
    return { kind: "route", to: "/sign-in" };
  }
  if (account.callsign === null) {
    return { kind: "route", to: "/profile" };
  }
  return { kind: "check-in" };
}
