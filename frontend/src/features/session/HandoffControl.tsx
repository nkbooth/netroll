// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
import { useState } from "react";
import type { CSSProperties, ReactElement } from "react";

import { messageForProblem } from "../../errors/problemMessages";
import { ProblemError } from "../auth/authApi";
import type { Problem } from "../auth/authApi";
import { handOffControl } from "./sessionApi";
import type { SessionSummaryBody } from "./sessionWire";

/**
 * The voluntary-handoff affordance shown to the CURRENT active NCS on a HEALTHY
 * (active, live) net. The active NCS picks a
 * NetControl-tier target and hands control over WITHOUT interrupting the live
 * stream (control_state stays active). The eligible targets are supplied by the
 * page (its resolved NetControl-tier grants + owners); the server re-verifies the
 * target's qualification (422 `handoff-target-unqualified` otherwise).
 */

const controlStyle: CSSProperties = { display: "flex", gap: "var(--space-2)", alignItems: "center" };

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

/** One NetControl-tier account the active NCS may hand off to. */
export interface HandoffTarget {
  readonly accountId: string;
  readonly callsign: string;
}

export interface HandoffControlProps {
  readonly sessionId: string;
  /** The eligible NetControl-tier targets (owners + granted net-control). */
  readonly targets: readonly HandoffTarget[];
  /** Invoked with the folded summary once the handoff succeeds. */
  readonly onHandedOff: (summary: SessionSummaryBody) => void;
}

/** A "Hand off control" picker + button for the active NCS on a healthy net. */
export function HandoffControl({
  sessionId,
  targets,
  onHandedOff,
}: HandoffControlProps): ReactElement {
  const [busy, setBusy] = useState(false);
  const [problem, setProblem] = useState<Problem | undefined | null>(null);
  const [selected, setSelected] = useState<string>(targets[0]?.accountId ?? "");

  const onHandoff = async (): Promise<void> => {
    if (selected === "") {
      return;
    }
    setBusy(true);
    setProblem(null);
    try {
      const summary = await handOffControl(sessionId, selected);
      onHandedOff(summary);
      setBusy(false);
    } catch (error: unknown) {
      setProblem(error instanceof ProblemError ? error.problem : undefined);
      setBusy(false);
    }
  };

  return (
    <div>
      <div style={controlStyle}>
        <label htmlFor="handoff-target">Hand off control</label>
        <select
          id="handoff-target"
          value={selected}
          disabled={busy || targets.length === 0}
          onChange={(event) => setSelected(event.target.value)}
        >
          {targets.map((target) => (
            <option key={target.accountId} value={target.accountId}>
              {target.callsign}
            </option>
          ))}
        </select>
        <button
          type="button"
          onClick={onHandoff}
          disabled={busy || selected === ""}
          style={buttonStyle}
        >
          Hand off
        </button>
      </div>
      {problem !== null && (
        <p role="alert" style={errorStyle}>
          {messageForProblem(problem)}
        </p>
      )}
    </div>
  );
}
