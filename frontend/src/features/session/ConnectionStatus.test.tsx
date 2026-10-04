// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
import { render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import { ConnectionStatus, connectionPresentation } from "./ConnectionStatus";
import type { ConnectionState } from "./sessionStore";
import { expectNoAxeViolations } from "../../test/axe";

afterEach(() => {
  vi.unstubAllGlobals();
});

/** Forces `prefers-reduced-motion` to the given result for a single test. */
function stubReducedMotion(reduce: boolean): void {
  vi.stubGlobal(
    "matchMedia",
    (query: string) =>
      ({
        matches: query.includes("reduce") ? reduce : false,
        media: query,
        onchange: null,
        addEventListener: () => {},
        removeEventListener: () => {},
        addListener: () => {},
        removeListener: () => {},
        dispatchEvent: () => false,
      }) as unknown as MediaQueryList,
  );
}

const ALL_STATES: ConnectionState[] = [
  "live",
  "catching-up",
  "out-of-sync",
  "net-paused",
];

describe("connectionPresentation", () => {
  it("maps every connection state to a distinct StatusIndicator tone", () => {
    const tones = ALL_STATES.map((s) => connectionPresentation(s).tone);
    expect(new Set(tones).size).toBe(ALL_STATES.length);
  });

  it("maps net-paused to the shipped paused tone (first-class, not color-only)", () => {
    expect(connectionPresentation("net-paused").tone).toBe("paused");
    expect(connectionPresentation("live").tone).toBe("live");
    expect(connectionPresentation("catching-up").tone).toBe("catching-up");
    expect(connectionPresentation("out-of-sync").tone).toBe("out-of-sync");
  });

  it("gives every state a non-empty text label (never color alone)", () => {
    for (const state of ALL_STATES) {
      expect(connectionPresentation(state).label.length).toBeGreaterThan(0);
    }
  });
});

describe("ConnectionStatus", () => {
  it("renders each state's label as text (color + icon + label contract)", () => {
    for (const state of ALL_STATES) {
      const { unmount } = render(<ConnectionStatus connection={state} />);
      expect(screen.getByText(connectionPresentation(state).label)).toBeInTheDocument();
      unmount();
    }
  });

  it("pulses the live dot when motion is allowed", () => {
    stubReducedMotion(false);
    render(<ConnectionStatus connection="live" />);
    expect(screen.getByTestId("live-dot").getAttribute("data-pulsing")).toBe("true");
  });

  it("drops the pulse under reduced motion but keeps color + icon + label", () => {
    stubReducedMotion(true);
    render(<ConnectionStatus connection="live" />);
    const dot = screen.getByTestId("live-dot");
    expect(dot.getAttribute("data-pulsing")).toBe("false");
    // The label still renders — reduced motion changes animation, not meaning.
    expect(screen.getByText(connectionPresentation("live").label)).toBeInTheDocument();
  });

  it("has no WCAG 2.1 AA violations for each state", async () => {
    for (const state of ALL_STATES) {
      const { container, unmount } = render(<ConnectionStatus connection={state} />);
      await expectNoAxeViolations(container);
      unmount();
    }
  });
});
