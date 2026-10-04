// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
import { render, screen } from "@testing-library/react";
import { describe, expect, it } from "vitest";

import { YourTurnIndicator } from "./YourTurnIndicator";
import { expectNoAxeViolations } from "../../test/axe";

describe("YourTurnIndicator", () => {
  it("renders an accessible 'your turn' status with a chip + note (color + icon + label)", () => {
    render(<YourTurnIndicator />);
    // The indicator is a live-region status carrying a text label — NOT color
    // alone: a "You" chip label plus the "You're next up" note.
    const status = screen.getByRole("status", { name: /next up/i });
    expect(status.getAttribute("data-your-turn")).toBe("true");
    // An icon accompanies the label (color + icon + label), marked aria-hidden so
    // the label carries the meaning.
    expect(status.querySelector("[aria-hidden='true']")).not.toBeNull();
  });

  it("uses the cyan accent tokens (same variables in both themes)", () => {
    render(<YourTurnIndicator />);
    const status = screen.getByRole("status", { name: /next up/i });
    // The chip's cyan fill and the note's accent-ink are token-driven, so a theme
    // switch re-resolves them — the component commits to no hard-coded color.
    const html = status.outerHTML;
    expect(html).toContain("var(--accent)");
    expect(html).toContain("var(--accent-ink)");
  });

  it("has no accessibility violations", async () => {
    const { container } = render(<YourTurnIndicator />);
    await expectNoAxeViolations(container);
  });
});
