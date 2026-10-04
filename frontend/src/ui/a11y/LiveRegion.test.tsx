// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
import { act, render, renderHook, screen } from "@testing-library/react";
import { beforeEach, describe, expect, it } from "vitest";

import { LiveRegion, announce, useLiveAnnouncer } from "./LiveRegion";

beforeEach(() => {
  // Reset the module singleton between tests.
  act(() => announce(""));
});

describe("LiveRegion", () => {
  it("renders a polite, atomic status region", () => {
    render(<LiveRegion />);

    const region = screen.getByRole("status");
    // Polite (never assertive): roster/connection updates must not interrupt
    // a screen reader mid-read.
    expect(region).toHaveAttribute("aria-live", "polite");
    expect(region).toHaveAttribute("aria-atomic", "true");
  });

  it("places an announced message into the live node", () => {
    render(<LiveRegion />);

    act(() => announce("roster updated"));

    expect(screen.getByRole("status")).toHaveTextContent("roster updated");
  });

  it("announces through the useLiveAnnouncer hook", () => {
    render(<LiveRegion />);
    const { result } = renderHook(() => useLiveAnnouncer());

    act(() => result.current("connection restored"));

    expect(screen.getByRole("status")).toHaveTextContent("connection restored");
  });
});
