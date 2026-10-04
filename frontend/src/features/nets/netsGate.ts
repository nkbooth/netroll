// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
/**
 * Net-form routing decision: reuses the consent-gate verdict
 * and adds the callsign requirement — net creation needs a reserved callsign,
 * so a callsign-less-but-consented account is steered to the profile
 * page. The server-enforced guards (`ConsentedAccount` + the in-handler
 * callsign check) are the control; this decision is UX only.
 */

import type { Account } from "../auth/authApi";
import { gateDecision } from "../consent/consentGate";

/** Where the net form sends the user; `null` means render in place. */
export function netFormDestination(
  account: Pick<Account, "consentRequired" | "callsign"> | null,
): string | null {
  switch (gateDecision(account)) {
    case "signed-out":
      return "/sign-in";
    case "gate":
      return "/consent";
    case "through":
      // Consented, but net creation is callsign-gated — send a callsign-less
      // account to set one first.
      return account && account.callsign === null ? "/profile" : null;
  }
}
