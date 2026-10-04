// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
import type { CSSProperties, ReactElement } from "react";

import { Tooltip } from "../../ui/components/Tooltip";
import { tokens } from "../../ui/tokens/tokens";

/**
 * The operator keycap-hint legend. It advertises the
 * two hot-path keys as keycap chips with keyboard-reachable tooltips:
 * - `n` — "Jump to check-in" (neutral keycap; wired by `QuickAddRow`).
 * - `w` — "Set working" (coral keycap). The document-level `w` hotkey in
 * `LiveSessionPage` toggles the working cursor on the selected roster row.
 *
 * This component is just the discoverability chip; the action lives in
 * `LiveSessionPage`/`RosterEntry`. No arrow / j-k / cursor navigation is offered
 * (none in MVP).
 */

const legendStyle: CSSProperties = {
  display: "inline-flex",
  alignItems: "center",
  gap: "var(--space-2)",
};

const keycapStyle: CSSProperties = {
  display: "inline-flex",
  alignItems: "center",
  justifyContent: "center",
  minWidth: "1.4em",
  padding: "1px var(--space-1)",
  background: "var(--surface-2)",
  color: "var(--text-muted)",
  border: "1px solid var(--border)",
  borderBottomWidth: "2px",
  borderRadius: "var(--rounded-keycap)",
  fontFamily: tokens.typography.mono.fontFamily,
  fontSize: tokens.typography.keycap.fontSize,
  fontWeight: tokens.typography.keycap.fontWeight,
  cursor: "default",
};

const coralKeycapStyle: CSSProperties = {
  ...keycapStyle,
  color: "var(--cursor-ink)",
};

interface KeycapProps {
  readonly keyLabel: string;
  readonly hint: string;
  readonly coral?: boolean;
}

/** One keycap chip + its wayfinding tooltip. Focusable for keyboard reveal. */
function Keycap({ keyLabel, hint, coral = false }: KeycapProps): ReactElement {
  return (
    <Tooltip content={hint}>
      <kbd tabIndex={0} aria-label={hint} style={coral ? coralKeycapStyle : keycapStyle}>
        {keyLabel}
      </kbd>
    </Tooltip>
  );
}

/** The `n`/`w` keycap-hint legend for the operator console toolbar. */
export function KeycapLegend(): ReactElement {
  return (
    <div role="group" aria-label="Keyboard shortcuts" style={legendStyle}>
      <Keycap keyLabel="n" hint="Jump to check-in" />
      <Keycap keyLabel="w" hint="Set working" coral />
    </div>
  );
}
