// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
import { render, screen } from "@testing-library/react";
import { describe, expect, it } from "vitest";

import { expectNoAxeViolations } from "../../test/axe";
import { RosterColumnHeads } from "./RosterColumnHeads";
import { ROSTER_CELL_BASIS } from "./RosterEntry";

describe("RosterColumnHeads", () => {
  it("labels only the columns the surface actually renders", () => {
    render(<RosterColumnHeads showPosition showBadge />);

    // A head for a column the rows omit would label empty space and push
    // every following label out of alignment.
    expect(screen.getByText("Station")).toBeInTheDocument();
    expect(screen.getByText("Source")).toBeInTheDocument();
    expect(screen.queryByText("Report")).not.toBeInTheDocument();
    expect(screen.queryByText("Precedence")).not.toBeInTheDocument();
  });

  it("adds the operator columns when the surface opts into them", () => {
    render(<RosterColumnHeads showBadge showReport showPrecedence />);

    expect(screen.getByText("Report")).toBeInTheDocument();
    expect(screen.getByText("Precedence")).toBeInTheDocument();
  });

  it("labels the precedence column on the observer density too", () => {
    // The strip's contract runs in BOTH directions: it must
    // not label a column the rows omit, AND it must not omit a label for a column
    // the rows show. The observer roster now renders a precedence/traffic cell,
    // so its strip has to follow — while still omitting the report column, which
    // is not one of the four fields the 2026-08-27 ruling moved.
    render(<RosterColumnHeads showPosition showBadge showPrecedence />);

    expect(screen.getByText("Precedence")).toBeInTheDocument();
    expect(screen.queryByText("Report")).not.toBeInTheDocument();
  });

  it("shares its cell widths with RosterEntry so heads sit over their columns", () => {
    render(<RosterColumnHeads showPosition showBadge showReport showPrecedence />);

    // The whole point of a head strip is alignment: these must be the SAME
    // flex bases the row cells use, not a second set that can drift.
    expect(screen.getByText("Source").style.flex).toBe(ROSTER_CELL_BASIS.badge);
    expect(screen.getByText("Report").style.flex).toBe(ROSTER_CELL_BASIS.report);
    expect(screen.getByText("Precedence").style.flex).toBe(
      ROSTER_CELL_BASIS.precedence,
    );
    expect(screen.getByText("Heard").style.flex).toBe(ROSTER_CELL_BASIS.heard);
  });

  it("names the trailing time column for what it holds on a closed session", () => {
    render(<RosterColumnHeads showBadge heardAbsolute />);

    // A frozen record shows a clock time, not "3m ago" — the label has to
    // agree with the cell beneath it.
    expect(screen.getByText("Checked")).toBeInTheDocument();
    expect(screen.queryByText("Heard")).not.toBeInTheDocument();
  });

  it("stays presentational: the rows below are list items, not table cells", () => {
    render(<RosterColumnHeads showBadge />);

    expect(screen.queryByRole("columnheader")).not.toBeInTheDocument();
    expect(screen.getByTestId("roster-column-heads")).toHaveAttribute(
      "aria-hidden",
      "true",
    );
  });

  it("has no accessibility violations", async () => {
    const { container } = render(<RosterColumnHeads showBadge showReport />);
    await expectNoAxeViolations(container);
  });
});
