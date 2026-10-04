// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
/**
 * Pure exponential backoff with jitter for reconnect attempts. Kept
 * side-effect-free and RNG-injectable so it is deterministically unit-testable.
 *
 * The exponential CEILING doubles from a base and clamps at a cap; the returned
 * delay applies EQUAL JITTER within `[ceiling/2, ceiling]` — bounded jitter that
 * still spreads retries across clients (thundering-herd avoidance) while never
 * collapsing to an immediate hammering retry.
 */

/** The first-attempt ceiling in milliseconds. */
export const BACKOFF_BASE_MS = 500;

/** The ceiling clamp — reconnect never waits longer than this (~10–30s). */
export const BACKOFF_CAP_MS = 15_000;

/**
 * The exponential ceiling for `attempt` (0-indexed): `base * 2^attempt`, clamped
 * to the cap. Monotonically non-decreasing, reaching `BACKOFF_CAP_MS`.
 */
export function backoffCeiling(attempt: number): number {
  const raw = BACKOFF_BASE_MS * 2 ** attempt;
  return Math.min(BACKOFF_CAP_MS, raw);
}

/**
 * The next backoff delay in milliseconds for `attempt`, with equal jitter in
 * `[ceiling/2, ceiling]`. `rng` defaults to `Math.random`; inject a fixed value
 * for deterministic tests.
 */
export function nextBackoff(attempt: number, rng: () => number = Math.random): number {
  const ceiling = backoffCeiling(attempt);
  const half = ceiling / 2;
  return half + rng() * half;
}
