// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
import { render, screen } from "@testing-library/react";
import { describe, expect, it } from "vitest";

import { expectNoAxeViolations } from "../../test/axe";
import { Card } from "./Card";

describe("Card", () => {
  it("renders its children", () => {
    render(
      <Card>
        <p>Net closed · logged</p>
      </Card>,
    );

    expect(screen.getByText("Net closed · logged")).toBeInTheDocument();
  });

  it("applies the shared surface/border/rounded chrome", () => {
    render(
      <Card>
        <p data-testid="card-content">content</p>
      </Card>,
    );

    const card = screen.getByTestId("card-content").parentElement;
    expect(card).toHaveStyle({
      background: "var(--surface)",
      borderRadius: "var(--rounded-xl)",
    });
    expect(card?.style.border).toBe("1px solid var(--border)");
  });

  it("merges caller-supplied style overrides without dropping the base chrome", () => {
    render(
      <Card style={{ padding: "24px", maxWidth: "440px" }}>
        <p data-testid="card-content">content</p>
      </Card>,
    );

    const card = screen.getByTestId("card-content").parentElement;
    expect(card).toHaveStyle({
      background: "var(--surface)",
      padding: "24px",
      maxWidth: "440px",
    });
  });

  it("has no accessibility violations", async () => {
    const { container } = render(
      <Card>
        <p>content</p>
      </Card>,
    );
    await expectNoAxeViolations(container);
  });
});
