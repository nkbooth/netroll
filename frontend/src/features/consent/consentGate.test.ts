// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
import { describe, expect, it } from "vitest";

import { gateDecision, postSignInDestination } from "./consentGate";

const account = (consentRequired: boolean) => ({
  id: "1",
  email: "op@example.com",
  emailVerifiedAt: "2026-07-15T00:00:00Z",
  consentRequired,
  requiredTermsVersion: "2026-07-15",
});

describe("gateDecision", () => {
  it("routes an unconsented account to the gate", () => {
    expect(gateDecision(account(true))).toBe("gate");
  });

  it("passes a consented account through", () => {
    expect(gateDecision(account(false))).toBe("through");
  });

  it("never gates the signed-out (browse/view stays public)", () => {
    expect(gateDecision(null)).toBe("signed-out");
  });
});

describe("postSignInDestination", () => {
  it("sends a consent-requiring sign-in to the gate", () => {
    expect(postSignInDestination(true)).toBe("/consent");
    expect(postSignInDestination(true, "/nets/42")).toBe("/consent");
  });

  it("sends a consented sign-in to the intended destination", () => {
    expect(postSignInDestination(false)).toBe("/");
    expect(postSignInDestination(false, "/nets/42")).toBe("/nets/42");
  });
});
