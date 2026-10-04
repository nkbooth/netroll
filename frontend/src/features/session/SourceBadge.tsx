// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
import type { CSSProperties, ReactElement, ReactNode } from "react";

import { StatusIndicator } from "../../ui/components/StatusIndicator";
import { tokens } from "../../ui/tokens/tokens";

/**
 * The check-in source badge. Built ON the shipped
 * `StatusIndicator` primitive, whose own doc reserves "source-badge …
 * variants that build on it", so color+icon+label is enforced structurally,
 * exactly like `ConnectionStatus`; this component adds only the outer chip
 * (fill + border) using the `--staff-*`/`--self-*` design tokens, which the
 * base primitive's plain-pill style does not carry.
 *
 * Two variants:
 * - `staff` (amber) — a check-in an operator logged through the staff
 * `LogCheckIn` write path. Every quick-add entry is staff-entered, since the
 * quick-add is the only producer.
 * - `self` (cyan) — a participant self-check-in.
 *
 * Operator-console only: the public/account-less roster gets no badge, so
 * `RosterEntry` renders this solely when told to (its `showSource`).
 */

/** The two check-in provenances a roster row can advertise. */
export type BadgeSource = "staff" | "self";

interface Variant {
  readonly label: string;
  readonly fill: string;
  readonly border: string;
  readonly icon: ReactNode;
}

/** A small filled square — the staff (log-entered) affordance. */
const squareIcon: ReactNode = (
  <svg width="8" height="8" viewBox="0 0 8 8" fill="currentColor">
    <rect x="0.5" y="0.5" width="7" height="7" rx="1.5" />
  </svg>
);

/** A small filled circle — the self affordance. */
const circleIcon: ReactNode = (
  <svg width="8" height="8" viewBox="0 0 8 8" fill="currentColor">
    <circle cx="4" cy="4" r="3.5" />
  </svg>
);

const variants: Record<BadgeSource, Variant> = {
  staff: {
    label: "Staff-entered",
    fill: "var(--staff-fill)",
    border: "var(--staff-border)",
    icon: squareIcon,
  },
  self: {
    label: "Self",
    fill: "var(--self-fill)",
    border: "var(--self-border)",
    icon: circleIcon,
  },
};

const chipStyle: CSSProperties = {
  display: "inline-flex",
  alignItems: "center",
  padding: "0 var(--space-2)",
  borderRadius: "var(--rounded-full)",
  borderStyle: "solid",
  borderWidth: "1px",
  fontSize: tokens.typography.microCaps.fontSize,
  fontWeight: tokens.typography.microCaps.fontWeight,
  letterSpacing: tokens.typography.microCaps.letterSpacing,
  textTransform: "uppercase",
};

export interface SourceBadgeProps {
  /** Which provenance to advertise. */
  readonly source: BadgeSource;
}

/** Color + icon + label check-in source chip (never color alone). */
export function SourceBadge({ source }: SourceBadgeProps): ReactElement {
  const variant = variants[source];
  return (
    <span
      data-source={source}
      style={{
        ...chipStyle,
        background: variant.fill,
        borderColor: variant.border,
      }}
    >
      <StatusIndicator tone={source} icon={variant.icon} label={variant.label} />
    </span>
  );
}
