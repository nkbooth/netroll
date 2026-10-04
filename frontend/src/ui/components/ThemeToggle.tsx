// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
import { useSyncExternalStore } from "react";
import type { CSSProperties, ReactElement } from "react";

import type { Theme } from "../tokens/tokens";
import { announce } from "../a11y/LiveRegion";
import { currentTheme, setTheme, subscribeToTheme } from "../theme";
import { tokens } from "../tokens/tokens";

// Track geometry from the locked mockup's `.themeswitch`.
const TRACK_WIDTH = 52;
const TRACK_HEIGHT = 28;
const KNOB_SIZE = 22;
const KNOB_INSET = (TRACK_HEIGHT - KNOB_SIZE) / 2;

// The switch itself is bare: no border, no fill — the track carries the
// affordance, so the button chrome would only add a second box around it.
const switchStyle: CSSProperties = {
  display: "inline-flex",
  alignItems: "center",
  gap: "var(--space-2)",
  padding: 0,
  border: "none",
  background: "transparent",
  color: "var(--text-muted)",
  fontFamily: "inherit",
  fontSize: tokens.typography.meta.fontSize,
  fontWeight: 700,
  cursor: "pointer",
};

// rounded.full is rationed to exactly ConnectionStatus + this toggle
// (DESIGN.md Shapes).
const trackStyle: CSSProperties = {
  position: "relative",
  flex: "0 0 auto",
  width: `${TRACK_WIDTH}px`,
  height: `${TRACK_HEIGHT}px`,
  borderRadius: tokens.rounded.full,
  background: "var(--toggle-track)",
  border: "1px solid var(--border)",
};

const knobStyle: CSSProperties = {
  position: "absolute",
  top: `${KNOB_INSET}px`,
  width: `${KNOB_SIZE}px`,
  height: `${KNOB_SIZE}px`,
  borderRadius: tokens.rounded.full,
  background: "var(--knob)",
  display: "flex",
  alignItems: "center",
  justifyContent: "center",
  transition: "background var(--motion-fast) var(--motion-ease)",
};

// The active theme's name, in primary ink against the muted "Theme" caption.
const currentStyle: CSSProperties = {
  color: "var(--text)",
  fontWeight: 800,
};

// Shown when persistence fails — polite, not the app's problem-slug voice.
const PERSIST_FAILED_ANNOUNCEMENT = "Theme won't be remembered next visit.";

const MoonIcon = (): ReactElement => (
  <svg
    data-testid="theme-icon"
    data-icon="moon"
    aria-hidden="true"
    width="13"
    height="13"
    viewBox="0 0 24 24"
    fill="currentColor"
  >
    <path d="M21 12.79A9 9 0 1 1 11.21 3 7 7 0 0 0 21 12.79z" />
  </svg>
);

const SunIcon = (): ReactElement => (
  <svg
    data-testid="theme-icon"
    data-icon="sun"
    aria-hidden="true"
    width="13"
    height="13"
    viewBox="0 0 24 24"
    fill="none"
    stroke="currentColor"
    strokeWidth="2"
    strokeLinecap="round"
  >
    <circle cx="12" cy="12" r="4" />
    <path d="M12 2v2M12 20v2M4.9 4.9l1.4 1.4M17.7 17.7l1.4 1.4M2 12h2M20 12h2M4.9 19.1l1.4-1.4M17.7 6.3l1.4-1.4" />
  </svg>
);

/**
 * Track-and-knob theme switch: pressed = dark active. Sighted users get three
 * redundant cues to AT's one `aria-pressed` — the knob's SIDE of the track,
 * the icon (moon when dark, sun when light), and the theme's name in text —
 * so the state never rests on color alone. The accessible label
 * stays constant ("Dark mode") per ARIA APG, which is why it is set
 * explicitly: the visible label names the active theme instead, and would
 * otherwise rename the control on every flip. State lives in the DOM
 * attribute + storage via the theme manager; subscribing keeps every instance
 * (and other tabs) in sync. A blocked persistence is announced politely
 * rather than swallowed.
 */
export function ThemeToggle(): ReactElement {
  const theme = useSyncExternalStore(subscribeToTheme, currentTheme);
  const pressed = theme === "dark";

  const onToggle = (): void => {
    const next: Theme = pressed ? "light" : "dark";
    if (setTheme(next)) {
      // Clear any stale failure announcement from an earlier blocked
      // persist — otherwise AT querying the region later hears about a
      // failure that no longer applies.
      announce("");
    } else {
      announce(PERSIST_FAILED_ANNOUNCEMENT);
    }
  };

  return (
    <button
      type="button"
      aria-pressed={pressed}
      aria-label="Dark mode"
      onClick={onToggle}
      style={switchStyle}
    >
      <span>Theme</span>
      <span aria-hidden="true" style={trackStyle}>
        <span
          data-testid="theme-knob"
          style={{
            ...knobStyle,
            // Exactly one edge is pinned, so the knob's position is itself a
            // state cue rather than a recolor.
            ...(pressed
              ? { right: `${KNOB_INSET}px` }
              : { left: `${KNOB_INSET}px` }),
            // The knob's ink has to survive its own fill: accent-filled on
            // dark, plain white on light.
            color: pressed ? "var(--bg)" : "var(--accent-ink)",
            boxShadow: pressed ? "none" : "0 1px 3px rgba(20,40,60,.25)",
          }}
        >
          {pressed ? <MoonIcon /> : <SunIcon />}
        </span>
      </span>
      <span data-testid="theme-current" style={currentStyle}>
        {pressed ? "Dark" : "Light"}
      </span>
    </button>
  );
}
