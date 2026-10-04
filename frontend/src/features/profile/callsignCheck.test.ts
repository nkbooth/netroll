// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
import { describe, expect, it } from "vitest";

import { isPlausibleCallsign } from "./callsignCheck";

describe("isPlausibleCallsign", () => {
  it("accepts shapes that look like a callsign, portable designators included", () => {
    for (const input of ["W1AW", "w1aw/p", "N1CCK", "2E0ABC", "  K1ABC  "]) {
      expect(isPlausibleCallsign(input)).toBe(true);
    }
  });

  it("rejects empty, whitespace-only, and digit-less input", () => {
    for (const input of ["", "   ", "ABC", "!!!", "W 1AW"]) {
      expect(isPlausibleCallsign(input)).toBe(false);
    }
  });
});
