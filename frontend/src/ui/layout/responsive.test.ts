// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
import { describe, expect, it } from "vitest";

import {
  BREAKPOINT_DESKTOP,
  BREAKPOINT_OPERATOR,
  layoutModeForWidth,
} from "./responsive";

describe("layoutModeForWidth", () => {
  it("stacks below the desktop breakpoint (phone)", () => {
    expect(layoutModeForWidth(340)).toEqual({
      mode: "stacked-card",
      density: "roomy",
    });
  });

  it("switches mode exactly at the desktop boundary", () => {
    // Both sides of the 639/640 instant — the boundary-vector discipline.
    expect(layoutModeForWidth(BREAKPOINT_DESKTOP - 1)).toMatchObject({
      mode: "stacked-card",
    });
    expect(layoutModeForWidth(BREAKPOINT_DESKTOP)).toMatchObject({
      mode: "row",
    });
  });

  it("keeps the tablet band roomy in rows", () => {
    // 768–1023: rows, but not yet operator-dense.
    expect(layoutModeForWidth(768)).toEqual({ mode: "row", density: "roomy" });
    expect(layoutModeForWidth(BREAKPOINT_OPERATOR - 1)).toEqual({
      mode: "row",
      density: "roomy",
    });
  });

  it("switches density exactly at the operator boundary", () => {
    expect(layoutModeForWidth(BREAKPOINT_OPERATOR - 1)).toMatchObject({
      density: "roomy",
    });
    expect(layoutModeForWidth(BREAKPOINT_OPERATOR)).toMatchObject({
      density: "compact",
    });
  });
});
