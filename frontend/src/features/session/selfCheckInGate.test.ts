// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
import { describe, expect, it } from "vitest";

import { selfCheckInGate } from "./selfCheckInGate";
import type { Account } from "../auth/authApi";

/** A fully-gated (allowed) account, overridable per case. */
function account(overrides: Partial<Account> = {}): Account {
  return {
    id: "a",
    email: "maria@example.com",
    emailVerifiedAt: "2026-07-16T00:00:00Z",
    consentRequired: false,
    requiredTermsVersion: "1",
    callsign: "W2BCD",
    displayName: "Maria",
    location: "Hartford, CT",
    grid: null,
    avatarUrl: null,
    gravatarUrl: "https://example.com/g",
    qrzCredentialsSet: false,
    isAdmin: false,
    ...overrides,
  };
}

describe("selfCheckInGate", () => {
  it("routes a signed-out viewer to sign-in", () => {
    expect(selfCheckInGate(null)).toEqual({ kind: "route", to: "/sign-in" });
  });

  it("routes an unconsented account to the consent gate", () => {
    expect(selfCheckInGate(account({ consentRequired: true }))).toEqual({
      kind: "route",
      to: "/consent",
    });
  });

  it("routes an email-unverified account back through sign-in", () => {
    expect(selfCheckInGate(account({ emailVerifiedAt: null }))).toEqual({
      kind: "route",
      to: "/sign-in",
    });
  });

  it("routes a callsign-less account to profile (callsign setup)", () => {
    expect(selfCheckInGate(account({ callsign: null }))).toEqual({
      kind: "route",
      to: "/profile",
    });
  });

  it("lets a verified, consented, callsign-holding account check in", () => {
    expect(selfCheckInGate(account())).toEqual({ kind: "check-in" });
  });
});
