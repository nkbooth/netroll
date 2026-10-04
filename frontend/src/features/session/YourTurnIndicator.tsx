// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
import type { CSSProperties, ReactElement, ReactNode } from "react";

import { tokens } from "../../ui/tokens/tokens";

/**
 * The "your turn" indicator (DESIGN.md:197-203). Rendered on
 * the viewer's OWN roster row when it is the next station the NCS will work — the
 * newcomer's "get ready to transmit" cue. A solid cyan "You" chip + a
 * "You're next up" note in `--accent-ink`; the row-level 4px cyan left-bar and
 * cyan-9% wash are applied by `RosterEntry` (the same split `WorkingCursor` uses).
 *
 * color + icon + label: the state is never cyan ALONE — a filled dot
 * icon and the "You" / "You're next up" text carry it too. Cyan (`--accent`/
 * `--accent-ink`) is token-driven, so it re-resolves in both light and dark
 * themes. There is NO animation, so the `prefers-reduced-motion` budget
 * is honored by construction — the appearance is instant.
 *
 * A `role="status"` live region so a screen reader announces "You're next up" the
 * moment the cursor advances to make this the viewer's turn.
 */

const wrapStyle: CSSProperties = {
  display: "inline-flex",
  alignItems: "center",
  gap: "var(--space-2)",
};

const chipStyle: CSSProperties = {
  display: "inline-flex",
  alignItems: "center",
  gap: "var(--space-1)",
  padding: "0 var(--space-2)",
  borderRadius: "var(--rounded-full)",
  background: "var(--accent)",
  color: "var(--accent-contrast)",
  fontSize: tokens.typography.microCaps.fontSize,
  fontWeight: tokens.typography.microCaps.fontWeight,
  letterSpacing: tokens.typography.microCaps.letterSpacing,
  textTransform: "uppercase",
};

const noteStyle: CSSProperties = {
  color: "var(--accent-ink)",
  fontWeight: 700,
  fontSize: tokens.typography.meta.fontSize,
};

/** A small filled circle — the "your turn" affordance icon. */
const dotIcon: ReactNode = (
  <svg width="8" height="8" viewBox="0 0 8 8" fill="currentColor" aria-hidden="true">
    <circle cx="4" cy="4" r="3.5" />
  </svg>
);

/** The cyan "You" chip + "You're next up" note shown on the viewer's next-up row. */
export function YourTurnIndicator(): ReactElement {
  return (
    <span data-your-turn="true" role="status" aria-label="You're next up" style={wrapStyle}>
      <span style={chipStyle}>
        {dotIcon}
        You
      </span>
      <span aria-hidden="true" style={noteStyle}>
        You&rsquo;re next up
      </span>
    </span>
  );
}
