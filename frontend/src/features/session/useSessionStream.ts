// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
import { useCallback, useEffect, useRef, useState } from "react";

import { ProblemError } from "../auth/authApi";
import type { Problem } from "../auth/authApi";
import { getEventsSince, getSession } from "./sessionApi";
import { createSessionStore } from "./sessionStore";
import type { SessionStore } from "./sessionStore";
import { createSessionStream } from "./sessionSocket";
import type { SessionStream } from "./sessionSocket";
import type { PublicSessionSummary, SessionSummaryBody, WireEvent } from "./sessionWire";
import type { StoreApi } from "zustand/vanilla";

/**
 * The endpoint set the stream binds to. Defaults to the
 * owner surface ([`OWNER_ENDPOINTS`]); the public live view passes the
 * account-less redacted endpoints so the SAME reconnect choreography drives
 * both. `fetchSnapshot` resolving to `null` means "unavailable" — a 401 for the
 * owner (signed-out → the page redirects to `/sign-in`), a 404 for the public
 * read (not-found → the page renders an empty state, never a sign-in redirect).
 */
export interface SessionEndpoints {
  readonly fetchSnapshot: (
    id: string,
  ) => Promise<SessionSummaryBody | PublicSessionSummary | null>;
  readonly fetchEventsSince: (id: string, since: number) => Promise<WireEvent[]>;
  readonly wsBasePath: (id: string) => string;
}

/** The owner (session-gated) endpoint set — the default. */
export const OWNER_ENDPOINTS: SessionEndpoints = {
  fetchSnapshot: getSession,
  fetchEventsSince: getEventsSince,
  wsBasePath: (id) => `/api/net-sessions/${id}/ws`,
};

/** The load lifecycle of the initial snapshot fetch (separate from connection). */
export type StreamPhase =
  | { readonly status: "loading" }
  | { readonly status: "ready" }
  | { readonly status: "signed-out" }
  | { readonly status: "error"; readonly problem?: Problem };

/** The hook's return: the bound store, the initial-load phase, and a resync trigger. */
export interface UseSessionStream {
  readonly store: StoreApi<SessionStore>;
  readonly phase: StreamPhase;
  readonly resync: () => void;
}

/**
 * Binds a per-session store to the reconnecting live stream. On mount it
 * fetches the folded snapshot over HTTP (the self-gating seam: a 401 means
 * signed-out), seeds the store, then opens the WebSocket stream which
 * self-recovers on disconnect. On unmount it stops the stream (StrictMode
 * double-invoke safe — each effect run owns its own stream instance).
 */
/**
 * Callers MUST mount this hook's consuming component under a `key={sessionId}`
 * (see `LiveSessionPage`'s outer/inner split): react-router reuses the SAME
 * element across a param-only navigation (e.g. Start Net's re-run creates a
 * new session and navigates to its id on the same route), and without a key
 * change forcing a full remount, this lazy `useState` store instance would
 * persist — the old session's roster/connection/pending would render under
 * the new id until the new snapshot fetch resolves.
 */
export function useSessionStream(
  sessionId: string,
  endpoints: SessionEndpoints = OWNER_ENDPOINTS,
): UseSessionStream {
  // Lazy-init: one store instance per mounted page, stable across renders.
  const [store] = useState<StoreApi<SessionStore>>(() => createSessionStore());
  const [phase, setPhase] = useState<StreamPhase>({ status: "loading" });
  const streamRef = useRef<SessionStream | null>(null);

  useEffect(() => {
    let cancelled = false;
    setPhase({ status: "loading" });

    endpoints
      .fetchSnapshot(sessionId)
      .then((snapshotSummary) => {
        if (cancelled) return;
        if (snapshotSummary === null) {
          setPhase({ status: "signed-out" });
          return;
        }
        store.getState().seedFromSnapshot(snapshotSummary);
        // Wire the real HTTP catch-up client — without this the controller's
        // default `fetchEventsSince` silently resolves `[]`, and the drop ->
        // catching-up -> HTTP catch-up -> fold -> fresh WS choreography never
        // actually recovers the missed-event gap (caught by the E2E lane).
        const stream = createSessionStream(sessionId, store, {
          fetchEventsSince: endpoints.fetchEventsSince,
          basePath: endpoints.wsBasePath(sessionId),
        });
        streamRef.current = stream;
        stream.start();
        setPhase({ status: "ready" });
      })
      .catch((error: unknown) => {
        if (cancelled) return;
        setPhase({
          status: "error",
          problem: error instanceof ProblemError ? error.problem : undefined,
        });
      });

    return () => {
      cancelled = true;
      streamRef.current?.stop();
      streamRef.current = null;
    };
  }, [sessionId, store, endpoints]);

  const resync = useCallback(() => {
    streamRef.current?.resync();
  }, []);

  return { store, phase, resync };
}
