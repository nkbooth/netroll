// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
/**
 * The WebSocket client + reconnect controller. Drives the session store
 * from the live stream and self-recovers on disconnect, composing BOTH shipped
 * resume seams in the architecture's canonical order:
 *
 * drop → `catching-up` → HTTP catch-up (`GET …/events?since=lastSeq`) → fold →
 * fresh WS `?since=M` → `live`
 *
 * with exponential backoff + jitter between attempts. A WS **1008** close means
 * the cursor is unrecoverable (invalid `since`, ownership revoked, or session
 * gone), so the cursor is dropped and a fresh snapshot is re-fetched rather than
 * retried with the same stale cursor. Persistent failure past `maxAttempts`
 * surfaces `out-of-sync` (last-known roster + a resync button) — never a manual
 * page-refresh prompt.
 *
 * The stream is structurally read-only: the client never sends a data frame.
 * Every collaborator (socket factory, catch-up fetch, timer, RNG) is injectable
 * so the controller is deterministically unit-testable against a fake socket.
 */

import { nextBackoff } from "./backoff";
import type { SessionStore } from "./sessionStore";
import type { EventMessage, ServerMessage, WireEvent } from "./sessionWire";
import type { StoreApi } from "zustand/vanilla";

/**
 * The minimal socket surface the controller drives. The default factory adapts
 * the browser `WebSocket`; tests supply a hand-driven fake.
 */
export interface StreamSocket {
  onopen: (() => void) | null;
  onmessage: ((data: string) => void) | null;
  onclose: ((code: number) => void) | null;
  close(): void;
}

/** Injectable collaborators (all default to real browser implementations). */
export interface SessionStreamDeps {
  socketFactory?: (path: string) => StreamSocket;
  fetchEventsSince?: (id: string, since: number) => Promise<WireEvent[]>;
  scheduleTimeout?: (fn: () => void | Promise<void>, ms: number) => () => void;
  rng?: () => number;
  maxAttempts?: number;
  /**
   * The WS base path. Defaults to the owner stream
   * (`/api/net-sessions/{id}/ws`); the public live view passes the account-less
   * `/api/net-sessions/{id}/live/ws` so the SAME reconnect choreography drives
   * both — the only difference is the endpoint, never the logic.
   */
  basePath?: string;
}

/** The controller handle bound to a component by `useSessionStream`. */
export interface SessionStream {
  start(): void;
  stop(): void;
  /** Manually restart the recovery choreography (the `out-of-sync` resync button). */
  resync(): void;
}

/** How a reconnect resumes: `resume` composes HTTP catch-up; `fresh` re-snapshots. */
type ReconnectMode = "resume" | "fresh";

/** Converts a same-origin path to a `ws`/`wss` absolute URL for `new WebSocket`. */
export function toWebSocketUrl(
  path: string,
  origin: string = window.location.origin,
): string {
  const url = new URL(path, origin);
  url.protocol = url.protocol === "https:" ? "wss:" : "ws:";
  return url.toString();
}

function defaultSocketFactory(path: string): StreamSocket {
  const ws = new WebSocket(toWebSocketUrl(path));
  const socket: StreamSocket = {
    onopen: null,
    onmessage: null,
    onclose: null,
    close: () => ws.close(),
  };
  ws.onopen = () => socket.onopen?.();
  ws.onmessage = (event) =>
    socket.onmessage?.(typeof event.data === "string" ? event.data : "");
  // An error is always followed by a close; the close handler owns recovery.
  ws.onclose = (event) => socket.onclose?.(event.code);
  return socket;
}

function defaultScheduleTimeout(
  fn: () => void | Promise<void>,
  ms: number,
): () => void {
  const id = window.setTimeout(() => void fn(), ms);
  return () => window.clearTimeout(id);
}

/** The WS policy-violation close code the backend uses for an invalid cursor. */
const CLOSE_POLICY_VIOLATION = 1008;

/** Strips the frame discriminator, yielding the shared `WireEvent` element. */
function frameToEvent(frame: EventMessage): WireEvent {
  const { type: _type, ...event } = frame;
  return event as WireEvent;
}

/**
 * Creates a reconnecting session stream bound to `store`. Call `start()` on
 * mount and `stop()` on unmount (idempotent, StrictMode-double-invoke safe).
 */
