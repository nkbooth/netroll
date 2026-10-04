// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
import { useState } from "react";
import type { CSSProperties, ReactElement } from "react";

import { messageForProblem } from "../../errors/problemMessages";
import { ProblemError } from "../auth/authApi";
import type { Problem } from "../auth/authApi";
import { canViewerDo } from "./capabilities";
import type { ViewerRole } from "./capabilities";
import { moderateCheckIn as moderateCheckInApi } from "./sessionApi";
import type { CheckInSource, SessionSummaryBody } from "./sessionWire";

/**
 * The NCS session-moderation control — the disciplinary
 * remove, and optional account block, of a disruptive station. Rendered inside
 * the staff console (the detail modal) as a control, not a separate surface
 * (EXPERIENCE.md:41).
 *
 * DISTINCT from the detail modal's own Remove button (the Logger-floor
 * logging correction): this offers an NCS-tier disciplinary action gated by
 * {@link canViewerDo}(`viewerRole`, `"moderate"`). "Remove & block" is shown ONLY
 * for a `self`-sourced entry (an account exists to block); an account-less
 * staff-logged entry offers plain "Remove" only. This is a UX affordance gate
 * ONLY — the server remains the sole authority (a tampered client that
 * renders the control still gets a 403 / 422).
 */

/** Injectable collaborators (default to the real `sessionApi`). */
export interface ModerationControlsDeps {
  moderateCheckIn?: typeof moderateCheckInApi;
}

export interface ModerationControlsProps {
  readonly sessionId: string;
  /** The roster entry's stable id (the CAS/moderation target). */
  readonly checkInId: string;
  /** The entry's fold-derived CAS version, sent as `expectedVersion`. */
  readonly version: number;
  /** The entry's provenance — `self` entries carry an account, so they can be
   * blocked; a `staff` (account-less) entry offers plain remove only. */
  readonly source: CheckInSource;
  /** The viewer's own resolved role — the moderation gate reads it. `null` (an
   * account-less public viewer) holds nothing, so the control renders nothing. */
  readonly viewerRole: ViewerRole | null;
  /** Called with the folded summary after a successful moderation (the parent
   * re-seeds its store and typically closes the modal). */
  readonly onModerated: (summary: SessionSummaryBody) => void;
  readonly deps?: ModerationControlsDeps;
}

const groupStyle: CSSProperties = {
  display: "flex",
  flexDirection: "column",
  gap: "var(--space-2)",
  paddingTop: "var(--space-3)",
  marginTop: "var(--space-3)",
  borderTop: "1px solid var(--border, rgba(128,128,128,.24))",
};

const buttonRowStyle: CSSProperties = {
  display: "flex",
  gap: "var(--space-2)",
  flexWrap: "wrap",
};

const dangerStyle: CSSProperties = {
  background: "var(--sync-solid)",
  color: "var(--on-accent)",
  border: "none",
  borderRadius: "var(--rounded-md)",
  padding: "var(--space-2) var(--space-3)",
  cursor: "pointer",
};

const errorStyle: CSSProperties = {
  color: "var(--sync-text)",
  margin: 0,
};

/**
 * Renders the NCS moderation affordance for one roster entry, or `null` when the
 * viewer lacks the `moderate` capability.
 */
export function ModerationControls({
  sessionId,
  checkInId,
  version,
  source,
  viewerRole,
  onModerated,
  deps,
}: ModerationControlsProps): ReactElement | null {
  const moderateCheckIn = deps?.moderateCheckIn ?? moderateCheckInApi;
  const [busy, setBusy] = useState(false);
  // Three-state sentinel (mirrors CheckInDetailModal's own `problem` state):
  // `null` = no error yet; `undefined` = a
  // non-`ProblemError` failure occurred (network error, etc. — shown via the
  // generic fallback message); a `Problem` = a specific mapped error. Using
  // ONLY `undefined` for both "no error" and "unknown error" (the original
  // shape here) made a genuine failure indistinguishable from the initial
  // state, so a non-`ProblemError` rejection silently rendered no feedback.
  const [problem, setProblem] = useState<Problem | undefined | null>(null);

  // UX render-gate only: the server re-checks Capability::Moderate.
  if (!canViewerDo(viewerRole, "moderate")) {
    return null;
  }

  const run = async (block: boolean): Promise<void> => {
    if (busy) {
      return;
    }
    setBusy(true);
    setProblem(null);
    try {
      const summary = await moderateCheckIn(sessionId, checkInId, {
        block,
        expectedVersion: version,
      });
      onModerated(summary);
    } catch (error: unknown) {
      setProblem(error instanceof ProblemError ? error.problem : undefined);
    } finally {
      setBusy(false);
    }
  };

  return (
    <div style={groupStyle} aria-label="Moderation">
      {/* A visible cue distinguishing this NCS-tier action from the plain
          correction "Remove" button above it in the same modal — every
          viewer who can reach this control (NetControl+) also holds the
          Logger-floor `EditCheckIn` that renders that OTHER button, so
          without this label the two identically-worded "Remove" buttons are
          not visually distinguishable. */}
      <p style={{ margin: 0, fontSize: "0.85em", opacity: 0.75 }}>
        NCS moderation — overrides another operator's edit lock
      </p>
      <div style={buttonRowStyle}>
        <button
          type="button"
          style={dangerStyle}
          disabled={busy}
          onClick={() => void run(false)}
        >
          Remove
        </button>
        {source === "self" && (
          <button
            type="button"
            style={dangerStyle}
            disabled={busy}
            onClick={() => void run(true)}
          >
            Remove &amp; block
          </button>
        )}
      </div>
      {problem !== null && (
        <p role="alert" style={errorStyle}>
          {messageForProblem(problem)}
        </p>
      )}
    </div>
  );
}
