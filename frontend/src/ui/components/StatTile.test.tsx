// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
import { render, screen } from "@testing-library/react";
import { describe, expect, it } from "vitest";

import { expectNoAxeViolations } from "../../test/axe";
import { StatTile } from "./StatTile";

describe("StatTile", () => {
  it("renders the numeral value and its label", () => {
    render(<StatTile value="14" label="Check-ins" />);

    expect(screen.getByText("14")).toBeInTheDocument();
    expect(screen.getByText("Check-ins")).toBeInTheDocument();
  });

  it("renders the label in uppercase presentation", () => {
    render(<StatTile value="3" label="Traffic passed" />);

    expect(screen.getByText("Traffic passed")).toHaveStyle({
      textTransform: "uppercase",
    });
  });

  it("renders a mono value when mono is set (elapsed/duration values)", () => {
    render(<StatTile value="01:31" label="Duration" mono />);

    expect(screen.getByText("01:31")).toHaveStyle({
      fontFamily: 'ui-monospace, "SF Mono", Menlo, Consolas, monospace',
    });
  });

  it("does not render a mono font for non-mono values", () => {
    render(<StatTile value="9" label="States / provinces" />);

    expect(screen.getByText("9").style.fontFamily).toBe("");
  });

  it("has no accessibility violations", async () => {
    const { container } = render(<StatTile value="14" label="Check-ins" />);
    await expectNoAxeViolations(container);
  });
});
