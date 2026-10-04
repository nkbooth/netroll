// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
/**
 * Pure responsive breakpoint mapping. No DOM — trivially testable,
 * the single source of truth the `ResponsiveList` component reads. For the
 * MVP list mode: below the desktop breakpoint the roster stacks into cards,
 * at/above it lays out in rows; only an operator-width viewport (≥1024) goes
 * compact density, everything below that stays roomy.
 */

/** At/above this width lists lay out in rows; below, stacked cards. */
export const BREAKPOINT_DESKTOP = 640;

/** At/above this width the viewport is operator-available: compact density. */
export const BREAKPOINT_OPERATOR = 1024;

/** Smallest supported phone width (first-class mobile target). */
export const BREAKPOINT_PHONE_FLOOR = 340;

/** List presentation mode selected by viewport width. */
export type LayoutMode = "row" | "stacked-card";

/** Vertical/spacing density selected by viewport width. */
export type LayoutDensity = "roomy" | "compact";

/** The layout decision for a given width. */
export interface LayoutInfo {
  readonly mode: LayoutMode;
  readonly density: LayoutDensity;
}

/** Resolve the row/stacked mode and density for a viewport width. */
export function layoutModeForWidth(width: number): LayoutInfo {
  return {
    mode: width >= BREAKPOINT_DESKTOP ? "row" : "stacked-card",
    density: width >= BREAKPOINT_OPERATOR ? "compact" : "roomy",
  };
}
