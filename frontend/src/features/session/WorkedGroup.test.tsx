// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
import { render, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it } from "vitest";

import { WorkedGroup } from "./WorkedGroup";
import { expectNoAxeViolations } from "../../test/axe";

function renderGroup(count = 3) {
  return render(
    <ul aria-label="Roster">
      <WorkedGroup count={count}>
        {Array.from({ length: count }, (_, i) => (
          <li key={i} data-worked-row>
            <span className="mono">{`W1${String.fromCharCode(65 + i).repeat(3)}`}</span>
          </li>
        ))}
      </WorkedGroup>
    </ul>,
  );
}

describe("WorkedGroup", () => {
  it("collapses the worked stations behind a live count", () => {
    renderGroup(7);
    const disclosure = screen.getByRole("button", { expanded: false });
    // The COUNT is the datum, not the phrasing.
    expect(disclosure.textContent).toMatch(/\b7\b/);
    expect(document.querySelectorAll("[data-worked-row]")).toHaveLength(0);
  });

  it("expands to reveal exactly the worked rows and collapses again", async () => {
    const user = userEvent.setup();
    renderGroup(3);
    await user.click(screen.getByRole("button", { expanded: false }));
    expect(document.querySelectorAll("[data-worked-row]")).toHaveLength(3);
    await user.click(screen.getByRole("button", { expanded: true }));
    expect(document.querySelectorAll("[data-worked-row]")).toHaveLength(0);
  });

  it("wires aria-expanded and aria-controls at the group container", async () => {
    const user = userEvent.setup();
    renderGroup(2);
    const disclosure = screen.getByRole("button");
    const controls = disclosure.getAttribute("aria-controls");
    expect(controls).toBeTruthy();
    const group = document.getElementById(controls as string);
    expect(group).not.toBeNull();
    expect(disclosure).toHaveAttribute("aria-expanded", "false");
    await user.click(disclosure);
    expect(disclosure).toHaveAttribute("aria-expanded", "true");
    expect(within(group as HTMLElement).getAllByRole("listitem")).toHaveLength(2);
  });

  it("is operable by keyboard alone", async () => {
    const user = userEvent.setup();
    renderGroup(2);
    await user.tab();
    expect(screen.getByRole("button")).toHaveFocus();
    await user.keyboard("{Enter}");
    expect(document.querySelectorAll("[data-worked-row]")).toHaveLength(2);
    await user.keyboard(" ");
    expect(document.querySelectorAll("[data-worked-row]")).toHaveLength(0);
  });

  it("has no WCAG 2.1 AA violations collapsed or expanded", async () => {
    const user = userEvent.setup();
    const { container } = renderGroup(3);
    await expectNoAxeViolations(container);
    await user.click(screen.getByRole("button"));
    await expectNoAxeViolations(container);
  });
});
