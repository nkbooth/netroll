// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
import { render, screen } from "@testing-library/react";
import { describe, expect, it } from "vitest";

import { expectNoAxeViolations } from "../../test/axe";
import { Breadcrumb } from "./Breadcrumb";

describe("Breadcrumb", () => {
  it("is a navigation landmark with an accessible name", () => {
    render(<Breadcrumb items={[{ label: "Nets" }]} />);
    // A named `nav` landmark is the wayfinding anchor.
    expect(
      screen.getByRole("navigation", { name: /breadcrumb/i }),
    ).toBeInTheDocument();
  });

  it("marks only the last crumb as the current page", () => {
    render(
      <Breadcrumb
        items={[
          { label: "Nets", href: "/" },
          { label: "20m · traffic", href: "/x" },
          { label: "Sunday Traffic Net" },
        ]}
      />,
    );

    // The trail's terminal crumb is the current location.
    const current = screen.getByText("Sunday Traffic Net");
    expect(current).toHaveAttribute("aria-current", "page");

    // Earlier crumbs are keyboard-reachable links to their targets; the
    // current crumb is NOT a link.
    const netsLink = screen.getByRole("link", { name: "Nets" });
    expect(netsLink).toHaveAttribute("href", "/");
    expect(
      screen.queryByRole("link", { name: "Sunday Traffic Net" }),
    ).toBeNull();
  });

  it("renders a lone root crumb as the current 'Nets' anchor with no link", () => {
    render(<Breadcrumb items={[{ label: "Nets" }]} />);

    const root = screen.getByText("Nets");
    expect(root).toHaveAttribute("aria-current", "page");
    expect(screen.queryByRole("link")).toBeNull();
  });

  it("has no accessibility violations", async () => {
    const { container } = render(
      <Breadcrumb
        items={[{ label: "Nets", href: "/" }, { label: "Sunday Traffic Net" }]}
      />,
    );
    await expectNoAxeViolations(container);
  });
});
