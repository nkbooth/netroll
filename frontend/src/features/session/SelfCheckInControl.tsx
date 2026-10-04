// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
import { useState } from "react";
import type { CSSProperties, ReactElement } from "react";
import { useLocation, useNavigate } from "react-router";
import type { StoreApi } from "zustand/vanilla";

import { messageForProblem } from "../../errors/problemMessages";
import { ProblemError } from "../auth/authApi";
import type { Account, Problem } from "../auth/authApi";
import {
  addCheckIn as defaultAddCheckIn,
  removeCheckIn as defaultRemoveCheckIn,
  updateCheckIn as defaultUpdateCheckIn,
} from "./sessionApi";
import { selfCheckInGate } from "./selfCheckInGate";
import type { SessionStore } from "./sessionStore";
import type { StayingStatus } from "./sessionWire";
import { tokens } from "../../ui/tokens/tokens";

/**
 * The participant self-check-in control. Rendered on the
 * public/participant live view. Behavior turns on the pure `selfCheckInGate`
 * verdict:
 * - an UNGATED viewer (signed out / unconsented / email-unverified / callsign-
 * less) sees a "Check in" button that ROUTES them to auth / consent / callsign
 * setup, carrying `returnTo = the /live/:id path` so they return to the action;
 * - a gated-through viewer NOT yet checked in sees a "Check in" button that
 * optimistically self-checks-in (own callsign forced server-side, source=self)
 * via the shipped `addPending`/`clientEventId`/echo-reconcile seam;
 * - a viewer already checked in sees a staying toggle + a "Check out" button,
 * both acting on their OWN entry only (the server enforces ownership).
 *
 * The server re-checks EVERYTHING (verified email, callsign, ownership) — this
 * control is UX only; a forged request from an ungated account is refused.
 */

/** The bounded window an optimistic entry waits for its echo before rollback. */
const ECHO_TIMEOUT_MS = 8000;

/** Injectable timer (defaults to `window.setTimeout`) — fake-clock testable. */
type ScheduleTimeout = (fn: () => void, ms: number) => () => void;

const defaultScheduleTimeout: ScheduleTimeout = (fn, ms) => {
  const id = window.setTimeout(fn, ms);
  return () => window.clearTimeout(id);
};

/** The viewer's OWN roster entry (matched by callsign), for toggle/checkout. */
export interface OwnCheckIn {
  readonly checkInId: string;
  readonly staying: StayingStatus;
  readonly version: number;
}

export interface SelfCheckInControlProps {
  readonly sessionId: string;
  /** The bound session store — the source of `addPending`/`removePending`. */
  readonly store: StoreApi<SessionStore>;
  /** The current account (or `null` when signed out) — drives the gate verdict. */
  readonly account: Account | null;
  /** The viewer's own roster entry, or `null` when they are not checked in. */
  readonly ownCheckIn: OwnCheckIn | null;
  /** Injectable client-event-id minter (defaults to `crypto.randomUUID`). */
  readonly mintClientEventId?: () => string;
  /** Injectable bounded-timeout scheduler (fake-clock testable). */
  readonly scheduleTimeout?: ScheduleTimeout;
  /** Injectable API seams (real fetch wrappers by default) — test hooks. */
  readonly addCheckInFn?: typeof defaultAddCheckIn;
  readonly updateCheckInFn?: typeof defaultUpdateCheckIn;
  readonly removeCheckInFn?: typeof defaultRemoveCheckIn;
}

const wrapStyle: CSSProperties = {
  display: "flex",
  flexWrap: "wrap",
  alignItems: "center",
  gap: "var(--space-3)",
  margin: "var(--space-4) 0",
};

const primaryButtonStyle: CSSProperties = {
  padding: "var(--space-1) var(--space-4)",
  background: "var(--accent-deep)",
  color: "var(--on-accent, #fff)",
  border: "none",
  borderRadius: "var(--rounded-md)",
  font: "inherit",
  fontWeight: 700,
  cursor: "pointer",
};

const secondaryButtonStyle: CSSProperties = {
  padding: "var(--space-1) var(--space-4)",
  background: "transparent",
  color: "var(--text)",
  border: "1px solid var(--border)",
  borderRadius: "var(--rounded-md)",
  font: "inherit",
  cursor: "pointer",
};

const errorStyle: CSSProperties = {
  color: "var(--sync-text)",
  fontSize: tokens.typography.meta.fontSize,
};

