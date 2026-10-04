// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
import { describe, expect, it, vi } from "vitest";

import { createSessionStream, toWebSocketUrl } from "./sessionSocket";
import { createSessionStore } from "./sessionStore";
import type { StreamSocket } from "./sessionSocket";
import type { ServerMessage, WireEvent } from "./sessionWire";

// A session carries its ways IN, not one frequency. The fixtures
// below reach the net on one HF connection unless a test says otherwise.
function hfWay(plannedFrequencyHz = 14_230_000) {
  return {
    id: "00000000-0000-0000-0000-000000001604",
    position: 0,
    kind: "hf",
    plannedFrequencyHz,
    band: "20m",
    mode: "ssb",
    repeaterOffsetHz: null,
    toneMode: null,
    toneValue: null,
    node: null,
    reflector: null,
    network: null,
    talkgroup: null,
    label: null,
    detail: null,
  };
}


const uuid = (n: number): string =>
  `00000000-0000-0000-0000-${n.toString(16).padStart(12, "0")}`;

function snapshotFrame(latestSeq: number): ServerMessage {
  return {
    type: "snapshot",
    session: {
      id: uuid(1),
      definitionId: uuid(7),
      definitionVersion: 3,
      lifecycle: "live",
      connections: [hfWay(14_250_000)],
      startedAt: "2026-07-16T00:00:00Z",
      closedAt: null,
      durationSeconds: null,
      latestSeq,
      participantCount: 0,
      workingCheckInId: null,
      netNote: null,
      controlState: "active",
      roster: [],
      definition: {
        title: "Sunday Traffic Net",
        connections: [hfWay()],
        netCategory: "traffic",
        netType: "open",
      },
    },
  };
}

function checkinFrame(seq: number, id: number, call: string): ServerMessage {
  return {
    type: "event",
    seq,
    kind: "checkin.added",
    actorId: uuid(200),
    at: "2026-07-16T00:00:05Z",
    payload: { checkInId: uuid(id), callsign: call },
  };
}

function lockFrame(id: number, holder: string | null): ServerMessage {
  return {
    type: "lock",
    checkInId: uuid(id),
    holderCallsign: holder,
    expiresAt: holder === null ? null : "2026-07-16T00:00:15Z",
  };
}

function checkinEvent(seq: number, id: number, call: string): WireEvent {
  return {
    seq,
    kind: "checkin.added",
    actorId: uuid(200),
    at: "2026-07-16T00:00:05Z",
    payload: { checkInId: uuid(id), callsign: call },
  };
}

/** A hand-driven StreamSocket the test opens/messages/closes explicitly. */
class FakeSocket implements StreamSocket {
  onopen: (() => void) | null = null;
  onmessage: ((data: string) => void) | null = null;
  onclose: ((code: number) => void) | null = null;
  closed = false;
  readonly url: string;

  constructor(url: string) {
    this.url = url;
  }

  emitOpen(): void {
    this.onopen?.();
  }
  emitMessage(frame: ServerMessage): void {
    this.onmessage?.(JSON.stringify(frame));
  }
  emitClose(code: number): void {
    this.onclose?.(code);
  }
  close(): void {
    this.closed = true;
  }
}

/** A fake scheduler that captures the pending callback for manual flushing. */
function fakeScheduler() {
  const queue: Array<() => void | Promise<void>> = [];
  const scheduleTimeout = (fn: () => void | Promise<void>): (() => void) => {
    queue.push(fn);
    return () => {
      const idx = queue.indexOf(fn);
      if (idx >= 0) queue.splice(idx, 1);
    };
  };
  const flush = async (): Promise<void> => {
    const pending = queue.splice(0, queue.length);
    for (const fn of pending) {
      await fn();
    }
  };
  return { scheduleTimeout, flush, size: () => queue.length };
}