export function createSessionStream(
  sessionId: string,
  store: StoreApi<SessionStore>,
  deps: SessionStreamDeps = {},
): SessionStream {
  const socketFactory = deps.socketFactory ?? defaultSocketFactory;
  const fetchEventsSince = deps.fetchEventsSince ?? (() => Promise.resolve([]));
  const scheduleTimeout = deps.scheduleTimeout ?? defaultScheduleTimeout;
  const rng = deps.rng ?? Math.random;
  const maxAttempts = deps.maxAttempts ?? 5;

  const basePath = deps.basePath ?? `/api/net-sessions/${sessionId}/ws`;

  let stopped = false;
  let attempt = 0;
  let socket: StreamSocket | null = null;
  let cancelTimer: (() => void) | null = null;

  const clearTimer = (): void => {
    cancelTimer?.();
    cancelTimer = null;
  };

  const onFrame = (data: string): void => {
    if (stopped) return;
    // The frame is untrusted network input, not an internal invariant: a
    // single malformed payload must not crash the live view. Drop it and keep
    // the socket open — the connection's own close/error path (not a parse
    // failure) is what should ever trigger a reconnect.
    let message: ServerMessage;
    try {
      message = JSON.parse(data) as ServerMessage;
    } catch (error: unknown) {
      console.error("session stream: dropped an unparseable frame", error);
      return;
    }
    if (message.type === "snapshot") {
      store.getState().seedFromSnapshot(message.session);
    } else if (message.type === "lock") {
      // The ephemeral soft-lock frame: route it to the separate
      // `locks` slice. It carries NO `seq`, so it NEVER advances the resume
      // cursor (`lastSeq`) — it is not folded and not an event.
      store.getState().applyLock({
        checkInId: message.checkInId,
        holderCallsign: message.holderCallsign,
        expiresAt: message.expiresAt,
      });
    } else {
      store.getState().applyEvent(frameToEvent(message));
    }
    // Any successful frame proves we are current: return to live, reset backoff.
    attempt = 0;
    if (store.getState().connection !== "live") {
      store.getState().setConnection("live");
    }
  };

  const connect = (since: number | null): void => {
    if (stopped) return;
    const path = since !== null ? `${basePath}?since=${since}` : basePath;
    const next = socketFactory(path);
    socket = next;
    next.onopen = () => {
      if (stopped) return;
      // A resume socket (?since=M) opens already caught up (the HTTP gap was
      // folded first) → go live immediately. A fresh socket waits for its
      // snapshot before it can be considered live.
      if (since !== null) {
        attempt = 0;
        store.getState().setConnection("live");
      }
    };
    next.onmessage = onFrame;
    next.onclose = (code) => {
      if (stopped || socket !== next) return;
      socket = null;
      scheduleReconnect(code === CLOSE_POLICY_VIOLATION ? "fresh" : "resume");
    };
  };

  const scheduleReconnect = (mode: ReconnectMode): void => {
    if (stopped) return;
    attempt += 1;
    store.getState().setConnection("catching-up");
    if (attempt > maxAttempts) {
      // Never silently stuck, never a raw "refresh the page" — a recoverable
      // out-of-sync state with a manual resync affordance.
      store.getState().setConnection("out-of-sync");
      return;
    }
    clearTimer();
    cancelTimer = scheduleTimeout(() => runReconnect(mode), nextBackoff(attempt, rng));
  };

  const runReconnect = async (mode: ReconnectMode): Promise<void> => {
    cancelTimer = null;
    if (stopped) return;
    if (mode === "fresh") {
      // 1008 / invalid cursor: re-snapshot from scratch, no HTTP catch-up.
      connect(null);
      return;
    }
    try {
      const gap = await fetchEventsSince(sessionId, store.getState().session.lastSeq);
      if (stopped) return;
      store.getState().applyCatchUp(gap);
      connect(store.getState().session.lastSeq);
    } catch {
      if (stopped) return;
      // Catch-up itself failed — back off and try again; enough failures
      // eventually cross maxAttempts into out-of-sync.
      scheduleReconnect("resume");
    }
  };

  return {
    start: () => {
      if (!stopped && socket === null && cancelTimer === null) {
        connect(null);
      }
    },
    stop: () => {
      stopped = true;
      clearTimer();
      socket?.close();
      socket = null;
    },
    resync: () => {
      if (stopped) return;
      attempt = 0;
      clearTimer();
      scheduleReconnect("resume");
    },
  };
}
