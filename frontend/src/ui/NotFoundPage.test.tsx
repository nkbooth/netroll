// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
import { render, screen } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import { RouterProvider, createMemoryRouter } from "react-router";

import { NotFoundPage, RouteError } from "./NotFoundPage";
import { expectNoAxeViolations } from "../test/axe";

function renderWithRouter(element: ReturnType<typeof NotFoundPage>) {
  const router = createMemoryRouter([{ path: "/", element }], {
    initialEntries: ["/"],
  });
  return render(<RouterProvider router={router} />);
}

describe("NotFoundPage", () => {
  it("renders a heading and a keyboard-operable link home", () => {
    renderWithRouter(<NotFoundPage />);

    expect(screen.getByRole("heading", { level: 1 })).toBeInTheDocument();
    expect(screen.getByRole("link")).toHaveAttribute("href", "/");
  });

  it("has no WCAG 2.1 AA violations", async () => {
    const { container } = renderWithRouter(<NotFoundPage />);

    await expectNoAxeViolations(container);
  });
});

describe("RouteError", () => {
  it("renders an error heading and a way home", () => {
    renderWithRouter(<RouteError />);

    expect(screen.getByRole("heading", { level: 1 })).toBeInTheDocument();
    expect(screen.getByRole("link")).toHaveAttribute("href", "/");
  });
});
