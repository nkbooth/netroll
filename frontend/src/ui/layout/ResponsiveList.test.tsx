// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
import { render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it } from "vitest";

import { ResponsiveList } from "./ResponsiveList";

const setWidth = (width: number): void => {
  Object.defineProperty(window, "innerWidth", {
    configurable: true,
    writable: true,
    value: width,
  });
};

afterEach(() => {
  setWidth(1024);
});

describe("ResponsiveList", () => {
  it("stacks into cards below the desktop breakpoint", () => {
    setWidth(400);

    render(
      <ResponsiveList>
        <div>row</div>
      </ResponsiveList>,
    );

    expect(screen.getByTestId("responsive-list")).toHaveAttribute(
      "data-layout-mode",
      "stacked-card",
    );
  });

  it("lays out rows at or above the desktop breakpoint", () => {
    setWidth(900);

    render(
      <ResponsiveList>
        <div>row</div>
      </ResponsiveList>,
    );

    expect(screen.getByTestId("responsive-list")).toHaveAttribute(
      "data-layout-mode",
      "row",
    );
  });

  it("scrolls wide content inside its own container, never the page body", () => {
    render(
      <ResponsiveList>
        <div>row</div>
      </ResponsiveList>,
    );

    const container = screen.getByTestId("responsive-list");
    expect(container).toHaveStyle({ overflowX: "auto", maxWidth: "100%" });
  });
});
