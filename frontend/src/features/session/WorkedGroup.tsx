// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
import { useId, useState } from "react";
import type { CSSProperties, ReactElement, ReactNode } from "react";

import { ROSTER_CELL_BASIS } from "./RosterEntry";

/**
 * The collapsed worked-station block at the foot of the roster. Worked
 * stations have already been SUNK by the server — this
 * component neither sorts nor reorders; it only folds the trailing worked run
 * behind a count so the roster shows the NCS who is left.
 *
 * Expansion is a PER-VIEWER view affordance: it appends no event and issues no
 * request, so two operators may disagree about expansion while agreeing about
 * order.
 */

const rowStyle: CSSProperties = { listStyle: "none" };

const disclosureStyle: CSSProperties = {
  display: "flex",
  alignItems: "center",
  gap: "var(--space-2)",
  width: "100%",
  padding: "var(--space-2) 0",
  background: "transparent",
  border: "none",
  borderTop: "1px solid var(--border)",
  color: "var(--text-muted)",
  font: "inherit",
  // The head strip uses a bare 11px literal for the same meta register; there is
  // no type-scale token to reach for here.
  fontSize: "12px",
  cursor: "pointer",
  textAlign: "left",
};

// The leading spacer sits on the roster's own `#` column basis so the group row
// lines up with the head strip and the rows above it.
const spacerStyle: CSSProperties = { flex: ROSTER_CELL_BASIS.position };

const countStyle: CSSProperties = { color: "var(--text)", fontWeight: 700 };

const listStyle: CSSProperties = { listStyle: "none", padding: 0, margin: 0 };

export interface WorkedGroupProps {
  /** The live number of worked entries — the datum the collapsed row reports. */
  readonly count: number;
  /** The already-rendered worked roster rows, revealed only when expanded. */
  readonly children: ReactNode;
}

/** A keyboard-operable disclosure collapsing the worked stations to a count. */
export function WorkedGroup({ count, children }: WorkedGroupProps): ReactElement {
  const [expanded, setExpanded] = useState(false);
  const groupId = useId();

  return (
    <li style={rowStyle}>
      <button
        type="button"
        aria-expanded={expanded}
        aria-controls={groupId}
        onClick={() => setExpanded((prev) => !prev)}
        style={disclosureStyle}
      >
        <span aria-hidden="true" style={spacerStyle} />
        <span>
          Worked <b style={countStyle}>{count}</b>
        </span>
        <span aria-hidden="true">{expanded ? "▾" : "▸"}</span>
      </button>
      {/* The container is always present so `aria-controls` always resolves; the
          rows themselves are unmounted while collapsed — that IS the collapse. */}
      <ul id={groupId} style={listStyle}>
        {expanded && children}
      </ul>
    </li>
  );
}
