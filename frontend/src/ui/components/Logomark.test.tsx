// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
import { render, screen } from "@testing-library/react";
import { describe, expect, it } from "vitest";

import { expectNoAxeViolations } from "../../test/axe";
import { Logomark } from "./Logomark";

describe("Logomark", () => {
  it("is decorative — the wordmark beside it carries the accessible name", () => {
    render(
      <>
        <Logomark />
        <span>NetRoll</span>
      </>,
    );

    // No img role, no accessible name of its own: a mark announced next to
    // the wordmark it accompanies would read the brand twice.
    expect(screen.queryByRole("img")).not.toBeInTheDocument();
    expect(screen.getByTestId("logomark")).toHaveAttribute("aria-hidden", "true");
  });

  it("fills the accent-square with the brand accent and inks the glyph against it", () => {
    render(<Logomark />);

    const mark = screen.getByTestId("logomark");
    expect(mark).toHaveStyle({ background: "var(--accent)" });
    // The glyph must ink against the accent fill, not against the page: on
    // light the accent is a mid teal, so a --text stroke would go muddy.
    expect(mark.querySelector("svg")).toHaveAttribute("stroke", "var(--bg)");
  });

  it("derives its halo from the accent token so it tracks the active theme", () => {
    render(<Logomark />);

    expect(screen.getByTestId("logomark").style.boxShadow).toContain(
      "var(--accent)",
    );
  });

  it("scales the glyph with the square rather than overflowing it", () => {
    render(<Logomark size={48} />);

    const mark = screen.getByTestId("logomark");
    const glyph = mark.querySelector("svg");

    expect(mark).toHaveStyle({ width: "48px", height: "48px" });
    const glyphWidth = Number(glyph?.getAttribute("width"));
    expect(glyphWidth).toBeGreaterThan(0);
    expect(glyphWidth).toBeLessThan(48);
  });

  it("defaults to the 30px shell-chrome size", () => {
    render(<Logomark />);

    expect(screen.getByTestId("logomark")).toHaveStyle({
      width: "30px",
      height: "30px",
    });
  });

  it("has no accessibility violations", async () => {
    const { container } = render(<Logomark />);
    await expectNoAxeViolations(container);
  });
});
