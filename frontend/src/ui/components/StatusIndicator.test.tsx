// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
import { render, screen } from "@testing-library/react";
import { describe, expect, it } from "vitest";

import { StatusIndicator } from "./StatusIndicator";

describe("StatusIndicator", () => {
  it("always renders an icon AND a text label, never color alone", () => {
    render(
      <StatusIndicator
        tone="live"
        icon={<svg data-testid="indicator-icon" />}
        label="Live"
      />,
    );

    // Color is supplementary — the label text and a non-color icon affordance
    // must both be present.
    expect(screen.getByText("Live")).toBeInTheDocument();
    expect(screen.getByTestId("indicator-icon")).toBeInTheDocument();
  });

  it("renders the label for the given status kind", () => {
    render(
      <StatusIndicator
        tone="out-of-sync"
        icon={<svg data-testid="indicator-icon" />}
        label="Out of sync"
      />,
    );

    expect(screen.getByText("Out of sync")).toBeInTheDocument();
    expect(screen.getByTestId("indicator-icon")).toBeInTheDocument();
  });
});
