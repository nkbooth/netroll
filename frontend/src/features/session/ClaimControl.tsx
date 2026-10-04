// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
import { useState } from "react";
import type { CSSProperties, ReactElement } from "react";

import { messageForProblem } from "../../errors/problemMessages";
import { ProblemError } from "../auth/authApi";
import type { Problem } from "../auth/authApi";
import { claimControl } from "./sessionApi";
import type { SessionSummaryBody } from "./sessionWire";

/**
 * The involuntary-claim affordance shown to `ClaimControl`-holders (Owner/
 * NetControl/Logger) while a net is STALLED. Clicking
 * it POSTs `claim-control`; the claimer becomes the active NCS and the net
 * returns to active. There is NO "resume" button — resume is presence-driven
 * This is the rescue path when the original NCS does not return.
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

export interface ClaimControlProps {
  readonly sessionId: string;
  /** Invoked with the folded summary once the claim succeeds. */
  readonly onClaimed: (summary: SessionSummaryBody) => void;
}

/** A "Take control" button that claims a stalled net for the acting operator. */
export function ClaimControl({ sessionId, onClaimed }: ClaimControlProps): ReactElement {
  const [busy, setBusy] = useState(false);
  const [problem, setProblem] = useState<Problem | undefined | null>(null);

  const onClaim = async (): Promise<void> => {
    setBusy(true);
    setProblem(null);
    try {
      const summary = await claimControl(sessionId);
      onClaimed(summary);
      setBusy(false);
    } catch (error: unknown) {
      setProblem(error instanceof ProblemError ? error.problem : undefined);
      setBusy(false);
    }
  };

  return (
    <div>
      <button type="button" onClick={onClaim} disabled={busy} style={buttonStyle}>
        Take control
      </button>
      {problem !== null && (
        <p role="alert" style={errorStyle}>
          {messageForProblem(problem)}
        </p>
      )}
    </div>
  );
}
