// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
import { useId } from "react";
import type { CSSProperties, ReactElement, ReactNode } from "react";

import { tokens } from "../tokens/tokens";

/**
 * The framed panel from the locked mockups: a surface-toned card with the
 * theme's elevation, an optional tonal header band, and an optional
 * column-head strip above caller-owned rows. Depth is tonal layering first,
 * shadow second (DESIGN.md § Elevation & Depth).
 */

/** Heading levels a panel title may occupy; pages own their outline. */
export type PanelTitleLevel = 2 | 3;

export interface PanelProps {
  readonly children: ReactNode;
  /** Section caption for the header band. Omit for an unbanded frame. */
  readonly title?: string;
  /** Trailing content in the header band — a count, a status pill, an action. */
  readonly headerAside?: ReactNode;
  readonly titleLevel?: PanelTitleLevel;
  readonly style?: CSSProperties;
}

const panelStyle: CSSProperties = {
  background: "var(--surface)",
  border: "1px solid var(--border)",
  borderRadius: "var(--rounded-xl)",
  boxShadow: "var(--shadow)",
  // Rows run full-bleed to the panel edge; without clipping they would square
  // off the rounded corners.
  overflow: "hidden",
};

const headerStyle: CSSProperties = {
  display: "flex",
  alignItems: "center",
  justifyContent: "space-between",
  gap: "var(--space-3)",
  padding: "var(--space-3) var(--space-row-x)",
  background: "var(--head-grad)",
  borderBottom: "1px solid var(--border)",
};

const titleStyle: CSSProperties = {
  margin: 0,
  fontSize: tokens.typography.labelCaps.fontSize,
  fontWeight: tokens.typography.labelCaps.fontWeight,
  letterSpacing: tokens.typography.labelCaps.letterSpacing,
  textTransform: "uppercase",
  color: "var(--text-muted)",
};

export function Panel({
  children,
  title,
  headerAside,
  titleLevel = 2,
  style,
}: PanelProps): ReactElement {
  const titleId = useId();
  const Heading = `h${titleLevel}` as "h2" | "h3";

  return (
    <section
      style={{ ...panelStyle, ...style }}
      {...(title === undefined
        ? {}
        : { role: "region", "aria-labelledby": titleId })}
    >
      {title !== undefined && (
        <div style={headerStyle}>
          <Heading id={titleId} style={titleStyle}>
            {title}
          </Heading>
          {headerAside}
        </div>
      )}
      {children}
    </section>
  );
}

/** A column-head label; the object form opts into trailing alignment for
 * numeric/time columns that read better right-aligned. */
export type PanelColumnLabel =
  | string
  | { readonly label: string; readonly align: "start" | "end" };

export interface PanelColumnHeadsProps {
  /** `grid-template-columns` shared with the rows below it. */
  readonly template: string;
  readonly labels: readonly PanelColumnLabel[];
}

const columnHeadsStyle: CSSProperties = {
  display: "grid",
  gap: "var(--space-gutter)",
  alignItems: "center",
  padding: "var(--space-2) var(--space-row-x)",
  background: "var(--surface-2)",
  borderBottom: "1px solid var(--border)",
  fontSize: tokens.typography.labelCaps.fontSize,
  fontWeight: tokens.typography.labelCaps.fontWeight,
  letterSpacing: tokens.typography.labelCaps.letterSpacing,
  textTransform: "uppercase",
  color: "var(--text-muted)",
};

/**
 * The uppercase column-head strip above a panel's rows. Deliberately
 * presentational — the rows beneath are cards/list items, so table semantics
 * here would promise a grid structure that does not exist.
 */
export function PanelColumnHeads({
  template,
  labels,
}: PanelColumnHeadsProps): ReactElement {
  return (
    <div
      aria-hidden="true"
      data-testid="panel-column-heads"
      style={{ ...columnHeadsStyle, gridTemplateColumns: template }}
    >
      {labels.map((entry) => {
        const label = typeof entry === "string" ? entry : entry.label;
        const align = typeof entry === "string" ? "start" : entry.align;
        return (
          <span
            key={label}
            style={{ textAlign: align === "end" ? "right" : "left" }}
          >
            {label}
          </span>
        );
      })}
    </div>
  );
}