function harness(
  opts: {
    fetchEventsSince?: (id: string, since: number) => Promise<WireEvent[]>;
    maxAttempts?: number;
  } = {},
) {
  const sockets: FakeSocket[] = [];
  const socketFactory = (url: string): FakeSocket => {
    const s = new FakeSocket(url);
    sockets.push(s);
    return s;
  };
  const scheduler = fakeScheduler();
  const store = createSessionStore();
  const stream = createSessionStream(uuid(1), store, {
    socketFactory,
    fetchEventsSince: opts.fetchEventsSince ?? (async () => []),
    scheduleTimeout: scheduler.scheduleTimeout,
    rng: () => 0.5,
    maxAttempts: opts.maxAttempts ?? 5,
  });
  return { sockets, scheduler, store, stream, last: () => sockets[sockets.length - 1] };
}

describe("toWebSocketUrl", () => {
  it("converts a same-origin path to a ws(s) absolute URL", () => {
    expect(toWebSocketUrl("/api/net-sessions/abc/ws", "http://localhost:3000")).toBe(
      "ws://localhost:3000/api/net-sessions/abc/ws",
    );
    expect(toWebSocketUrl("/api/net-sessions/abc/ws?since=5", "https://netroll.app")).toBe(
      "wss://netroll.app/api/net-sessions/abc/ws?since=5",
    );
  });
});

