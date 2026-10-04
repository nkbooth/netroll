// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
import { describe, expect, it } from "vitest";

import { netFormDestination } from "./netsGate";

describe("netFormDestination", () => {
  it("sends the signed-out to sign-in", () => {
    expect(netFormDestination(null)).toBe("/sign-in");
  });

  it("steers an unconsented account to the consent gate", () => {
    expect(
      netFormDestination({ consentRequired: true, callsign: "W1AW" }),
    ).toBe("/consent");
  });

  it("steers a consented but callsign-less account to the profile", () => {
    expect(
      netFormDestination({ consentRequired: false, callsign: null }),
    ).toBe("/profile");
  });

  it("renders in place for a consented account holding a callsign", () => {
    expect(
      netFormDestination({ consentRequired: false, callsign: "W1AW" }),
    ).toBeNull();
  });
});
