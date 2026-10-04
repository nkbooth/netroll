// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
import { useEffect, useState } from "react";
import type { CSSProperties, ReactElement } from "react";

import { messageForProblem } from "../../errors/problemMessages";
import { ProblemError } from "../auth/authApi";
import type { Problem } from "../auth/authApi";
import { describeConnection, isRfKind } from "../nets/connectionPresentation";
import type { NetConnection } from "../nets/netsApi";
import { changeFrequency } from "./sessionApi";
import { tokens } from "../../ui/tokens/tokens";

/**
 * Thin owner control that changes the operating frequency of a live session
 * mid-run (`POST /api/net-sessions/{id}/frequency`). It renders only
 * on the live-session owner surface alongside `CloseNetControl`. On success it
 * simply clears its busy state and lets the authoritative `frequency.changed`
 * WS delta re-render the pill — it never hand-sets store state (no optimistic
 * frequency echo). The server (`session_sm::ensure_mutable`) remains the real
 * authority for whether the change is admitted.
 */

const fieldStyle: CSSProperties = {
  display: "flex",
  flexDirection: "column",
  gap: "var(--space-1)",
  marginBottom: "var(--space-2)",
};

const labelStyle: CSSProperties = {
  fontSize: tokens.typography.labelCaps.fontSize,
  color: "var(--text-muted)",
  textTransform: "uppercase",
  letterSpacing: "0.04em",
};

const inputStyle: CSSProperties = {
  padding: "var(--space-1) var(--space-2)",
  background: "var(--surface-2)",
  color: "var(--text)",
  border: "1px solid var(--border)",
  borderRadius: "var(--rounded-md)",
  font: "inherit",
};

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

export interface FrequencyControlProps {
  readonly sessionId: string;
  /** The session's folded connection set — the RF entries are what can move. */
  readonly connections: readonly NetConnection[];
}

/**
 * Connection picker + decimal-MHz input + Set frequency button that retunes one
 * of a live session's connections. Renders `null` when no connection has a
 * frequency to change.
 */
export function FrequencyControl({
  sessionId,
  connections,
}: FrequencyControlProps): ReactElement | null {
  const tunable = connections.filter(
    (c) => isRfKind(c.kind) && c.plannedFrequencyHz !== null,
  );
  const [selectedId, setSelectedId] = useState(tunable.at(0)?.id ?? "");
  // `.at(0)` rather than `[0]`: without `noUncheckedIndexedAccess` an index
  // read is typed as always present, which made `selected` non-nullable to the
  // compiler while it is `undefined` for every internet-only net. `.at()` is
  // typed `T | undefined`, so the single guard below is what lets `onSubmit`
  // read `selected.id` — the type enforces the narrowing rather than decorating it.
  const selected = tunable.find((c) => c.id === selectedId) ?? tunable.at(0);
  const currentFrequencyMhz = (selected?.plannedFrequencyHz ?? 0) / 1_000_000;
  const [operatingFrequency, setOperatingFrequency] = useState(String(currentFrequencyMhz));
  // Tracks an unsubmitted edit so the resync effect below never clobbers
  // in-progress typing, and the field's prefill still recovers correctly the
  // moment a submit succeeds: without this,
  // `currentFrequencyMhz` was only read once at mount, so a co-owner's
  // concurrent change (or this session's own confirmed delta) updated the pill
  // but left this input showing the STALE value — an untouched resubmit would
  // silently revert the concurrent change.
  const [dirty, setDirty] = useState(false);
  const [busy, setBusy] = useState(false);
  const [problem, setProblem] = useState<Problem | undefined | null>(null);

  useEffect(() => {
    if (!dirty) {
      setOperatingFrequency(String(currentFrequencyMhz));
    }
  }, [currentFrequencyMhz, dirty]);

  // After every hook (rules of hooks) and before `onSubmit` is defined: the one
  // narrowing of `selected` that `onSubmit` and the JSX depend on. (The optional
  // chain above it is a separate, deliberate narrowing — two hooks read
  // `selected` before any early return is allowed, and substitute 0 Hz.) A
  // second guard inside `onSubmit` used to return after `setBusy(true)` with no
  // `finally`, a branch that could only be reached from a state this
  // `return null` forbids.
  if (selected === undefined) {
    return null;
  }

  const onSubmit = async (): Promise<void> => {
    setBusy(true);
    setProblem(null);
    try {
      // The authoritative frequency.changed delta drives the pill; do not poke
      // store state here. Clearing `dirty` lets the resync effect above
      // pick up that eventual delta once it arrives as a new prop.
      await changeFrequency(sessionId, selected.id, operatingFrequency);
      setDirty(false);
      setBusy(false);
    } catch (error: unknown) {
      setProblem(error instanceof ProblemError ? error.problem : undefined);
      setBusy(false);
    }
  };

  return (
    <div>
      {tunable.length > 1 && (
        <label style={fieldStyle}>
          <span style={labelStyle}>Which way in</span>
          <select
            value={selected.id}
            onChange={(event) => {
              setDirty(false);
              setSelectedId(event.target.value);
            }}
            style={inputStyle}
          >
            {tunable.map((connection) => (
              <option key={connection.id} value={connection.id}>
                {describeConnection(connection).kindLabel}
                {connection.band === null ? "" : ` · ${connection.band}`}
              </option>
            ))}
          </select>
        </label>
      )}
      <label style={fieldStyle}>
        <span style={labelStyle}>Operating frequency (MHz)</span>
        <input
          type="text"
          inputMode="decimal"
          value={operatingFrequency}
          onChange={(event) => {
            setDirty(true);
            setOperatingFrequency(event.target.value);
          }}
          style={inputStyle}
        />
      </label>
      <button type="button" onClick={onSubmit} disabled={busy} style={buttonStyle}>
        Set frequency
      </button>
      {problem !== null && (
        <p role="alert" style={errorStyle}>
          {messageForProblem(problem)}
        </p>
      )}
    </div>
  );
}
