// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
import type { CSSProperties, ReactElement } from "react";

import { tokens } from "../tokens/tokens";

/**
 * A single numeral+label stat tile (post-net summary's Check-ins / Traffic
 * passed / Duration / States-provinces row, and any future stat-tile row).
 */

export interface StatTileProps {
  /** The large numeral (or mono duration/time string) to display. */
  readonly value: string;
  /** The small uppercase label under the value. */
  readonly label: string;
  /** Render the value in the radio-data mono face (elapsed/duration values). */
  readonly mono?: boolean;
  /** Denser phone-width presentation. */
  readonly compact?: boolean;
}

const tileStyle: CSSProperties = {
  flex: "1 1 120px",
  background: "var(--surface-2)",
  border: "1px solid var(--border)",
  borderRadius: "var(--rounded-lg)",
  padding: "13px 15px",
};

const compactTileStyle: CSSProperties = {
  ...tileStyle,
  flex: "1",
  padding: "10px",
};

const valueStyle: CSSProperties = {
  fontSize: "26px",
  fontWeight: 800,
  letterSpacing: "-0.02em",
};

const compactValueStyle: CSSProperties = {
  ...valueStyle,
  fontSize: "20px",
};

const labelStyle: CSSProperties = {
  fontSize: tokens.typography.labelCaps.fontSize,
  fontWeight: tokens.typography.labelCaps.fontWeight,
  textTransform: "uppercase",
  letterSpacing: tokens.typography.labelCaps.letterSpacing,
  color: "var(--text-muted)",
  marginTop: "2px",
};

const compactLabelStyle: CSSProperties = {
  ...labelStyle,
  fontSize: tokens.typography.microCaps.fontSize,
  letterSpacing: tokens.typography.microCaps.letterSpacing,
};

/** Numeral + uppercase label stat card, e.g. post-net summary's stat row. */
export function StatTile({
  value,
  label,
  mono = false,
  compact = false,
}: StatTileProps): ReactElement {
  return (
    <div style={compact ? compactTileStyle : tileStyle}>
      <div
        style={{
          ...(compact ? compactValueStyle : valueStyle),
          ...(mono ? { fontFamily: tokens.typography.mono.fontFamily } : {}),
        }}
      >
        {value}
      </div>
      <div style={compact ? compactLabelStyle : labelStyle}>{label}</div>
    </div>
  );
}
