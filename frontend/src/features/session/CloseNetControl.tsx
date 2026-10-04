// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
import { useState } from "react";
import type { CSSProperties, ReactElement } from "react";

import { messageForProblem } from "../../errors/problemMessages";
import { ProblemError } from "../auth/authApi";
import type { Problem } from "../auth/authApi";
import { closeSession } from "./sessionApi";
import type { SessionSummaryBody } from "./sessionWire";

/**
 * Thin owner control that closes the viewed live session
 * (`POST /api/net-sessions/{id}/close`). The authoritative `session.closed`
 * event also streams over the WS and folds the state to `closed`; `onClosed`
 * lets the page reflect the returned summary immediately.
 */

const buttonStyle: CSSProperties = {
  padding: "var(--space-1) var(--space-4)",
  background: "var(--surface-2)",
  color: "var(--text)",
  border: "1px solid var(--sync-border)",
  borderRadius: "var(--rounded-md)",
  font: "inherit",
  fontWeight: 700,
  cursor: "pointer",
};

const errorStyle: CSSProperties = { color: "var(--sync-text)", marginTop: "var(--space-2)" };

export interface CloseNetControlProps {
  readonly sessionId: string;
  /** Invoked with the folded summary once the close succeeds. */
  readonly onClosed: (summary: SessionSummaryBody) => void;
}

/** A Close Net button that ends the session and reports the closed summary. */
export function CloseNetControl({
  sessionId,
  onClosed,
}: CloseNetControlProps): ReactElement {
  const [busy, setBusy] = useState(false);
  const [problem, setProblem] = useState<Problem | undefined | null>(null);

  const onClose = async (): Promise<void> => {
    setBusy(true);
    setProblem(null);
    try {
      const summary = await closeSession(sessionId);
      onClosed(summary);
      setBusy(false);
    } catch (error: unknown) {
      setProblem(error instanceof ProblemError ? error.problem : undefined);
      setBusy(false);
    }
  };

  return (
    <div>
      <button type="button" onClick={onClose} disabled={busy} style={buttonStyle}>
        Close Net
      </button>
      {problem !== null && (
        <p role="alert" style={errorStyle}>
          {messageForProblem(problem)}
        </p>
      )}
    </div>
  );
}
