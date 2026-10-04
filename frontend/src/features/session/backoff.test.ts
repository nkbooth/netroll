// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
import { describe, expect, it } from "vitest";

import { BACKOFF_CAP_MS, backoffCeiling, nextBackoff } from "./backoff";

/**
 * Backoff is pure and deterministically testable via an injected RNG.
 * Tests assert the exponential ceiling grows monotonically to the cap and that
 * the jittered value stays within bounds — never a brittle exact-ms assertion.
 */

describe("backoffCeiling", () => {
  it("grows monotonically and never decreases", () => {
    let previous = 0;
    for (let attempt = 0; attempt <= 12; attempt += 1) {
      const ceiling = backoffCeiling(attempt);
      expect(ceiling).toBeGreaterThanOrEqual(previous);
      previous = ceiling;
    }
  });

  it("caps at BACKOFF_CAP_MS for large attempts", () => {
    expect(backoffCeiling(50)).toBe(BACKOFF_CAP_MS);
    expect(backoffCeiling(100)).toBe(BACKOFF_CAP_MS);
  });

  it("grows exponentially early (each step at least the previous)", () => {
    expect(backoffCeiling(1)).toBeGreaterThan(backoffCeiling(0));
    expect(backoffCeiling(2)).toBeGreaterThan(backoffCeiling(1));
  });
});

describe("nextBackoff", () => {
  it("stays within [ceiling/2, ceiling] across the jitter range", () => {
    for (let attempt = 0; attempt <= 10; attempt += 1) {
      const ceiling = backoffCeiling(attempt);
      const low = nextBackoff(attempt, () => 0);
      const high = nextBackoff(attempt, () => 1);
      const mid = nextBackoff(attempt, () => 0.5);
      expect(low).toBeGreaterThanOrEqual(ceiling / 2);
      expect(high).toBeLessThanOrEqual(ceiling);
      expect(mid).toBeGreaterThanOrEqual(ceiling / 2);
      expect(mid).toBeLessThanOrEqual(ceiling);
    }
  });

  it("never exceeds the cap", () => {
    expect(nextBackoff(99, () => 1)).toBeLessThanOrEqual(BACKOFF_CAP_MS);
  });

  it("returns a non-negative delay", () => {
    expect(nextBackoff(0, () => 0)).toBeGreaterThanOrEqual(0);
  });

  it("adds real jitter — different RNG outputs give different delays", () => {
    const a = nextBackoff(5, () => 0);
    const b = nextBackoff(5, () => 1);
    expect(a).not.toBe(b);
  });
});
