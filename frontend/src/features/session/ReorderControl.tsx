// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
import { useState } from "react";
import type { CSSProperties, ReactElement } from "react";

import { messageForProblem } from "../../errors/problemMessages";
import { ProblemError } from "../auth/authApi";
import type { Problem } from "../auth/authApi";
import { reorderRoster } from "./sessionApi";
import type { SessionSummaryBody } from "./sessionWire";

/**
 * Thin operator control that orders the shared roster by precedence
 * (`POST /api/net-sessions/{id}/reorder`). NCS-only server-side
 * (`ReorderRoster`); a non-NCS operator sees the button but the server refuses
 * with 403, surfaced inline. The authoritative `roster.reordered` delta also
 * streams over the WS and folds the new order for every console; `onReordered`
 * lets this page reflect the returned summary immediately.
 */

const buttonStyle: CSSProperties = {
  padding: "var(--space-1) var(--space-4)",
  background: "var(--surface-2)",
  color: "var(--text)",
  border: "1px solid var(--border)",
  borderRadius: "var(--rounded-md)",
  font: "inherit",
  fontWeight: 700,
  cursor: "pointer",
};

const errorStyle: CSSProperties = { color: "var(--sync-text)", marginTop: "var(--space-2)" };

export interface ReorderControlProps {
  readonly sessionId: string;
  /** Invoked with the folded summary once the reorder succeeds. */
  readonly onReordered: (summary: SessionSummaryBody) => void;
}

/** An "Order by precedence" button that reorders the roster NCS-side. */
export function ReorderControl({ sessionId, onReordered }: ReorderControlProps): ReactElement {
  const [busy, setBusy] = useState(false);
  const [problem, setProblem] = useState<Problem | undefined | null>(null);

  const onClick = async (): Promise<void> => {
    setBusy(true);
    setProblem(null);
    try {
      const summary = await reorderRoster(sessionId);
      onReordered(summary);
      setBusy(false);
    } catch (error: unknown) {
      setProblem(error instanceof ProblemError ? error.problem : undefined);
      setBusy(false);
    }
  };

  return (
    <div>
      <button type="button" onClick={() => void onClick()} disabled={busy} style={buttonStyle}>
        Order by precedence
      </button>
      {problem !== null && (
        <p role="alert" style={errorStyle}>
          {messageForProblem(problem)}
        </p>
      )}
    </div>
  );
}
