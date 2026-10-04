// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
import type { CSSProperties, ReactElement } from "react";

import { tokens } from "../../ui/tokens/tokens";
import { ROSTER_CELL_BASIS } from "./RosterEntry";

/**
 * The uppercase column-head strip above a roster (the mock's `.rhead`). Which
 * heads render is driven by the same flags the surface passes to its
 * `RosterEntry` rows, so the strip never labels a column the rows omit — and,
 * equally, never omits a label for a column the rows show. Widening the
 * observer roster added the
 * precedence cell to its rows, so `showPrecedence` had to reach this strip in
 * the same change. It lays out on `ROSTER_CELL_BASIS` — the row cells' own
 * widths.
 *
 * Deliberately presentational: the roster is a list of `<li>` rows, not a
 * table, so promising `columnheader` semantics here would describe a grid
 * structure assistive tech would then fail to find.
 */

export interface RosterColumnHeadsProps {
  /** The roster ordinal column (rendered when rows carry a `position`). */
  readonly showPosition?: boolean;
  /** The Self/Staff provenance column. */
  readonly showBadge?: boolean;
  /** The signal-report column. */
  readonly showReport?: boolean;
  /**
   * The precedence column — the chip and, beside it, the labelled traffic count.
   *
   * `PublicLiveSessionPage` passes `showPrecedence` to this strip and to its
   * rows together.
   *
   * Still true, and it is this component's whole contract: the strip labels
   * exactly the columns its rows render, so this flag is passed to BOTH or to
   * neither. The head reads "Precedence" while the cell under it carries a chip
   * AND a count; the count carries its own visible "traffic" label on the row
   * rather than borrowing this one.
   */
  readonly showPrecedence?: boolean;
  /** Label the trailing time column "Checked" (a closed session's absolute
   * clock time) instead of "Heard" (the live relative phrase). */
  readonly heardAbsolute?: boolean;
}

const stripStyle: CSSProperties = {
  display: "flex",
  alignItems: "baseline",
  gap: "var(--space-3)",
  padding: "var(--space-2) 0",
  borderBottom: "1px solid var(--border)",
  fontSize: tokens.typography.labelCaps.fontSize,
  fontWeight: tokens.typography.labelCaps.fontWeight,
  letterSpacing: tokens.typography.labelCaps.letterSpacing,
  textTransform: "uppercase",
  color: "var(--text-muted)",
  flexWrap: "wrap",
};

export function RosterColumnHeads({
  showPosition = false,
  showBadge = false,
  showReport = false,
  showPrecedence = false,
  heardAbsolute = false,
}: RosterColumnHeadsProps): ReactElement {
  return (
    <div aria-hidden="true" data-testid="roster-column-heads" style={stripStyle}>
      {showPosition && (
        <span style={{ flex: ROSTER_CELL_BASIS.position, textAlign: "center" }}>
          #
        </span>
      )}
      <span style={{ flex: ROSTER_CELL_BASIS.info, minWidth: 0 }}>Station</span>
      {showBadge && <span style={{ flex: ROSTER_CELL_BASIS.badge }}>Source</span>}
      {showReport && <span style={{ flex: ROSTER_CELL_BASIS.report }}>Report</span>}
      {showPrecedence && (
        <span style={{ flex: ROSTER_CELL_BASIS.precedence }}>Precedence</span>
      )}
      <span style={{ flex: ROSTER_CELL_BASIS.heard, textAlign: "right" }}>
        {heardAbsolute ? "Checked" : "Heard"}
      </span>
    </div>
  );
}
