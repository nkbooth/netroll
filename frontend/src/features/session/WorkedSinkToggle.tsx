// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
import { useState } from "react";
import type { CSSProperties, ReactElement } from "react";

import { messageForProblem } from "../../errors/problemMessages";
import { ProblemError } from "../auth/authApi";
import type { Problem } from "../auth/authApi";
import { setRosterOrderMode } from "./sessionApi";
import type { RosterOrderMode, SessionSummaryBody } from "./sessionWire";

/**
 * Thin operator control that turns the SHARED worked-sink ordering mode on and
 * off (`POST /api/net-sessions/{id}/roster-order-mode`). NCS-only
 * server-side (`SetRosterOrderMode`); a non-NCS operator sees the switch but the
 * server refuses with 403, surfaced inline.
 *
 * The mode is SERVER state, not a view preference: the authoritative
 * `roster.order-mode-set` delta — and the `roster.reordered` that may follow it
 * in the same transaction — stream over the WS and fold for every console and
 * the public page alike. `onModeSet` lets this page reflect the returned summary
 * immediately.
 */

const rowStyle: CSSProperties = { display: "inline-flex", alignItems: "center" };

const switchStyle = (on: boolean): CSSProperties => ({
  display: "inline-flex",
  alignItems: "center",
  gap: "var(--space-2)",
  padding: "var(--space-1) var(--space-4)",
  background: on ? "var(--accent)" : "var(--surface-2)",
  color: on ? "var(--on-accent)" : "var(--text)",
  border: "1px solid var(--border)",
  borderRadius: "var(--rounded-md)",
  font: "inherit",
  fontWeight: 700,
  cursor: "pointer",
});

const pipStyle = (on: boolean): CSSProperties => ({
  width: "8px",
  height: "8px",
  borderRadius: "50%",
  background: on ? "var(--on-accent)" : "var(--text-muted)",
});

const errorStyle: CSSProperties = { color: "var(--sync-text)", marginTop: "var(--space-2)" };

export interface WorkedSinkToggleProps {
  readonly sessionId: string;
  /** The session's CURRENT folded ordering mode — server state, not local. */
  readonly mode: RosterOrderMode;
  /** Invoked with the folded summary once the mode change succeeds. */
  readonly onModeSet: (summary: SessionSummaryBody) => void;
}

/** A switch that sinks worked stations to the bottom of the shared roster. */
export function WorkedSinkToggle({
  sessionId,
  mode,
  onModeSet,
}: WorkedSinkToggleProps): ReactElement {
  const [busy, setBusy] = useState(false);
  const [problem, setProblem] = useState<Problem | undefined | null>(null);
  const on = mode === "worked-sink";

  const onClick = async (): Promise<void> => {
    setBusy(true);
    setProblem(null);
    try {
      const summary = await setRosterOrderMode(sessionId, on ? "manual" : "worked-sink");
      onModeSet(summary);
      setBusy(false);
    } catch (error: unknown) {
      setProblem(error instanceof ProblemError ? error.problem : undefined);
      setBusy(false);
    }
  };

  return (
    <div style={rowStyle}>
      <div>
        <button
          type="button"
          role="switch"
          aria-checked={on}
          onClick={() => void onClick()}
          disabled={busy}
          style={switchStyle(on)}
        >
          <span aria-hidden="true" style={pipStyle(on)} />
          Sink worked stations
        </button>
        {problem !== null && (
          <p role="alert" style={errorStyle}>
            {messageForProblem(problem)}
          </p>
        )}
      </div>
    </div>
  );
}
