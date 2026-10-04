// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";

import { ReplayingState } from "./ReplayingState";
import { expectNoAxeViolations } from "../../test/axe";

describe("ReplayingState", () => {
  it("renders nothing when live", () => {
    const { container } = render(
      <ReplayingState connection="live" onResync={vi.fn()} />,
    );
    expect(container).toBeEmptyDOMElement();
  });

  it("renders nothing for net-paused", () => {
    const { container } = render(
      <ReplayingState connection="net-paused" onResync={vi.fn()} />,
    );
    expect(container).toBeEmptyDOMElement();
  });

  it("shows a status banner without a resync button while catching-up", () => {
    render(<ReplayingState connection="catching-up" onResync={vi.fn()} />);
    expect(screen.getByRole("status")).toBeInTheDocument();
    expect(screen.queryByRole("button")).not.toBeInTheDocument();
  });

  it("shows an alert banner with a resync button when out-of-sync", async () => {
    const onResync = vi.fn();
    const user = userEvent.setup();
    render(<ReplayingState connection="out-of-sync" onResync={onResync} />);

    expect(screen.getByRole("alert")).toBeInTheDocument();
    const button = screen.getByRole("button");
    await user.click(button);
    expect(onResync).toHaveBeenCalledTimes(1);
  });

  it("never prompts a manual page refresh", () => {
    render(<ReplayingState connection="out-of-sync" onResync={vi.fn()} />);
    expect(screen.queryByText(/refresh/i)).not.toBeInTheDocument();
  });

  it("has no WCAG 2.1 AA violations in either non-live state", async () => {
    const { container: a } = render(
      <ReplayingState connection="catching-up" onResync={vi.fn()} />,
    );
    await expectNoAxeViolations(a);
    const { container: b } = render(
      <ReplayingState connection="out-of-sync" onResync={vi.fn()} />,
    );
    await expectNoAxeViolations(b);
  });
});
