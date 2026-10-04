// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
import { describe, expect, it } from "vitest";

describe("DOM test harness", () => {
  it("provides a document whose root element accepts data attributes", () => {
    document.documentElement.dataset.probe = "on";

    expect(document.documentElement.dataset.probe).toBe("on");

    delete document.documentElement.dataset.probe;
  });

  it("provides a localStorage that persists within the test", () => {
    localStorage.setItem("harness-probe", "1");

    expect(localStorage.getItem("harness-probe")).toBe("1");

    localStorage.removeItem("harness-probe");
  });
});
