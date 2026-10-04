// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
import type { CSSProperties, ReactElement } from "react";

import { tokens } from "../../ui/tokens/tokens";

/**
 * The working-station ornament (DESIGN.md:366). On the row that
 * currently holds the working cursor it shows an animated coral wave glyph + a
 * "Working now" flag in `var(--cursor-ink)`; on every operator row it also renders
 * the inline coral `w` set-working control (the `KeycapLegend` coral-keycap
 * style) that moves the cursor to — or, when already working, clears — this row.
 *
 * color + icon + label: the state is never coral ALONE — the wave icon
 * and the "Working now" text carry it too. The wave glyph is tagged
 * `.cursor-wash` so the shipped `prefers-reduced-motion` rule (tokens.css) can
 * neutralize its animation while the color/icon/label switch stays instant.
 *
 * Coral (`--cursor`/`--cursor-ink`) appears ONLY here and on the `w` keycap —
 * never elsewhere (DESIGN.md:294/311).
 */

const wrapStyle: CSSProperties = {
  display: "inline-flex",
  alignItems: "center",
  gap: "var(--space-2)",
};

const flagStyle: CSSProperties = {
  display: "inline-flex",
  alignItems: "center",
  gap: "var(--space-1)",
  color: "var(--cursor-ink)",
  fontWeight: 700,
  fontSize: tokens.typography.meta.fontSize,
};

/** The coral-variant `w` keycap — the `KeycapLegend` coral-keycap style. */
const keycapStyle: CSSProperties = {
  display: "inline-flex",
  alignItems: "center",
  justifyContent: "center",
  minWidth: "1.4em",
  padding: "1px var(--space-1)",
  background: "var(--surface-2)",
  color: "var(--cursor-ink)",
  border: "1px solid var(--cursor)",
  borderBottomWidth: "2px",
  borderRadius: "var(--rounded-keycap)",
  fontFamily: tokens.typography.mono.fontFamily,
  fontSize: tokens.typography.keycap.fontSize,
  fontWeight: tokens.typography.keycap.fontWeight,
  cursor: "pointer",
};

export interface WorkingCursorProps {
  /** Whether this row currently holds the working cursor. */
  readonly working: boolean;
  /**
   * Toggle the working cursor on this row — sets it working, or clears it
   * (completes the station) when it is already working. The parent decides
   * which by inspecting `working`, so this is a single toggle callback.
   */
  readonly onSetWorking: () => void;
}

/** The working-cursor flag + inline `w` set-working control. */
export function WorkingCursor({ working, onSetWorking }: WorkingCursorProps): ReactElement {
  return (
    <span data-working={String(working)} style={wrapStyle}>
      {working && (
        <span style={flagStyle}>
          {/* The animated coral wave glyph (icon). `.cursor-wash` hooks the
              shipped reduced-motion rule; aria-hidden — the label carries meaning. */}
          <span className="cursor-wash" aria-hidden="true" data-wave>
            &#8767;
          </span>
          Working now
        </span>
      )}
      <button
        type="button"
        data-set-working
        aria-label={working ? "Clear working station" : "Set as working station"}
        style={keycapStyle}
        onClick={onSetWorking}
      >
        w
      </button>
    </span>
  );
}
