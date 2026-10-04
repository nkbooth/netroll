// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
import { describe, expect, it } from "vitest";

import { adminDestination } from "./adminGate";
import type { Account } from "../auth/authApi";

function account(isAdmin: boolean): Account {
  return {
    id: "acct-1",
    email: "op@example.com",
    emailVerifiedAt: null,
    consentRequired: false,
    requiredTermsVersion: "1",
    callsign: null,
    displayName: null,
    location: null,
    grid: null,
    avatarUrl: null,
    gravatarUrl: "https://example.test/avatar",
    qrzCredentialsSet: false,
    isAdmin,
  };
}

describe("adminDestination", () => {
  it("holds still while the session check is in flight", () => {
    // Redirecting on `undefined` would bounce an admin to the discovery root
    // before /me has answered, every single load.
    expect(adminDestination(undefined)).toBeNull();
  });

  it("sends a signed-out visitor to sign in", () => {
    expect(adminDestination(null)).toBe("/sign-in");
  });

  it("sends a signed-in non-admin away from the admin surface", () => {
    // The server would 403 every call anyway; redirecting avoids rendering a
    // page whose every request is doomed.
    expect(adminDestination(account(false))).toBe("/");
  });

  it("renders in place for an admin", () => {
    expect(adminDestination(account(true))).toBeNull();
  });
});
