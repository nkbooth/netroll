// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
import { afterEach, describe, expect, it, vi } from "vitest";

import { motionDuration, prefersReducedMotion } from "./motion";

const stubMatchMedia = (reduce: boolean): void => {
  vi.stubGlobal(
    "matchMedia",
    vi.fn().mockImplementation((query: string) => ({
      matches: query === "(prefers-reduced-motion: reduce)" && reduce,
      media: query,
    })),
  );
};

afterEach(() => {
  vi.unstubAllGlobals();
});

describe("motionDuration", () => {
  it("resolves the DESIGN.md budget when motion is allowed", () => {
    stubMatchMedia(false);

    expect(prefersReducedMotion()).toBe(false);
    expect(motionDuration("durationFast")).toBe("140ms");
    expect(motionDuration("livePulse")).toBe("2s");
    expect(motionDuration("cursorWashFade")).toBe("200ms");
  });

  it("resolves to instant when prefers-reduced-motion is set", () => {
    stubMatchMedia(true);

    expect(prefersReducedMotion()).toBe(true);
    expect(motionDuration("durationFast")).toBe("0s");
    expect(motionDuration("livePulse")).toBe("0s");
    expect(motionDuration("cursorWashFade")).toBe("0s");
  });
});
