// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
import type { CSSProperties, ReactElement } from "react";

import type { ConnectionState } from "./sessionStore";
import { tokens } from "../../ui/tokens/tokens";

/**
 * The full-width recovery banner above the roster. While
 * `catching-up` it shows an amber "reconnecting" status; when `out-of-sync` it
 * shows a red "lost the server — showing last known roster" alert plus a SOLID
 * resync button that restarts the recovery choreography. It renders NOTHING when
 * `live` (or `net-paused`, which has no server trigger). It NEVER
 * prompts a manual page refresh (EXPERIENCE.md:119).
 */

const bannerBase: CSSProperties = {
  padding: "var(--space-2) var(--space-page-x)",
  fontSize: tokens.typography.meta.fontSize,
  display: "flex",
  alignItems: "center",
  gap: "var(--space-3)",
  borderRadius: "var(--rounded-md)",
};

const resyncButtonStyle: CSSProperties = {
  padding: "var(--space-1) var(--space-3)",
  background: "var(--sync-solid)",
  color: "var(--on-accent)",
  border: "none",
  borderRadius: "var(--rounded-md)",
  font: "inherit",
  fontWeight: 700,
  cursor: "pointer",
};

export interface ReplayingStateProps {
  readonly connection: ConnectionState;
  /** Restarts the recovery choreography (wired to the stream's resync). */
  readonly onResync: () => void;
}

/** The reconnect/out-of-sync banner; renders nothing when the stream is live. */
export function ReplayingState({
  connection,
  onResync,
}: ReplayingStateProps): ReactElement | null {
  if (connection === "catching-up") {
    return (
      <div
        role="status"
        style={{
          ...bannerBase,
          background: "var(--catch-fill)",
          color: "var(--catch-text)",
          border: "1px solid var(--catch-border)",
        }}
      >
        <span>Reconnecting — catching up…</span>
      </div>
    );
  }

  if (connection === "out-of-sync") {
    return (
      <div
        role="alert"
        style={{
          ...bannerBase,
          background: "var(--sync-fill)",
          color: "var(--sync-text)",
          border: "1px solid var(--sync-border)",
        }}
      >
        <span>Lost the server — showing last known roster.</span>
        <button type="button" onClick={onResync} style={resyncButtonStyle}>
          Resync
        </button>
      </div>
    );
  }

  return null;
}
