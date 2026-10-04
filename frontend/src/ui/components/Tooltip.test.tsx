// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it } from "vitest";

import { Tooltip } from "./Tooltip";

describe("Tooltip", () => {
  it("reveals on hover and associates via aria-describedby", async () => {
    const user = userEvent.setup();
    render(
      <Tooltip content="How check-ins work">
        <button type="button">i</button>
      </Tooltip>,
    );

    expect(screen.queryByRole("tooltip")).not.toBeInTheDocument();

    await user.hover(screen.getByRole("button"));

    const tip = screen.getByRole("tooltip");
    expect(tip).toBeInTheDocument();
    expect(screen.getByRole("button")).toHaveAttribute(
      "aria-describedby",
      tip.id,
    );
  });

  it("reveals on keyboard focus of the trigger", async () => {
    const user = userEvent.setup();
    render(
      <Tooltip content="How check-ins work">
        <button type="button">i</button>
      </Tooltip>,
    );

    await user.tab();
    expect(screen.getByRole("button")).toHaveFocus();
    expect(screen.getByRole("tooltip")).toBeInTheDocument();
  });

  it("reveals on focus-within when a nested control is focused", async () => {
    const user = userEvent.setup();
    render(
      <Tooltip content="How check-ins work">
        <span>
          help <button type="button">?</button>
        </span>
      </Tooltip>,
    );

    // Focus lands on the nested button — a descendant, not the wrapper —
    // exercising focus-within reach (keyboard help must be reachable).
    await user.tab();
    expect(screen.getByRole("button")).toHaveFocus();
    expect(screen.getByRole("tooltip")).toBeInTheDocument();
  });
});
