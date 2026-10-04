// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
/**
 * Profile page routing decision: reuses the consent-gate
 * verdict (`GateDecision`) rather than reinventing gating, and maps it to
 * where the Profile page sends the user — or `null` to render in place.
 * ProfilePage is the first real destination-capturing consumer of the
 * gate-and-return generality VerifyPage established (`state: { returnTo }`).
 */

import type { GateDecision } from "../consent/consentGate";

/** Where a `GateDecision` sends the Profile page; `null` means render. */
export function profileDestination(decision: GateDecision): string | null {
  switch (decision) {
    case "signed-out":
      return "/sign-in";
    case "gate":
      return "/consent";
    case "through":
      return null;
  }
}
