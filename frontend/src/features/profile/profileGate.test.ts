// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
import { describe, expect, it } from "vitest";

import { profileDestination } from "./profileGate";

describe("profileDestination", () => {
  it("sends the signed-out to sign-in", () => {
    expect(profileDestination("signed-out")).toBe("/sign-in");
  });

  it("steers an unconsented account to the gate", () => {
    expect(profileDestination("gate")).toBe("/consent");
  });

  it("renders in place once through (no redirect)", () => {
    expect(profileDestination("through")).toBeNull();
  });
});