/** The participant self-check-in / self-toggle / self-checkout control. */
export function SelfCheckInControl({
  sessionId,
  store,
  account,
  ownCheckIn,
  mintClientEventId = () => crypto.randomUUID(),
  scheduleTimeout = defaultScheduleTimeout,
  addCheckInFn = defaultAddCheckIn,
  updateCheckInFn = defaultUpdateCheckIn,
  removeCheckInFn = defaultRemoveCheckIn,
}: SelfCheckInControlProps): ReactElement {
  const navigate = useNavigate();
  const location = useLocation();
  // `null` = no error yet; `undefined` = a failure that carried no problem
  // body; a `Problem` = the server explained itself. The whole body is held,
  // not just the slug, because `detail` is where the server names the field
  // and the remedy.
  const [problem, setProblem] = useState<Problem | null | undefined>(null);
  const [busy, setBusy] = useState(false);

  const gate = selfCheckInGate(account);

  // An ungated viewer is ROUTED to the missing step, carrying returnTo so the
  // routed page brings them back to this action afterward.
  const routeToGate = (to: string): void => {
    navigate(to, { state: { returnTo: location.pathname } });
  };

  const selfCheckIn = async (): Promise<void> => {
    if (gate.kind === "route") {
      routeToGate(gate.to);
      return;
    }
    // gate.kind === "check-in": the account has a callsign (the gate guaranteed it).
    const callsign = account?.callsign;
    if (callsign === null || callsign === undefined || busy) {
      return;
    }
    setBusy(true);
    setProblem(null);
    let clientEventId: string | undefined;
    let cancelTimer: (() => void) | undefined;
    try {
      clientEventId = mintClientEventId();
      // Optimistic: the dimmed self row renders instantly, tagged source=self so
      // it shows the cyan Self badge before the authoritative echo.
      store.getState().addPending(clientEventId, callsign, null, "in-and-out", "self");
      const id = clientEventId;
      // The bounded echo timeout stays armed through a successful send too — the
      // only thing that clears the dimmed row if the echo is silently lost.
      cancelTimer = scheduleTimeout(() => {
        store.getState().removePending(id);
      }, ECHO_TIMEOUT_MS);
      // The server FORCES the callsign to the account's own; sending it here is a
      // UX nicety only. The WS echo reconciles the pending row.
      await addCheckInFn(sessionId, { callsign, clientEventId });
    } catch (error: unknown) {
      if (clientEventId !== undefined) {
        store.getState().removePending(clientEventId);
        cancelTimer?.();
      }
      setProblem(error instanceof ProblemError ? error.problem : undefined);
    } finally {
      setBusy(false);
    }
  };

  const toggleStaying = async (): Promise<void> => {
    const callsign = account?.callsign;
    if (ownCheckIn === null || callsign === null || callsign === undefined || busy) {
      return;
    }
    const next: StayingStatus =
      ownCheckIn.staying === "staying-for-comments" ? "in-and-out" : "staying-for-comments";
    setBusy(true);
    setProblem(null);
    try {
      await updateCheckInFn(sessionId, ownCheckIn.checkInId, {
        callsign,
        staying: next,
        expectedVersion: ownCheckIn.version,
      });
    } catch (error: unknown) {
      setProblem(error instanceof ProblemError ? error.problem : undefined);
    } finally {
      setBusy(false);
    }
  };

  const checkOut = async (): Promise<void> => {
    if (ownCheckIn === null || busy) {
      return;
    }
    setBusy(true);
    setProblem(null);
    try {
      await removeCheckInFn(sessionId, ownCheckIn.checkInId, ownCheckIn.version);
    } catch (error: unknown) {
      setProblem(error instanceof ProblemError ? error.problem : undefined);
    } finally {
      setBusy(false);
    }
  };

  return (
    <div style={wrapStyle} data-self-check-in>
      {ownCheckIn === null ? (
        <button
          type="button"
          data-action="check-in"
          style={primaryButtonStyle}
          disabled={busy}
          onClick={() => void selfCheckIn()}
        >
          Check in
        </button>
      ) : (
        <>
          <button
            type="button"
            data-action="toggle-staying"
            style={secondaryButtonStyle}
            disabled={busy}
            onClick={() => void toggleStaying()}
          >
            {ownCheckIn.staying === "staying-for-comments"
              ? "Switch to in and out"
              : "Stay for comments"}
          </button>
          <button
            type="button"
            data-action="check-out"
            style={secondaryButtonStyle}
            disabled={busy}
            onClick={() => void checkOut()}
          >
            Check out
          </button>
        </>
      )}
      {problem !== null && (
        <span role="alert" style={errorStyle}>
          {messageForProblem(problem)}
        </span>
      )}
    </div>
  );
}
