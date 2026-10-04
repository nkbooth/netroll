// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
import { tokens } from "./tokens";

/** Duration tokens from the motion budget (`ease` is not a duration). */
export type MotionDurationKey = Exclude<keyof typeof tokens.motion, "ease">;

/**
 * Every motion-budget duration collapses to this under reduced motion.
 * tokens.css's `prefers-reduced-motion` override must use the identical
 * representation — a drift test (tokens-css.test.ts) welds the two.
 */
export const REDUCED_MOTION_DURATION = "0s";

/** True when the user has asked the OS for reduced motion. */
export function prefersReducedMotion(): boolean {
  return window.matchMedia("(prefers-reduced-motion: reduce)").matches;
}

/**
 * Resolve a motion-budget duration, honoring `prefers-reduced-motion`:
 * the DESIGN.md value normally, instant (`0s`) under reduced motion.
 */
export function motionDuration(name: MotionDurationKey): string {
  return prefersReducedMotion() ? REDUCED_MOTION_DURATION : tokens.motion[name];
}
