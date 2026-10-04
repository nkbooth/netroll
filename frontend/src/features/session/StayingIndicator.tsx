// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
import type { CSSProperties, ReactElement, ReactNode } from "react";

import { StatusIndicator } from "../../ui/components/StatusIndicator";
import type { StatusTone } from "../../ui/components/StatusIndicator";
import type { StayingStatus } from "./sessionWire";

/**
 * The roster staying-status indicator. Built ON the
 * shipped `StatusIndicator` primitive — exactly the `SourceBadge`-on-
 * `StatusIndicator` pattern — so color + icon + label is enforced structurally
 * (operators watch on a phone in the dark; never color alone).
 *
 * - `staying-for-comments` → the `--staying` green + a check icon + "Staying".
 * - `in-and-out` → the muted `neutral` tone + a dash icon + "In & out".
 *
 * `RosterEntry` renders this only when told to — its `showSource` (staff) or its
 * `showStaying` (every observer surface).
 *
 * The colour + icon + LABEL treatment above is NOT relaxed for the denser
 * public row: it is a structural accessibility rule, not an operator-console
 * one.
 */

/** A check mark — the staying-for-comments affordance. */
const checkIcon: ReactNode = (
  <svg width="10" height="10" viewBox="0 0 10 10" fill="none" stroke="currentColor" strokeWidth="2">
    <path d="M2 5.5 L4 7.5 L8 2.5" strokeLinecap="round" strokeLinejoin="round" />
  </svg>
);

/** A short dash — the in-and-out (passed through) affordance. */
const dashIcon: ReactNode = (
  <svg width="10" height="10" viewBox="0 0 10 10" fill="currentColor">
    <rect x="1.5" y="4.25" width="7" height="1.5" rx="0.75" />
  </svg>
);

interface Variant {
  readonly tone: StatusTone;
  readonly icon: ReactNode;
  readonly label: string;
}

const variants: Record<StayingStatus, Variant> = {
  "staying-for-comments": { tone: "staying", icon: checkIcon, label: "Staying" },
  "in-and-out": { tone: "neutral", icon: dashIcon, label: "In & out" },
};

const wrapStyle: CSSProperties = { display: "inline-flex" };

export interface StayingIndicatorProps {
  /** Which staying status to advertise. */
  readonly staying: StayingStatus;
}

/** Color + icon + label staying-status indicator (never color alone). */
export function StayingIndicator({ staying }: StayingIndicatorProps): ReactElement {
  const variant = variants[staying];
  return (
    <span data-staying={staying} style={wrapStyle}>
      <StatusIndicator tone={variant.tone} icon={variant.icon} label={variant.label} />
    </span>
  );
}
