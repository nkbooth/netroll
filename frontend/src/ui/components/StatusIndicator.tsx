// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
import type { CSSProperties, ReactElement, ReactNode } from "react";

import { tokens } from "../tokens/tokens";

/**
 * The status-display contract primitive: a status is ALWAYS
 * color + icon + label, never color alone (operators watch on a phone in the
 * dark). This primitive enforces the rule structurally — `icon` and `label`
 * are required, and both always render alongside the tone color. The concrete
 * ConnectionStatus / source-badge / precedence-chip variants that build on it
 * come later; this ships only the enforcing contract.
 */

/** Semantic status tones, each mapped to a token color. */
export type StatusTone =
  | "live"
  | "catching-up"
  | "out-of-sync"
  | "paused"
  | "warn"
  | "neutral"
  | "staff"
  | "self"
  | "staying";

const toneColor: Record<StatusTone, string> = {
  live: "var(--live-text)",
  "catching-up": "var(--catch-text)",
  "out-of-sync": "var(--sync-text)",
  paused: "var(--pause-text)",
  warn: "var(--warn)",
  neutral: "var(--text-muted)",
  // The check-in source badge's two provenances: builds ON
  // this primitive rather than reimplementing color+icon+label from scratch.
  staff: "var(--staff-text)",
  self: "var(--self-text)",
  // The staying-for-comments roster indicator: the shipped
  // --staying green. `in-and-out` uses the `neutral` (muted) tone instead.
  staying: "var(--staying)",
};

export interface StatusIndicatorProps {
  /** Semantic tone — supplies color only; never the sole signal. */
  readonly tone: StatusTone;
  /** Non-color affordance (decorative — the label carries the meaning). */
  readonly icon: ReactNode;
  /** The always-present text label assistive tech reads. */
  readonly label: string;
}

const containerStyle: CSSProperties = {
  display: "inline-flex",
  alignItems: "center",
  gap: "var(--space-1)",
  fontSize: tokens.typography.labelCaps.fontSize,
  fontWeight: tokens.typography.labelCaps.fontWeight,
  letterSpacing: tokens.typography.labelCaps.letterSpacing,
};

/** Color + icon + label status pill. All three signals always render. */
export function StatusIndicator({
  tone,
  icon,
  label,
}: StatusIndicatorProps): ReactElement {
  return (
    <span style={{ ...containerStyle, color: toneColor[tone] }}>
      <span aria-hidden="true" style={{ display: "inline-flex" }}>
        {icon}
      </span>
      <span>{label}</span>
    </span>
  );
}
