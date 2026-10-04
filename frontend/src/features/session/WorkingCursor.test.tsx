// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
import { fireEvent, render, screen } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";

import { WorkingCursor } from "./WorkingCursor";

/**
 * WorkingCursor behavior. Asserts the color+icon+
 * label affordance and that the inline `w` control fires `onSetWorking` — never
 * rendered-style assertions on animation (that is reduced-motion CSS).
 */

describe("WorkingCursor", () => {
  it("renders the 'Working now' flag + wave glyph when the row is working", () => {
    render(<WorkingCursor working onSetWorking={() => {}} />);
    // Label, never colour alone.
    expect(screen.getByText(/working now/i)).toBeInTheDocument();
    // The animated wave glyph is present and hooked into the reduced-motion
    // class so the shipped `.cursor-wash` rule can neutralize it.
    const wave = document.querySelector(".cursor-wash");
    expect(wave).not.toBeNull();
  });

  it("does NOT render the 'Working now' flag when the row is not working", () => {
    render(<WorkingCursor working={false} onSetWorking={() => {}} />);
    expect(screen.queryByText(/working now/i)).toBeNull();
  });

  it("renders an inline `w` set-working control that fires onSetWorking", () => {
    const onSetWorking = vi.fn();
    render(<WorkingCursor working={false} onSetWorking={onSetWorking} />);
    const control = screen.getByRole("button", { name: /working/i });
    // The keycap advertises the `w` key (icon/label parity with the legend).
    expect(control.textContent).toContain("w");
    fireEvent.click(control);
    expect(onSetWorking).toHaveBeenCalledTimes(1);
  });

  it("labels the control 'clear' when the row is already working (toggle-off)", () => {
    const onSetWorking = vi.fn();
    render(<WorkingCursor working onSetWorking={onSetWorking} />);
    // A working row's control clears the cursor (completes the station).
    const control = screen.getByRole("button", { name: /clear|complete|done/i });
    fireEvent.click(control);
    expect(onSetWorking).toHaveBeenCalledTimes(1);
  });
});
