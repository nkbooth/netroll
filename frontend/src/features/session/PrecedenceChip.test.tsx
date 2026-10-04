// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
import { render, screen } from "@testing-library/react";
import { describe, expect, it } from "vitest";

import { PrecedenceChip } from "./PrecedenceChip";
import { expectNoAxeViolations } from "../../test/axe";

/**
 * Precedence-chip tests. The chip enforces the
 * color + icon + label contract (never color alone): Routine neutral, Priority
 * amber (the `catch` token family), Emergency red (the `sync` token family),
 * micro-caps, 6px radius. Assertions are on structure + tokens, never on the
 * exact copy beyond the variant label.
 */
describe("PrecedenceChip", () => {
  it("renders the Routine variant with the neutral tone + surface-2 fill", () => {
    render(<PrecedenceChip precedence="routine" />);
    const chip = screen.getByText(/routine/i).closest("[data-precedence]");
    expect(chip?.getAttribute("data-precedence")).toBe("routine");
    // A decorative, aria-hidden icon — the non-color affordance.
    expect(chip?.querySelector('[aria-hidden="true"]')).not.toBeNull();
    const label = screen.getByText("Routine");
    expect(label.parentElement?.style.color).toContain("--text-muted");
    expect((chip as HTMLElement).style.background).toContain("--surface-2");
    expect((chip as HTMLElement).style.borderRadius).toContain("--rounded-sm");
  });

  it("renders the Priority variant in the amber catch family", () => {
    render(<PrecedenceChip precedence="priority" />);
    const chip = screen.getByText(/priority/i).closest("[data-precedence]");
    expect(chip?.getAttribute("data-precedence")).toBe("priority");
    const label = screen.getByText("Priority");
    expect(label.parentElement?.style.color).toContain("--catch-text");
    expect((chip as HTMLElement).style.background).toContain("--catch-fill");
  });

  it("renders the Emergency variant in the red sync family", () => {
    render(<PrecedenceChip precedence="emergency" />);
    const chip = screen.getByText(/emergency/i).closest("[data-precedence]");
    expect(chip?.getAttribute("data-precedence")).toBe("emergency");
    const label = screen.getByText("Emergency");
    expect(label.parentElement?.style.color).toContain("--sync-text");
    expect((chip as HTMLElement).style.background).toContain("--sync-fill");
  });

  it("has no WCAG 2.1 AA violations", async () => {
    const { container } = render(<PrecedenceChip precedence="emergency" />);
    await expectNoAxeViolations(container);
  });
});
