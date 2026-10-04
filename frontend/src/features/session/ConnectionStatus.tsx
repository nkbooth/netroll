// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
import type { CSSProperties, ReactElement } from "react";

import { StatusIndicator } from "../../ui/components/StatusIndicator";
import type { StatusTone } from "../../ui/components/StatusIndicator";
import { prefersReducedMotion } from "../../ui/tokens/motion";
import type { ConnectionState } from "./sessionStore";
import "./liveIndicator.css";

/**
 * The connection-status pill. Built ON the shipped
 * `StatusIndicator` primitive so a status is ALWAYS color + icon + label, never
 * color alone — operators watch on a phone in the dark. The live dot pulses via
 * the shipped `.live-pulse` hook; `prefers-reduced-motion` drops the pulse while
 * KEEPING the color + icon + label (motion.ts). `net-paused` renders as a
 * first-class state though no server signal enters it yet.
 */

interface Presentation {
  readonly tone: StatusTone;
  readonly label: string;
}

/**
 * Maps a connection state to its `StatusIndicator` tone + label. Pure and
 * exported so the state→presentation mapping is unit-tested as logic, apart from
 * rendering. `net-paused` uses the shipped `paused` tone.
 */
export function connectionPresentation(connection: ConnectionState): Presentation {
  switch (connection) {
    case "live":
      return { tone: "live", label: "Live" };
    case "catching-up":
      return { tone: "catching-up", label: "Catching up" };
    case "out-of-sync":
      return { tone: "out-of-sync", label: "Out of sync" };
    case "net-paused":
      return { tone: "paused", label: "Paused" };
  }
}

const dotBase: CSSProperties = {
  width: "8px",
  height: "8px",
  borderRadius: "var(--rounded-full)",
  display: "inline-block",
};

/** The colored status dot; the live variant pulses unless motion is reduced. */
function StatusDot({ connection }: { connection: ConnectionState }): ReactElement {
  const isLive = connection === "live";
  const pulsing = isLive && !prefersReducedMotion();
  const color = isLive ? "var(--live-dot)" : "currentColor";
  return (
    <span
      data-testid={isLive ? "live-dot" : "status-dot"}
      data-pulsing={String(pulsing)}
      className={pulsing ? "live-pulse" : undefined}
      style={{ ...dotBase, background: color }}
    />
  );
}

export interface ConnectionStatusProps {
  readonly connection: ConnectionState;
}

/** The color + icon + label connection pill. */
export function ConnectionStatus({ connection }: ConnectionStatusProps): ReactElement {
  const { tone, label } = connectionPresentation(connection);
  return (
    // `data-state` is a stable behavioral hook for the E2E (assert the state,
    // not brittle label copy).
    <span data-testid="connection-status" data-state={connection}>
      <StatusIndicator tone={tone} icon={<StatusDot connection={connection} />} label={label} />
    </span>
  );
}
