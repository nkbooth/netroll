// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
import type { CSSProperties, ReactElement } from "react";

/**
 * The NetRoll logomark: a concentric radio-signal arc set in a cyan rounded
 * square with a soft accent halo (DESIGN.md § Brand & Style). Decorative by
 * contract — it always sits beside the "NetRoll" wordmark, which carries the
 * accessible name.
 */

export interface LogomarkProps {
  /** Edge length of the rounded square, in px. Defaults to the 30px used by
   * the app-shell chrome; larger marks (auth card, discovery hero) pass their
   * own. */
  readonly size?: number;
  readonly style?: CSSProperties;
}

/** The shell-chrome size from the locked mockup's `.wordmark .glyph`. */
const DEFAULT_SIZE = 30;

/** Glyph-to-square ratio from the same mockup (18px glyph in a 30px square). */
const GLYPH_RATIO = 0.6;

/** Corner radius relative to the square, keeping the mark soft-cornered
 * rather than pill-shaped at every size (DESIGN.md § Shapes). */
const RADIUS_RATIO = 0.27;

export function Logomark({ size = DEFAULT_SIZE, style }: LogomarkProps): ReactElement {
  const glyphSize = Math.round(size * GLYPH_RATIO);
  const markStyle: CSSProperties = {
    display: "inline-flex",
    alignItems: "center",
    justifyContent: "center",
    flex: "0 0 auto",
    width: `${size}px`,
    height: `${size}px`,
    borderRadius: `${Math.round(size * RADIUS_RATIO)}px`,
    background: "var(--accent)",
    // The only glow in the system besides the live dot; color-mix keeps it
    // welded to whichever accent the active theme resolved.
    boxShadow: `0 0 ${Math.round(size * 0.47)}px color-mix(in srgb, var(--accent) 45%, transparent)`,
    ...style,
  };

  return (
    <span aria-hidden="true" data-testid="logomark" style={markStyle}>
      <svg
        viewBox="0 0 24 24"
        width={glyphSize}
        height={glyphSize}
        fill="none"
        stroke="var(--bg)"
        strokeWidth={2.4}
      >
        <path d="M5 12a7 7 0 0 1 14 0M8.5 12a3.5 3.5 0 0 1 7 0" />
        <circle cx="12" cy="12" r="1.4" fill="var(--bg)" stroke="none" />
      </svg>
    </span>
  );
}
