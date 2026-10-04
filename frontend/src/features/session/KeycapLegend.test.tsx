// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it } from "vitest";

import { KeycapLegend } from "./KeycapLegend";
import { expectNoAxeViolations } from "../../test/axe";

/**
 * Keycap-legend tests. The legend makes BOTH `n` and `w`
 * discoverable via keyboard-reachable tooltips. Only `n`
 * had a wired action first; the legend advertises `w` as well. No arrow/j-k
 * navigation exists.
 */
describe("KeycapLegend", () => {
  it("renders both the n and w keycap chips", () => {
    render(<KeycapLegend />);
    expect(screen.getByText("n")).toBeInTheDocument();
    expect(screen.getByText("w")).toBeInTheDocument();
  });

  it("reveals each keycap's tooltip hint on keyboard focus", async () => {
    const user = userEvent.setup();
    render(<KeycapLegend />);

    await user.tab();
    expect(screen.getByRole("tooltip")).toHaveTextContent(/jump to check-in/i);

    await user.tab();
    expect(screen.getByRole("tooltip")).toHaveTextContent(/set working/i);
  });

  it("has no WCAG 2.1 AA violations", async () => {
    const { container } = render(<KeycapLegend />);
    await expectNoAxeViolations(container);
  });
});