describe("createSessionStream", () => {
  it("opens a fresh WS with no ?since and seeds+goes live on the snapshot", () => {
    const { last, store, stream } = harness();
    stream.start();

    expect(last().url).toBe(`/api/net-sessions/${uuid(1)}/ws`);
    last().emitOpen();
    last().emitMessage(snapshotFrame(1));

    expect(store.getState().connection).toBe("live");
    expect(store.getState().session.lastSeq).toBe(1);
  });

  it("applies live event deltas through the reducer after seeding", () => {
    const { last, store, stream } = harness();
    stream.start();
    last().emitMessage(snapshotFrame(1));
    last().emitMessage(checkinFrame(2, 42, "W1AW"));

    expect(store.getState().session.roster).toHaveLength(1);
    expect(store.getState().session.lastSeq).toBe(2);
  });

  it("drops a delta whose seq does not advance the cursor", () => {
    const { last, store, stream } = harness();
    stream.start();
    last().emitMessage(snapshotFrame(5));
    last().emitMessage(checkinFrame(3, 42, "W1AW"));

    expect(store.getState().session.roster).toHaveLength(0);
    expect(store.getState().session.lastSeq).toBe(5);
  });

  it("recovers a dropped socket: catching-up → HTTP catch-up → resume WS ?since=M → live", async () => {
    const fetchEventsSince = vi.fn(async () => [checkinEvent(2, 42, "W1AW")]);
    const { last, sockets, scheduler, store, stream } = harness({ fetchEventsSince });
    stream.start();
    last().emitMessage(snapshotFrame(1));
    expect(store.getState().connection).toBe("live");

    // Socket drops (normal close, not 1008).
    last().emitClose(1006);
    expect(store.getState().connection).toBe("catching-up");

    // Flush the backoff timer → HTTP catch-up runs and folds the gap.
    await scheduler.flush();
    expect(fetchEventsSince).toHaveBeenCalledWith(uuid(1), 1);
    expect(store.getState().session.lastSeq).toBe(2);

    // A fresh resume WS opened at ?since=2, and opening it returns us to live.
    const resume = sockets[sockets.length - 1];
    expect(resume.url).toContain("?since=2");
    resume.emitOpen();
    expect(store.getState().connection).toBe("live");
  });

  it("observes each event exactly once across the HTTP→WS boundary", async () => {
    const fetchEventsSince = vi.fn(async () => [checkinEvent(2, 42, "W1AW")]);
    const { last, sockets, scheduler, store, stream } = harness({ fetchEventsSince });
    stream.start();
    last().emitMessage(snapshotFrame(1));
    last().emitClose(1006);
    await scheduler.flush();

    const resume = sockets[sockets.length - 1];
    resume.emitOpen();
    // First live frame after resume is seq M+1 = 3; the gap event (seq 2) was
    // folded over HTTP, so the roster carries each check-in exactly once.
    resume.emitMessage(checkinFrame(3, 43, "N1CCK"));

    expect(store.getState().session.roster.map((r) => r.callsign)).toEqual([
      "W1AW",
      "N1CCK",
    ]);
    expect(store.getState().session.lastSeq).toBe(3);
  });

  it("on a 1008 close drops the cursor and reconnects fresh (no ?since)", async () => {
    const fetchEventsSince = vi.fn(async () => []);
    const { last, sockets, scheduler, store, stream } = harness({ fetchEventsSince });
    stream.start();
    last().emitMessage(snapshotFrame(5));

    last().emitClose(1008);
    expect(store.getState().connection).toBe("catching-up");
    await scheduler.flush();

    // 1008 = invalid cursor → re-snapshot: no HTTP catch-up, fresh WS w/o since.
    expect(fetchEventsSince).not.toHaveBeenCalled();
    const reconnect = sockets[sockets.length - 1];
    expect(reconnect.url).toBe(`/api/net-sessions/${uuid(1)}/ws`);
  });

  it("surfaces out-of-sync after repeated recovery failures", async () => {
    const fetchEventsSince = vi.fn(async () => {
      throw new Error("network down");
    });
    const { last, scheduler, store, stream } = harness({
      fetchEventsSince,
      maxAttempts: 3,
    });
    stream.start();
    last().emitMessage(snapshotFrame(1));
    last().emitClose(1006);

    // Each flush retries catch-up and fails; after maxAttempts → out-of-sync.
    for (let i = 0; i < 5 && store.getState().connection !== "out-of-sync"; i += 1) {
      await scheduler.flush();
    }
    expect(store.getState().connection).toBe("out-of-sync");
  });

  it("resync restarts the choreography from out-of-sync", async () => {
    // Catch-up fails first (driving us to out-of-sync), then succeeds so the
    // resync-restarted choreography can re-open a resume socket.
    const fetchEventsSince = vi
      .fn<(id: string, since: number) => Promise<WireEvent[]>>()
      .mockRejectedValueOnce(new Error("network down"))
      .mockResolvedValue([]);
    const { last, sockets, scheduler, store, stream } = harness({
      fetchEventsSince,
      maxAttempts: 1,
    });
    stream.start();
    last().emitMessage(snapshotFrame(1));
    last().emitClose(1006);
    await scheduler.flush();
    expect(store.getState().connection).toBe("out-of-sync");

    const before = sockets.length;
    stream.resync();
    expect(store.getState().connection).toBe("catching-up");
    await scheduler.flush();
    // A new resume socket was opened by the restarted choreography.
    expect(sockets.length).toBeGreaterThan(before);
  });

  it("stop closes the socket and prevents further reconnects", async () => {
    const { last, scheduler, sockets, stream } = harness();
    stream.start();
    last().emitMessage(snapshotFrame(1));
    const openSocket = last();
    stream.stop();
    expect(openSocket.closed).toBe(true);

    // A drop after stop must not schedule a reconnect.
    openSocket.emitClose(1006);
    await scheduler.flush();
    expect(sockets).toHaveLength(1);
  });
  it("routes a lock frame to the ephemeral locks slice WITHOUT advancing the cursor", () => {
    const { last, store, stream } = harness();
    stream.start();
    last().emitMessage(snapshotFrame(1));
    last().emitMessage(lockFrame(42, "W2BCD"));

    // The lock landed in the ephemeral slice, not the fold.
    expect(store.getState().locks.get(uuid(42))?.holderCallsign).toBe("W2BCD");
    // A lock frame carries no seq — the resume cursor is unchanged.
    expect(store.getState().session.lastSeq).toBe(1);

    // A release frame drops it.
    last().emitMessage(lockFrame(42, null));
    expect(store.getState().locks.has(uuid(42))).toBe(false);
  });
});

