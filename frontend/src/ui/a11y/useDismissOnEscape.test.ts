// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
import { fireEvent, renderHook } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";

import { useDismissOnEscape } from "./useDismissOnEscape";

describe("useDismissOnEscape", () => {
  it("invokes the dismiss callback on Escape", () => {
    const onDismiss = vi.fn();
    renderHook(() => useDismissOnEscape(onDismiss));

    fireEvent.keyDown(document, { key: "Escape" });

    expect(onDismiss).toHaveBeenCalledTimes(1);
  });

  it("ignores non-Escape keys", () => {
    const onDismiss = vi.fn();
    renderHook(() => useDismissOnEscape(onDismiss));

    fireEvent.keyDown(document, { key: "Enter" });

    expect(onDismiss).not.toHaveBeenCalled();
  });

  it("only the topmost (most-recent) registration fires", () => {
    const lower = vi.fn();
    const upper = vi.fn();
    renderHook(() => useDismissOnEscape(lower));
    const top = renderHook(() => useDismissOnEscape(upper));

    fireEvent.keyDown(document, { key: "Escape" });
    expect(upper).toHaveBeenCalledTimes(1);
    expect(lower).not.toHaveBeenCalled();

    // Once the top unmounts, the next registration becomes topmost.
    top.unmount();
    fireEvent.keyDown(document, { key: "Escape" });
    expect(lower).toHaveBeenCalledTimes(1);
  });
});
