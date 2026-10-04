// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
import { render, screen } from "@testing-library/react";
import { describe, expect, it } from "vitest";

import { SourceBadge } from "./SourceBadge";
import { expectNoAxeViolations } from "../../test/axe";

/**
 * Source-badge tests. The badge enforces color + icon + label, never color
 * alone. The staff-entry flow only ever produces the Staff variant; the Self
 * variant is asserted here at the component level.
 */
describe("SourceBadge", () => {
  it("renders the Staff variant with a label, an icon, and the staff tone", () => {
    render(<SourceBadge source="staff" />);
    const badge = screen.getByText(/staff/i).closest("[data-source]");
    expect(badge?.getAttribute("data-source")).toBe("staff");
    // Icon is a decorative, aria-hidden non-color affordance (not the sole signal).
    expect(badge?.querySelector('[aria-hidden="true"]')).not.toBeNull();
    // Built on StatusIndicator: the staff tone token drives the label's
    // color (amber), never a hardcoded hex.
    const label = screen.getByText("Staff-entered");
    expect(label.parentElement?.style.color).toContain("--staff-text");
    // The chip's own fill/border (StatusIndicator does not carry these).
    expect((badge as HTMLElement).style.background).toContain("--staff-fill");
  });

  it("renders the Self variant when passed source=self", () => {
    render(<SourceBadge source="self" />);
    const badge = screen.getByText(/self/i).closest("[data-source]");
    expect(badge?.getAttribute("data-source")).toBe("self");
    const label = screen.getByText("Self");
    expect(label.parentElement?.style.color).toContain("--self-text");
    expect((badge as HTMLElement).style.background).toContain("--self-fill");
  });

  it("has no WCAG 2.1 AA violations", async () => {
    const { container } = render(<SourceBadge source="staff" />);
    await expectNoAxeViolations(container);
  });
});
