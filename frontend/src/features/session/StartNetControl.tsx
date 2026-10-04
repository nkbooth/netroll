// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
import { useState } from "react";
import type { CSSProperties, ReactElement } from "react";
import { useNavigate } from "react-router";

import { messageForProblem } from "../../errors/problemMessages";
import { ProblemError } from "../auth/authApi";
import type { Problem } from "../auth/authApi";
import { startSession } from "./sessionApi";

/**
 * Thin owner control that starts a live session for a definition
 * (`POST /api/net-sessions`) and navigates to its live page.
 *
 * There is no frequency field: the session freezes the definition's
 * whole connection list, so there is nothing left for the operator to type here.
 * Retuning one of those connections mid-run is `FrequencyControl`.
 */

const buttonStyle: CSSProperties = {
  padding: "var(--space-1) var(--space-4)",
  background: "var(--accent)",
  color: "var(--on-accent)",
  border: "none",
  borderRadius: "var(--rounded-md)",
  font: "inherit",
  fontWeight: 700,
  cursor: "pointer",
};

const errorStyle: CSSProperties = { color: "var(--sync-text)", marginTop: "var(--space-2)" };

export interface StartNetControlProps {
  readonly definitionId: string;
}

/** Start Net button that launches a session and navigates to it. */
export function StartNetControl({
  definitionId,
}: StartNetControlProps): ReactElement {
  const navigate = useNavigate();
  const [busy, setBusy] = useState(false);
  const [problem, setProblem] = useState<Problem | undefined | null>(null);

  const onStart = async (): Promise<void> => {
    setBusy(true);
    setProblem(null);
    try {
      const session = await startSession({ definitionId });
      void navigate(`/net-sessions/${session.id}`);
    } catch (error: unknown) {
      setProblem(error instanceof ProblemError ? error.problem : undefined);
      setBusy(false);
    }
  };

  return (
    <div>
      <button type="button" onClick={onStart} disabled={busy} style={buttonStyle}>
        Start Net
      </button>
      {problem !== null && (
        <p role="alert" style={errorStyle}>
          {messageForProblem(problem)}
        </p>
      )}
    </div>
  );
}
