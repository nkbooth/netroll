// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
import type { CSSProperties, ReactElement, ReactNode } from "react";

import { StatusIndicator } from "../../ui/components/StatusIndicator";
import type { StatusTone } from "../../ui/components/StatusIndicator";
import { tokens } from "../../ui/tokens/tokens";
import type { Precedence } from "./sessionWire";

/**
 * The traffic/emergency precedence chip (DESIGN.md:243-248).
 * Built ON the shipped `StatusIndicator` primitive — the `SourceBadge`-on-
 * `StatusIndicator` pattern — so color + icon + label is enforced structurally
 * (operators watch on a phone in the dark; never color alone). This
 * component adds only the outer chip (fill + border + 6px radius) around it.
 *
 * Three variants:
 * - `routine` → neutral (`--text-muted` / `--surface-2` / `--border`);
 * - `priority` → the amber `catch` family (`--catch-*`) — Priority reuses
 * amber deliberately (DESIGN.md:307): precedence never co-occurs with a
 * catching-up/edit-lock indicator in the same cell, so it cannot ambiguate;
 * - `emergency` → the red `sync` family (`--sync-*`).
 *
 * `PublicLiveSessionPage` passes `showPrecedence`, so this component is what
 * an account-less observer's row renders too, and the chip's legibility on
 * that surface matters as much as on the console.
 *
 * `RosterEntry` renders this only when told to, via `showSource ||
 * showPrecedence`, and the colour + icon + label treatment below is structural
 * precisely so it survives being read by someone who did not set it.
 */

/** A short dash — the routine (ordinary traffic) affordance. */
const dashIcon: ReactNode = (
  <svg width="10" height="10" viewBox="0 0 10 10" fill="currentColor">
    <rect x="1.5" y="4.25" width="7" height="1.5" rx="0.75" />
  </svg>
);

/** An up-chevron — the priority (worked ahead) affordance. */
const chevronIcon: ReactNode = (
  <svg width="10" height="10" viewBox="0 0 10 10" fill="none" stroke="currentColor" strokeWidth="2">
    <path d="M2 6.5 L5 3 L8 6.5" strokeLinecap="round" strokeLinejoin="round" />
  </svg>
);

/** A warning triangle with a bang — the emergency affordance. */
const alertIcon: ReactNode = (
  <svg width="10" height="10" viewBox="0 0 10 10" fill="currentColor">
    <path d="M5 0.5 L9.5 9 L0.5 9 Z" />
    <rect x="4.35" y="3.2" width="1.3" height="3" rx="0.65" fill="var(--sync-fill)" />
    <rect x="4.35" y="6.9" width="1.3" height="1.3" rx="0.65" fill="var(--sync-fill)" />
  </svg>
);

interface Variant {
  readonly tone: StatusTone;
  readonly label: string;
  readonly fill: string;
  readonly border: string;
  readonly icon: ReactNode;
}

const variants: Record<Precedence, Variant> = {
  routine: {
    tone: "neutral",
    label: "Routine",
    fill: "var(--surface-2)",
    border: "var(--border)",
    icon: dashIcon,
  },
  priority: {
    tone: "catching-up",
    label: "Priority",
    fill: "var(--catch-fill)",
    border: "var(--catch-border)",
    icon: chevronIcon,
  },
  emergency: {
    tone: "out-of-sync",
    label: "Emergency",
    fill: "var(--sync-fill)",
    border: "var(--sync-border)",
    icon: alertIcon,
  },
};

const chipStyle: CSSProperties = {
  display: "inline-flex",
  alignItems: "center",
  padding: "0 var(--space-2)",
  borderRadius: "var(--rounded-sm)",
  borderStyle: "solid",
  borderWidth: "1px",
  fontSize: tokens.typography.microCaps.fontSize,
  fontWeight: tokens.typography.microCaps.fontWeight,
  letterSpacing: tokens.typography.microCaps.letterSpacing,
  textTransform: "uppercase",
};

export interface PrecedenceChipProps {
  /** Which precedence to advertise. */
  readonly precedence: Precedence;
}

/** Color + icon + label precedence chip (never color alone). */
export function PrecedenceChip({ precedence }: PrecedenceChipProps): ReactElement {
  const variant = variants[precedence];
  return (
    <span
      data-precedence={precedence}
      style={{ ...chipStyle, background: variant.fill, borderColor: variant.border }}
    >
      <StatusIndicator tone={variant.tone} icon={variant.icon} label={variant.label} />
    </span>
  );
}
