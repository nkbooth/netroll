// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
import { describe, expect, it } from "vitest";

import type {
  CheckinAddedEvent,
  SessionClosedEvent,
  SessionStartedEvent,
  SessionSummaryBody,
  WireEvent,
} from "./sessionWire";

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


/**
 * These tests pin the TWO DISTINCT optionality conventions the shipped serde
 * uses at the value level — not the rendered text. `actorId` and
 * `clientEventId` are OMITTED when absent (the key is not present at all);
 * `startedAt`/`closedAt`/`durationSeconds`/`addedBy` are NULL when absent (the
 * key is present with value `null`). Conflating the two is the exact drift
 * this suite guards against.
 */

describe("sessionWire optionality conventions", () => {
  it("omits actorId entirely when the event has no actor", () => {
    // A system-originated (actor-less) close event: `actorId` is not a key.
    const closed: SessionClosedEvent = {
      seq: 2,
      kind: "session.closed",
      at: "2026-07-16T00:00:00Z",
      payload: {},
    };

    expect("actorId" in closed).toBe(false);
    expect(closed.actorId).toBeUndefined();
  });

  it("omits clientEventId entirely when a check-in carries no optimistic id", () => {
    const checkin: CheckinAddedEvent = {
      seq: 3,
      kind: "checkin.added",
      actorId: "00000000-0000-0000-0000-0000000000c8",
      at: "2026-07-16T00:00:01Z",
      payload: {
        checkInId: "00000000-0000-0000-0000-00000000002a",
        callsign: "W1AW",
      },
    };

    expect("clientEventId" in checkin.payload).toBe(false);
    expect(checkin.payload.clientEventId).toBeUndefined();
  });

  it("carries clientEventId as a present key when the check-in echoes one", () => {
    const checkin: CheckinAddedEvent = {
      seq: 4,
      kind: "checkin.added",
      at: "2026-07-16T00:00:02Z",
      payload: {
        checkInId: "00000000-0000-0000-0000-00000000002b",
        callsign: "N1CCK",
        clientEventId: "00000000-0000-0000-0000-000000000009",
      },
    };

    expect(checkin.payload.clientEventId).toBe(
      "00000000-0000-0000-0000-000000000009",
    );
  });

  it("models null-when-absent summary optionals as present keys valued null", () => {
    // A live, never-closed session: startedAt is a present string, but
    // closedAt / durationSeconds are present keys valued `null` (not omitted).
    const summary: SessionSummaryBody = {
      id: "00000000-0000-0000-0000-000000000001",
      definitionId: "00000000-0000-0000-0000-000000000007",
      definitionVersion: 3,
      lifecycle: "live",
      connections: [hfWay(14_250_000)],
      startedAt: "2026-07-16T00:00:00Z",
      closedAt: null,
      durationSeconds: null,
      latestSeq: 1,
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
    };

    expect("closedAt" in summary).toBe(true);
    expect(summary.closedAt).toBeNull();
    expect(summary.durationSeconds).toBeNull();
  });

  it("models a null-when-absent roster addedBy as a present key valued null", () => {
    const summary: SessionSummaryBody = {
      id: "00000000-0000-0000-0000-000000000001",
      definitionId: "00000000-0000-0000-0000-000000000007",
      definitionVersion: 3,
      lifecycle: "live",
      connections: [hfWay(14_250_000)],
      startedAt: "2026-07-16T00:00:00Z",
      closedAt: null,
      durationSeconds: null,
      latestSeq: 2,
      participantCount: 1,
      workingCheckInId: null,
      netNote: null,
      controlState: "active",
      roster: [
        {
          checkInId: "00000000-0000-0000-0000-00000000002a",
          callsign: "W1AW",
          addedAt: "2026-07-16T00:00:01Z",
          addedBy: null,
          source: "staff",
          staying: "in-and-out",
          precedence: "routine",
          worked: false,
          version: 1,
          corrections: [],
        },
      ],
      definition: {
        title: "Sunday Traffic Net",
        connections: [hfWay()],
        netCategory: "traffic",
        netType: "open",
      },
    };

    expect("addedBy" in summary.roster[0]).toBe(true);
    expect(summary.roster[0].addedBy).toBeNull();
  });

  it("carries the viewer's own role on the staff summary, omitted on the public one", () => {
    // The staff summary carries viewerRole; the redacted public
    // summary type has no such field (modeled optional so both shapes seed the
    // same store, but the staff wire always sends it).
    const staff: SessionSummaryBody = {
      id: "00000000-0000-0000-0000-000000000001",
      definitionId: "00000000-0000-0000-0000-000000000007",
      definitionVersion: 3,
      lifecycle: "live",
      connections: [hfWay(14_250_000)],
      startedAt: "2026-07-16T00:00:00Z",
      closedAt: null,
      durationSeconds: null,
      latestSeq: 1,
      participantCount: 0,
      workingCheckInId: null,
      netNote: null,
      controlState: "active",
      viewerRole: "relay",
      roster: [],
      definition: {
        title: "Sunday Traffic Net",
        connections: [hfWay()],
        netCategory: "traffic",
        netType: "open",
      },
    };

    expect(staff.viewerRole).toBe("relay");
  });

  it("narrows a WireEvent union on its kind discriminator", () => {
    const started: SessionStartedEvent = {
      seq: 1,
      kind: "session.started",
      actorId: "00000000-0000-0000-0000-000000000064",
      at: "2026-07-16T00:00:00Z",
      payload: {
        definitionId: "00000000-0000-0000-0000-000000000007",
        definitionVersion: 3,
      },
    };

    const event: WireEvent = started;
    // The union discriminates on `kind`; narrowing exposes the started payload.
    expect(event.kind === "session.started" ? event.payload.definitionVersion : -1).toBe(3);
  });
});
