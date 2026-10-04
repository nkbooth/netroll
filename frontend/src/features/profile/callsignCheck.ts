// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
/**
 * Client-side callsign plausibility pre-check. UX only — mirrors
 * the domain grammar in spirit for fast feedback, never authoritative. The
 * server's `parse_callsign` is the only source of truth; this never blocks
 * a submission the server would accept, and never accepts one the server
 * would reject without the user finding out via the response.
 */

/**
 * Loosely judges whether `input` could be a callsign: non-empty once
 * trimmed, only letters/digits/`/`, and at least one digit (every real
 * callsign grammar separates a prefix and suffix with one).
 */
export function isPlausibleCallsign(input: string): boolean {
  const trimmed = input.trim();
  if (trimmed.length === 0) {
    return false;
  }
  if (!/^[A-Za-z0-9/]+$/.test(trimmed)) {
    return false;
  }
  return /\d/.test(trimmed);
}
