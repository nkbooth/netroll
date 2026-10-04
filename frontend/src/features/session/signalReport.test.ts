// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
import { describe, expect, it } from "vitest";

import { reportShapeForMode } from "./signalReport";

/**
 * These assert the mode→report-format LOGIC (the ham-radio family each mode
 * maps to), not just the copy: SSB/AM→RS, CW→RST, digital→dB, FM→qualitative,
 * mixed/unknown→general. The `format` field is the load-bearing decision.
 */
describe("reportShapeForMode", () => {
  it("maps SSB and AM to the RS family", () => {
    expect(reportShapeForMode("ssb").format).toBe("rs");
    expect(reportShapeForMode("am").format).toBe("rs");
  });

  it("maps CW to the RST family (adds the tone digit)", () => {
    expect(reportShapeForMode("cw").format).toBe("rst");
  });

  it("maps digital to the signed dB-SNR family", () => {
    expect(reportShapeForMode("digital").format).toBe("db");
  });

  it("maps FM to the qualitative family", () => {
    expect(reportShapeForMode("fm").format).toBe("qualitative");
  });

  it("maps mixed to the general free-form family with no fixed placeholder", () => {
    const shape = reportShapeForMode("mixed");
    expect(shape.format).toBe("general");
    expect(shape.placeholder).toBe("");
  });

  it("falls back to general for an unknown or absent mode (total, never throws)", () => {
    expect(reportShapeForMode(undefined).format).toBe("general");
    expect(reportShapeForMode("bogus").format).toBe("general");
  });

  it("gives each of the six mode tokens a distinct aria hint that names the format", () => {
    // The aria hint is what a screen-reader operator hears — it must be
    // format-appropriate and distinct across the report families.
    const hints = ["ssb", "cw", "am", "fm", "digital", "mixed"].map(
      (m) => reportShapeForMode(m).ariaHint,
    );
    // SSB and AM share the RS hint; the other four are distinct — 5 unique.
    expect(new Set(hints).size).toBe(5);
    expect(reportShapeForMode("cw").ariaHint).toContain("RST");
    expect(reportShapeForMode("digital").ariaHint).toContain("dB");
  });
});
