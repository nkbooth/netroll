// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
import { render, screen } from "@testing-library/react";
import { describe, expect, it } from "vitest";

import { StayingIndicator } from "./StayingIndicator";
import { expectNoAxeViolations } from "../../test/axe";

/**
 * The staying indicator carries color + icon + label, never color alone, by
 * building on the shipped StatusIndicator. Tests assert the LABEL and
 * the presence of a non-color icon for each state, plus the data-staying hook
 * carrying the semantic state — not the exact hex.
 */
describe("StayingIndicator", () => {
  it("renders a green check + 'Staying' label for staying-for-comments", () => {
    render(<StayingIndicator staying="staying-for-comments" />);
    const el = screen.getByText("Staying").closest("[data-staying]");
    expect(el?.getAttribute("data-staying")).toBe("staying-for-comments");
    // A non-color icon renders alongside the label (color+icon+label floor).
    expect(el?.querySelector("svg")).not.toBeNull();
  });

  it("renders a muted 'In & out' label for in-and-out", () => {
    render(<StayingIndicator staying="in-and-out" />);
    const el = screen.getByText(/in ?& ?out/i).closest("[data-staying]");
    expect(el?.getAttribute("data-staying")).toBe("in-and-out");
    expect(el?.querySelector("svg")).not.toBeNull();
  });

  it("has no WCAG 2.1 AA violations", async () => {
    const { container } = render(<StayingIndicator staying="staying-for-comments" />);
    await expectNoAxeViolations(container);
  });
});
