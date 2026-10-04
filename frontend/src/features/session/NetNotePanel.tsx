// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
import { useEffect, useState } from "react";
import type { CSSProperties, ReactElement } from "react";

import { messageForProblem } from "../../errors/problemMessages";
import { ProblemError } from "../auth/authApi";
import type { Problem } from "../auth/authApi";
import { setNetNote } from "./sessionApi";
import type { SessionSummaryBody } from "./sessionWire";
import { tokens } from "../../ui/tokens/tokens";

/**
 * The session-scoped net-level note panel — a dedicated
 * operator-console home for the running net note (NOT inside a per-check-in
 * modal; net-level notes are session-scoped). It seeds from the folded
 * `netNote`, and Save calls `setNetNote` and re-seeds the returned summary so the
 * panel reflects the authoritative value. Logger+ server-side (`AnnotateSession`);
 * a non-Logger sees the panel but the server refuses with 403, surfaced inline.
 */

const panelStyle: CSSProperties = {
  display: "flex",
  flexDirection: "column",
  gap: "var(--space-1)",
  margin: "var(--space-2) 0",
};

const labelStyle: CSSProperties = {
  fontSize: tokens.typography.labelCaps.fontSize,
  color: "var(--text-muted)",
  textTransform: "uppercase",
  letterSpacing: "0.04em",
};

const textareaStyle: CSSProperties = {
  padding: "var(--space-1) var(--space-2)",
  background: "var(--surface-2)",
  color: "var(--text)",
  border: "1px solid var(--border)",
  borderRadius: "var(--rounded-md)",
  font: "inherit",
  minHeight: "3.5em",
  resize: "vertical",
};

const buttonStyle: CSSProperties = {
  alignSelf: "flex-start",
  padding: "var(--space-1) var(--space-4)",
  background: "var(--surface-2)",
  color: "var(--text)",
  border: "1px solid var(--border)",
  borderRadius: "var(--rounded-md)",
  font: "inherit",
  fontWeight: 700,
  cursor: "pointer",
};

const errorStyle: CSSProperties = { color: "var(--sync-text)", margin: 0 };

export interface NetNotePanelProps {
  readonly sessionId: string;
  /** The current folded net-level note, or `null`. Seeds the textarea. */
  readonly netNote: string | null;
  /** Invoked with the folded summary once the note saves. */
  readonly onSaved: (summary: SessionSummaryBody) => void;
}

/** A net-level note textarea + Save for the operator console. */
export function NetNotePanel({ sessionId, netNote, onSaved }: NetNotePanelProps): ReactElement {
  const [value, setValue] = useState(netNote ?? "");
  const [busy, setBusy] = useState(false);
  const [problem, setProblem] = useState<Problem | undefined | null>(null);
  // Tracks whether the operator has typed a local draft since the last seed
  // (mount, external resync, or a successful Save). There is no per-field
  // lock on the net note (unlike the per-check-in modal's soft-lock) — this
  // panel stays mounted for the whole session, so another
  // operator's `session.note-set` delta can fold in at any time. Without
  // this, the textarea seeded once at mount and never followed later
  // updates, silently hiding a teammate's saved note and risking Save
  // clobbering it with stale local text.
  const [dirty, setDirty] = useState(false);

  useEffect(() => {
    if (!dirty) {
      setValue(netNote ?? "");
    }
  }, [netNote, dirty]);

  const onSave = async (): Promise<void> => {
    setBusy(true);
    setProblem(null);
    try {
      // A blank note clears it server-side (parsed to None); send the raw value.
      const summary = await setNetNote(sessionId, value.trim() === "" ? null : value);
      onSaved(summary);
      setDirty(false);
      setBusy(false);
    } catch (error: unknown) {
      setProblem(error instanceof ProblemError ? error.problem : undefined);
      setBusy(false);
    }
  };

  return (
    <div style={panelStyle}>
      <label style={labelStyle} htmlFor="net-note-field">
        Net note
      </label>
      <textarea
        id="net-note-field"
        style={textareaStyle}
        value={value}
        onChange={(event) => {
          setDirty(true);
          setValue(event.target.value);
        }}
        placeholder="Running net-level note (operator-only)"
      />
      <button type="button" style={buttonStyle} disabled={busy} onClick={() => void onSave()}>
        Save note
      </button>
      {problem !== null && (
        <p role="alert" style={errorStyle}>
          {messageForProblem(problem)}
        </p>
      )}
    </div>
  );
}
