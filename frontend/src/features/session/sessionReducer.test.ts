// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
import { describe, expect, it } from "vitest";

import {
  DEFAULT_SESSION_STATE,
  fold,
  replay,
} from "./sessionReducer";
import type { ViaWire } from "../nets/connectionPresentation";
import { buildRoster } from "./sessionStore";
import type {
  CheckinAddedEvent,
  FrequencyChangedEvent,
  SessionClosedEvent,
  SessionStartedEvent,
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
 * These mirror the Rust `fold.rs` proptest/unit cases BYTE-FOR-BYTE in intent
 * Idempotency guard first, scalars last-write-wins, roster
 * append-with-dedupe keyed on `checkInId` (NOT `clientEventId`), `lastSeq`
 * advance, and totality on illegal orderings. Assertions are on STATE
 * transitions, never rendered text (house TDD rule).
 */

const uuid = (n: number): string =>
  `00000000-0000-0000-0000-${n.toString(16).padStart(12, "0")}`;

function started(seq: number, at: string, _hz: number): SessionStartedEvent {
  return {
    seq,
    kind: "session.started",
    actorId: uuid(100),
    at,
    payload: {
      definitionId: uuid(7),
      definitionVersion: 3,
    },
  };
}

function freq(
  seq: number,
  at: string,
  connectionId: string,
  hz: number,
): FrequencyChangedEvent {
  return {
    seq,
    kind: "frequency.changed",
    at,
    payload: { connectionId, operatingFrequencyHz: hz },
  };
}

// `actorId: null` forces an OMITTED actor (no key); the default supplies one.
function checkin(
  seq: number,
  at: string,
  id: number,
  call: string,
  actorId: string | null = uuid(200),
): CheckinAddedEvent {
  const base: CheckinAddedEvent = {
    seq,
    kind: "checkin.added",
    at,
    payload: { checkInId: uuid(id), callsign: call },
  };
  return actorId === null ? base : { ...base, actorId };
}

function closed(seq: number, at: string): SessionClosedEvent {
  return { seq, kind: "session.closed", actorId: uuid(100), at, payload: {} };
}

function updated(
  seq: number,
  at: string,
  id: number,
  payload: Partial<{ callsign: string; name: string; location: string; grid: string; signalReport: string; staying: "staying-for-comments" | "in-and-out"; precedence: "routine" | "priority" | "emergency"; traffic: number; via: ViaWire }> & { callsign: string },
): WireEvent {
  // precedence is a required wire field on checkin.updated — default it to
  // "routine" so callers exercising only the older fields stay unchanged.
  return {
    seq,
    kind: "checkin.updated",
    actorId: uuid(300),
    at,
    payload: { checkInId: uuid(id), precedence: "routine", ...payload },
  };
}

function removed(seq: number, at: string, id: number): WireEvent {
  return { seq, kind: "checkin.removed", actorId: uuid(300), at, payload: { checkInId: uuid(id) } };
}

function blocked(seq: number, at: string, accountId: number): WireEvent {
  return {
    seq,
    kind: "station.blocked",
    actorId: uuid(300),
    at,
    payload: { accountId: uuid(accountId) },
  };
}

function reordered(seq: number, at: string, order: number[]): WireEvent {
  return {
    seq,
    kind: "roster.reordered",
    actorId: uuid(300),
    at,
    payload: { order: order.map(uuid) },
  };
}

function workedSet(seq: number, at: string, id: number | null): WireEvent {
  return {
    seq,
    kind: "station.worked-set",
    actorId: uuid(300),
    at,
    payload: { checkInId: id === null ? null : uuid(id) },
  };
}

function noteSet(seq: number, at: string, note: string | null): WireEvent {
  const base = { seq, kind: "session.note-set" as const, actorId: uuid(300), at };
  // The wire omits `note` when cleared (omit-optional); include it only when set.
  return note === null
    ? ({ ...base, payload: {} } as WireEvent)
    : ({ ...base, payload: { note } } as WireEvent);
}

function orderModeSet(seq: number, at: string, mode: string | null): WireEvent {
  const base = { seq, kind: "roster.order-mode-set" as const, actorId: uuid(300), at };
  // An absent `mode` (`null` here) is a shape the reducer must stay total over.
  // It was once also what the PUBLIC wire projected for this kind;
  // the public delta now carries the token (see the test below), so an
  // absent key is a defensive case rather than an expected one.
  return mode === null
    ? ({ ...base, payload: {} } as unknown as WireEvent)
    : ({ ...base, payload: { mode } } as unknown as WireEvent);
}

describe("sessionReducer.fold", () => {
  it("default state is empty, scheduled, with a zero cursor", () => {
    const s = DEFAULT_SESSION_STATE;
    expect(s.lifecycle).toBe("scheduled");
    expect(s.lastSeq).toBe(0);
    expect(s.roster).toEqual([]);
    expect(s.startedAt).toBeNull();
    expect(s.connections).toEqual([]);
  });

  it("session.started sets live, start instant, and provenance", () => {
    const s = fold(DEFAULT_SESSION_STATE, started(1, "2026-07-16T00:00:01Z", 14_250_000));
    expect(s.lifecycle).toBe("live");
    expect(s.startedAt).toBe("2026-07-16T00:00:01Z");
    expect(s.definitionId).toBe(uuid(7));
    expect(s.definitionVersion).toBe(3);
    expect(s.lastSeq).toBe(1);
  });

  it("frequency.changed moves only the connection it names", () => {
    // A net with two ways in retunes ONE of them. Before the
    // event named its connection, this reducer had a single scalar and the
    // other way silently inherited the number.
    const hf = hfWay();
    const repeater = { ...hfWay(146_940_000), id: uuid(0x99), position: 1, kind: "repeater" };
    const seeded = {
      ...DEFAULT_SESSION_STATE,
      connections: [hf, repeater],
    };
    const s = [started(1, "a", 14_250_000), freq(2, "b", hf.id, 7_200_000)].reduce(
      fold,
      seeded,
    );
    expect(s.connections[0]?.plannedFrequencyHz).toBe(7_200_000);
    expect(s.connections[1]?.plannedFrequencyHz).toBe(146_940_000);
    expect(s.lifecycle).toBe("live");
    expect(s.lastSeq).toBe(2);
  });

  it("checkin.added appends one roster row with envelope provenance", () => {
    const s = replay(
      [started(1, "a", 14_250_000), checkin(2, "2026-07-16T00:00:02Z", 42, "W1AW")],
      0,
    );
    expect(s.roster).toHaveLength(1);
    expect(s.roster[0]).toEqual({
      checkInId: uuid(42),
      callsign: "W1AW",
      addedAt: "2026-07-16T00:00:02Z",
      addedBy: uuid(200),
      source: "staff",
      signalReport: null,
      staying: "in-and-out",
      name: null,
      location: null,
      grid: null,
      precedence: "routine",
      traffic: null,
      notes: null,
      // Both notes are edit-only, so an add carries neither. This is
      // an EXHAUSTIVE `toEqual`, deliberately — a new roster field that nobody
      // decided about fails right here.
      publicNote: null,
      // `via` IS an add-time field, unlike the eight edit-only
      // ones — this add simply carried no key, which means nobody recorded a
      // way in and never means "the net's first connection".
      via: null,
      // `relayed_by` is an add-time field too, and an add with no
      // key folds to null — NOT RELAYED.
      relayedBy: null,
      worked: false,
      version: 1,
      corrections: [],
    });
  });

  it("checkin.added folds source (self/staff) and defaults an omitted source to staff", () => {
    // The self path is the only producer of `source: "self"`; a staff add or
    // an omitted-source event folds to `staff` (additive-compat).
    const selfAdd: CheckinAddedEvent = {
      seq: 2,
      kind: "checkin.added",
      at: "b",
      actorId: uuid(200),
      payload: { checkInId: uuid(43), callsign: "N1CCK", source: "self" },
    };
    const legacyAdd: CheckinAddedEvent = {
      seq: 3,
      kind: "checkin.added",
      at: "c",
      actorId: uuid(201),
      payload: { checkInId: uuid(44), callsign: "K1XYZ" },
    };
    const s = replay([started(1, "a", 14_250_000), selfAdd, legacyAdd], 0);
    expect(s.roster[0].source).toBe("self");
    expect(s.roster[1].source).toBe("staff");
  });

  it("checkin.added projects signalReport and staying onto the roster row", () => {
    const withFields: CheckinAddedEvent = {
      seq: 2,
      kind: "checkin.added",
      at: "b",
      actorId: uuid(200),
      payload: {
        checkInId: uuid(42),
        callsign: "W1AW",
        signalReport: "599",
        staying: "staying-for-comments",
      },
    };
    const s = replay([started(1, "a", 14_250_000), withFields], 0);
    expect(s.roster[0].signalReport).toBe("599");
    expect(s.roster[0].staying).toBe("staying-for-comments");
  });

  it("checkin.added projects name and location onto the roster row", () => {
    // The prefilled identity committed at add time is folded verbatim onto the
    // roster entry (no longer hard-coded null) — the client mirror of the Rust
    // fold. A field-less add still folds them to null (below).
    const withIdentity: CheckinAddedEvent = {
      seq: 2,
      kind: "checkin.added",
      at: "b",
      actorId: uuid(200),
      payload: {
        checkInId: uuid(42),
        callsign: "W1AW",
        name: "Maria",
        location: "Hartford, CT",
      },
    };
    const s = replay([started(1, "a", 14_250_000), withIdentity], 0);
    expect(s.roster[0].name).toBe("Maria");
    expect(s.roster[0].location).toBe("Hartford, CT");
    expect(s.roster[0].version).toBe(1);
    expect(s.roster[0].corrections).toEqual([]);
  });

  it("checkin.added folds absent name/location/grid to null (additive-compat)", () => {
    // `checkin` builds a callsign-only payload with NO name/location/grid keys —
    // exactly the shape an older delta (or a redacted public delta) carries.
    const event = checkin(2, "2026-07-16T00:00:02Z", 42, "W1AW");
    expect(event.payload).not.toHaveProperty("grid");
    const s = replay([started(1, "a", 14_250_000), event], 0);
    expect(s.roster[0].name).toBeNull();
    expect(s.roster[0].location).toBeNull();
    expect(s.roster[0].grid).toBeNull();
  });

  it("checkin.added projects a grid onto the roster row, independent of location", () => {
    // Two distinct fields on one add — a place name AND a locator.
    const withGrid: CheckinAddedEvent = {
      seq: 2,
      kind: "checkin.added",
      at: "b",
      actorId: uuid(200),
      payload: {
        checkInId: uuid(42),
        callsign: "W1AW",
        location: "Hartford, CT",
        grid: "FN31pr",
      },
    };
    const s = replay([started(1, "a", 14_250_000), withGrid], 0);
    expect(s.roster[0].location).toBe("Hartford, CT");
    expect(s.roster[0].grid).toBe("FN31pr");
    expect(s.roster[0].corrections).toEqual([]);
  });

  it("checkin.added defaults staying to in-and-out and signalReport to null when absent (HISTORICAL entry)", () => {
    // A HISTORICAL delta carries no staying key. A PUBLIC one does:
    // `ws/protocol.rs`'s public `checkin.added` arm projects `staying`, so an
    // observer's live add carries the operator's value and never reaches this
    // default.
    //
    // The OLD half is still true and is the whole of what this test now guards:
    // an older delta in the append-only log carries neither key, and the fold
    // must default them rather than leave them `undefined` (additive-compat).
    // `signalReport` is still absent on the public wire too — it is one of the
    // fields that stayed redacted — but that is not what this fixture is.
    const s = replay(
      [started(1, "a", 14_250_000), checkin(2, "b", 42, "W1AW")],
      0,
    );
    expect(s.roster[0].staying).toBe("in-and-out");
    expect(s.roster[0].signalReport).toBeNull();
  });

  it("a check-in with an omitted actorId folds addedBy to null", () => {
    // OMITTED actorId (no key) becomes the null-when-absent roster `addedBy`.
    const s = replay(
      [started(1, "a", 14_250_000), checkin(2, "b", 42, "W1AW", null)],
      0,
    );
    expect(s.roster[0].addedBy).toBeNull();
  });

  it("a duplicate checkInId at an advancing seq does not grow the roster", () => {
    const s = replay(
      [
        started(1, "a", 14_250_000),
        checkin(2, "b", 42, "W1AW"),
        checkin(3, "c", 42, "W1AW"),
      ],
      0,
    );
    expect(s.roster).toHaveLength(1);
    expect(s.lastSeq).toBe(3);
  });

  it("dedupe keys on checkInId, NOT clientEventId (distinct clientEventId, same checkInId)", () => {
    // Same entity id, different optimistic ids at advancing seqs: still one row.
    const first: CheckinAddedEvent = {
      seq: 2,
      kind: "checkin.added",
      at: "b",
      payload: { checkInId: uuid(42), callsign: "W1AW", clientEventId: uuid(1) },
    };
    const second: CheckinAddedEvent = {
      seq: 3,
      kind: "checkin.added",
      at: "c",
      payload: { checkInId: uuid(42), callsign: "W1AW", clientEventId: uuid(2) },
    };
    const s = replay([started(1, "a", 14_250_000), first, second], 0);
    expect(s.roster).toHaveLength(1);
  });

  it("distinct checkInIds preserve seq insertion order", () => {
    const s = replay(
      [
        started(1, "a", 14_250_000),
        checkin(2, "b", 42, "W1AW"),
        checkin(3, "c", 43, "N1CCK"),
      ],
      0,
    );
    expect(s.roster.map((r) => r.checkInId)).toEqual([uuid(42), uuid(43)]);
  });

  it("session.closed sets closed lifecycle and closed instant", () => {
    const s = replay([started(1, "a", 14_250_000), closed(2, "2026-07-16T01:00:00Z")], 0);
    expect(s.lifecycle).toBe("closed");
    expect(s.closedAt).toBe("2026-07-16T01:00:00Z");
    expect(s.lastSeq).toBe(2);
  });

  it("a non-advancing seq is an inert no-op for every kind", () => {
    const base = fold(DEFAULT_SESSION_STATE, started(5, "a", 14_250_000));
    expect(fold(base, freq(5, "z", hfWay().id, 7_200_000))).toEqual(base);
    expect(fold(base, checkin(2, "z", 1, "W1AW"))).toEqual(base);
    expect(fold(base, started(5, "z", 1_000_000))).toEqual(base);
    expect(fold(base, closed(3, "z"))).toEqual(base);
  });

  it("returns the SAME reference on an inert event (no needless copy)", () => {
    const base = fold(DEFAULT_SESSION_STATE, started(5, "a", 14_250_000));
    expect(fold(base, freq(5, "z", hfWay().id, 7_200_000))).toBe(base);
  });

  it("is total on illegal orderings — never throws, projects close-before-start", () => {
    const s = replay([closed(1, "x"), started(2, "y", 14_250_000)], 0);
    expect(s.closedAt).toBe("x");
    expect(s.lifecycle).toBe("live");
    expect(s.lastSeq).toBe(2);
  });

  it("does not mutate the input state (immutability)", () => {
    const before = { ...DEFAULT_SESSION_STATE, roster: [] };
    const snapshot = JSON.stringify(before);
    fold(before, started(1, "a", 14_250_000));
    expect(JSON.stringify(before)).toBe(snapshot);
  });

  it("folds a station.blocked as an advancing no-op that leaves the roster intact", () => {
    // The block is enforced server-side (the self-check-in 403) and its
    // roster effect rides the accompanying checkin.removed — the public reducer
    // needs NO state from station.blocked, so folding it must advance the cursor
    // without corrupting the roster, lifecycle, or working cursor.
    let s = fold(DEFAULT_SESSION_STATE, started(1, "2026-07-16T00:00:00Z", 14_250_000));
    s = fold(s, checkin(2, "2026-07-16T00:00:01Z", 42, "W1AW"));
    const before = s;
    const next = fold(s, blocked(3, "2026-07-16T00:00:02Z", 500));
    expect(next.lastSeq).toBe(3);
    expect(next.roster).toEqual(before.roster);
    expect(next.lifecycle).toBe(before.lifecycle);
    expect(next.workingCheckInId).toBe(before.workingCheckInId);
  });

  it("a non-advancing station.blocked is inert", () => {
    let s = fold(DEFAULT_SESSION_STATE, started(1, "a", 14_250_000));
    s = fold(s, checkin(2, "b", 42, "W1AW"));
    // seq <= lastSeq: absorbed as a no-op, same reference returned.
    expect(fold(s, blocked(2, "c", 500))).toBe(s);
  });

  it("absorbs an unrecognized future `kind` as an advancing no-op instead of throwing", () => {
    // Kind tokens are an additive-only server contract (sessionWire.ts docs):
    // an old client can receive a `kind` newer than its own union before it
    // redeploys. `as unknown as WireEvent` simulates that wire reality past
    // the type system, which cannot express "a kind I don't know about yet."
    const base = fold(DEFAULT_SESSION_STATE, started(1, "a", 14_250_000));
    const fromTheFuture = {
      seq: 2,
      kind: "net.paused",
      at: "z",
      payload: {},
    } as unknown as WireEvent;
    const next = fold(base, fromTheFuture);
    expect(next.lastSeq).toBe(2);
    expect(next.roster).toEqual(base.roster);
    expect(next.lifecycle).toBe(base.lifecycle);
  });
});

describe("sessionReducer.replay", () => {
  it("folds only events with seq > since (the resuming-client tail)", () => {
    const log: WireEvent[] = [
      started(1, "a", 14_250_000),
      checkin(2, "b", 42, "W1AW"),
      checkin(3, "c", 43, "N1CCK"),
    ];
    const tail = replay(log, 1);
    // Only seq > 1 applied: two check-ins, but no start ⇒ still scheduled.
    expect(tail.lifecycle).toBe("scheduled");
    expect(tail.roster).toHaveLength(2);
    expect(tail.lastSeq).toBe(3);
  });

  it("reapplying the whole log twice equals applying it once (idempotent)", () => {
    const log: WireEvent[] = [
      started(1, "a", 14_250_000),
      checkin(2, "b", 42, "W1AW"),
      freq(3, "c", hfWay().id, 7_200_000),
    ];
    const once = replay(log, 0);
    const twice = replay([...log, ...log], 0);
    expect(twice).toEqual(once);
  });

  it("folding the tail onto the prefix equals the full fold (composition)", () => {
    const log: WireEvent[] = [
      started(1, "a", 14_250_000),
      checkin(2, "b", 42, "W1AW"),
      checkin(3, "c", 43, "N1CCK"),
      closed(4, "d"),
    ];
    for (const since of [0, 1, 2, 3, 4]) {
      const whole = replay(log, 0);
      const prefix = replay(
        log.filter((e) => e.seq <= since),
        0,
      );
      const composed = log
        .filter((e) => e.seq > since)
        .reduce(fold, prefix);
      expect(composed).toEqual(whole);
      // The delta a resuming client applies equals replay(log, since).
      expect(replay(log, since)).toEqual(replay(log.filter((e) => e.seq > since), 0));
    }
  });
});

describe("sessionReducer.fold — checkin.updated / checkin.removed", () => {
  const base = () =>
    replay(
      [
        started(1, "2026-07-16T00:00:01Z", 14_250_000),
        checkin(2, "2026-07-16T00:00:02Z", 42, "W1AW"),
      ],
      0,
    );

  it("checkin.added starts at version 1 with no corrections and null name/location", () => {
    const s = base();
    expect(s.roster[0].version).toBe(1);
    expect(s.roster[0].corrections).toEqual([]);
    expect(s.roster[0].name).toBeNull();
    expect(s.roster[0].location).toBeNull();
  });

  it("checkin.updated replaces the editable fields and bumps the version", () => {
    const s = fold(
      base(),
      updated(3, "2026-07-16T00:00:03Z", 42, {
        callsign: "W1AX",
        name: "Maria",
        location: "Hartford, CT",
        signalReport: "599",
        staying: "staying-for-comments",
      }),
    );
    expect(s.roster).toHaveLength(1);
    expect(s.roster[0].callsign).toBe("W1AX");
    expect(s.roster[0].name).toBe("Maria");
    expect(s.roster[0].location).toBe("Hartford, CT");
    expect(s.roster[0].signalReport).toBe("599");
    expect(s.roster[0].staying).toBe("staying-for-comments");
    expect(s.roster[0].version).toBe(2);
  });

  it("derives a correction per CHANGED field only (old==new derives nothing)", () => {
    // Change callsign only; name/location stay absent, staying stays in-and-out.
    const s = fold(base(), updated(3, "2026-07-16T00:00:03Z", 42, { callsign: "W1AX" }));
    expect(s.roster[0].corrections).toEqual([
      { field: "callsign", from: "W1AW", to: "W1AX", at: "2026-07-16T00:00:03Z" },
    ]);
  });

  it("derives a grid correction between the location and signal-report ones", () => {
    // The derived-correction ORDER is the shared contract with the
    // Rust fold (callsign, name, location, grid, signal-report, staying, …).
    const s = fold(
      base(),
      updated(3, "2026-07-16T00:00:03Z", 42, {
        callsign: "W1AW",
        location: "Hartford, CT",
        grid: "FN31",
        signalReport: "599",
      }),
    );
    expect(s.roster[0].grid).toBe("FN31");
    expect(s.roster[0].corrections.map((c) => c.field)).toEqual([
      "location",
      "grid",
      "signal-report",
    ]);
    expect(s.roster[0].corrections[1]).toEqual({
      field: "grid",
      from: null,
      to: "FN31",
      at: "2026-07-16T00:00:03Z",
    });
  });

  it("an unchanged grid derives no correction, and clearing one derives a to-null", () => {
    const withGrid = fold(
      base(),
      updated(3, "2026-07-16T00:00:03Z", 42, { callsign: "W1AW", grid: "FN31" }),
    );
    // Re-sending the SAME grid must derive nothing new — this is the `None`
    // vs `None` / equal-value guard a historical replay depends on.
    const unchanged = fold(
      withGrid,
      updated(4, "2026-07-16T00:00:04Z", 42, { callsign: "W1AW", grid: "FN31" }),
    );
    expect(unchanged.roster[0].corrections).toHaveLength(1);
    const cleared = fold(
      unchanged,
      updated(5, "2026-07-16T00:00:05Z", 42, { callsign: "W1AW" }),
    );
    expect(cleared.roster[0].grid).toBeNull();
    expect(cleared.roster[0].corrections[1]).toEqual({
      field: "grid",
      from: "FN31",
      to: null,
      at: "2026-07-16T00:00:05Z",
    });
  });

  it("an update with all fields unchanged derives no correction but still bumps version", () => {
    const s = fold(base(), updated(3, "2026-07-16T00:00:03Z", 42, { callsign: "W1AW" }));
    expect(s.roster[0].corrections).toEqual([]);
    expect(s.roster[0].version).toBe(2);
  });

  it("checkin.updated on an absent id is a no-op that still advances the cursor", () => {
    const s = fold(base(), updated(3, "2026-07-16T00:00:03Z", 999, { callsign: "N0BODY" }));
    expect(s.roster).toHaveLength(1);
    expect(s.roster[0].callsign).toBe("W1AW");
    expect(s.roster[0].version).toBe(1);
    expect(s.lastSeq).toBe(3);
  });

  it("checkin.removed drops the row while the cursor advances; absent id is inert", () => {
    const withTwo = fold(base(), checkin(3, "2026-07-16T00:00:03Z", 43, "N1CCK"));
    const s = fold(withTwo, removed(4, "2026-07-16T00:00:04Z", 42));
    expect(s.roster).toHaveLength(1);
    expect(s.roster[0].checkInId).toBe(uuid(43));
    const s2 = fold(s, removed(5, "2026-07-16T00:00:05Z", 999));
    expect(s2.roster).toHaveLength(1);
    expect(s2.lastSeq).toBe(5);
  });

  it("a non-advancing seq is inert for the new kinds too", () => {
    const s = base();
    expect(fold(s, updated(2, "x", 42, { callsign: "W9XX" }))).toEqual(s);
    expect(fold(s, removed(1, "x", 42))).toEqual(s);
  });

  it("checkin.added defaults precedence to routine and traffic to null", () => {
    const s = base();
    expect(s.roster[0].precedence).toBe("routine");
    expect(s.roster[0].traffic).toBeNull();
  });

  it("checkin.updated replaces precedence/traffic and derives corrections after staying", () => {
    const s = fold(
      base(),
      updated(3, "2026-07-16T00:00:03Z", 42, {
        callsign: "W1AW",
        precedence: "emergency",
        traffic: 3,
      }),
    );
    expect(s.roster[0].precedence).toBe("emergency");
    expect(s.roster[0].traffic).toBe(3);
    // The two new corrections are appended AFTER the staying check, in the SAME
    // order as the Rust fold (precedence then traffic).
    expect(s.roster[0].corrections).toEqual([
      { field: "precedence", from: "routine", to: "emergency", at: "2026-07-16T00:00:03Z" },
      { field: "traffic", from: null, to: "3", at: "2026-07-16T00:00:03Z" },
    ]);
  });

  it("checkin.updated with a HISTORICAL payload missing precedence keeps the prior value, not undefined", () => {
    // The account-less `checkin.updated` payload is NOT redacted to
    // `{ checkInId, callsign }`: the public arm projects `staying` and
    // `precedence` always, and `traffic`/`publicNote` when present. Only a
    // HISTORICAL payload is missing them.
    //
    // The SHAPE this test folds is still real and still has to fold safely — it
    // is an older event in the append-only log, which carries no precedence key
    // because the field did not exist when it was written. The TS type promises
    // `precedence` is always present; that promise holds on the live wire, owner
    // and public alike, and not on history. So: no corrupted `undefined`
    // precedence, and no bogus derived correction on replay.
    const s = fold(base(), {
      seq: 3,
      kind: "checkin.updated",
      at: "2026-07-16T00:00:03Z",
      payload: { checkInId: uuid(42), callsign: "W1AW" },
    } as WireEvent);
    expect(s.roster[0].precedence).toBe("routine");
    expect(s.roster[0].traffic).toBeNull();
    expect(s.roster[0].corrections).toEqual([]);
  });

  it("a PUBLIC checkin.updated delta carrying staying/precedence/traffic/publicNote folds to THOSE values, not the absent-key defaults", () => {
    // The frontend half, and the assertion that would have
    // caught the cheapest wrong implementation.
    //
    // The public wire once sent NEITHER key, and the defaults at the top
    // of this arm (`staying ?? "in-and-out"`, `precedence ?? "routine"`) supplied
    // a value the component could not tell from a real one. Passing `showStaying`
    // and `showPrecedence` on the public page without widening the SERVER
    // therefore renders "In & out / Routine" against EVERY station, including
    // one holding emergency traffic — it renders, it passes axe, and nothing
    // reds. This test reds for exactly that mutation: it seeds NON-DEFAULT
    // values and asserts the fold carries them, so a reverted server projection
    // cannot pass it.
    const s = fold(base(), {
      seq: 3,
      kind: "checkin.updated",
      at: "2026-07-16T00:00:03Z",
      payload: {
        checkInId: uuid(42),
        callsign: "W1AW",
        staying: "staying-for-comments",
        precedence: "emergency",
        traffic: 3,
        publicNote: "relaying for the county EOC",
      },
    } as WireEvent);
    expect(s.roster[0].staying).toBe("staying-for-comments");
    expect(s.roster[0].precedence).toBe("emergency");
    expect(s.roster[0].traffic).toBe(3);
    expect(s.roster[0].publicNote).toBe("relaying for the county EOC");
    // The public note derives NO correction — it inherits the staff note's rule
    // exactly (running commentary, not a corrected mis-entry).
    expect(
      s.roster[0].corrections.some((c) => (c.field as string) === "publicNote"),
    ).toBe(false);
  });

  it("checkin.updated with no publicNote key folds it to null and leaves the staff note alone", () => {
    // A HISTORICAL payload written before the public note existed carries no
    // public-note key at all. It decodes to `null` — not `undefined`, and not
    // by blanking anything already stored under `notes`.
    let s = fold(base(), {
      seq: 3,
      kind: "checkin.updated",
      at: "2026-07-16T00:00:03Z",
      payload: {
        checkInId: uuid(42),
        callsign: "W1AW",
        precedence: "routine",
        notes: "the staff note, written months ago",
      },
    } as WireEvent);
    expect(s.roster[0].notes).toBe("the staff note, written months ago");
    expect(s.roster[0].publicNote).toBeNull();
    s = fold(s, {
      seq: 4,
      kind: "checkin.updated",
      at: "2026-07-16T00:00:04Z",
      payload: { checkInId: uuid(42), callsign: "W1AW", precedence: "routine" },
    } as WireEvent);
    expect(s.roster[0].publicNote).toBeNull();
  });

  it("roster.reordered reorders the roster to the given order and advances the cursor", () => {
    let s = base(); // 42 present
    s = fold(s, checkin(3, "2026-07-16T00:00:03Z", 43, "N1CCK"));
    s = fold(s, checkin(4, "2026-07-16T00:00:04Z", 44, "K1XYZ"));
    s = fold(s, reordered(5, "2026-07-16T00:00:05Z", [44, 42, 43]));
    expect(s.roster.map((e) => e.checkInId)).toEqual([uuid(44), uuid(42), uuid(43)]);
    expect(s.lastSeq).toBe(5);
    // A reorder bumps no version and derives no correction.
    expect(s.roster.every((e) => e.version === 1)).toBe(true);
    expect(s.roster.every((e) => e.corrections.length === 0)).toBe(true);
  });

  it("roster.reordered keeps unlisted entries in order and ignores unknown ids", () => {
    let s = base();
    s = fold(s, checkin(3, "2026-07-16T00:00:03Z", 43, "N1CCK"));
    s = fold(s, checkin(4, "2026-07-16T00:00:04Z", 44, "K1XYZ"));
    // Only 43 named; 42 and 44 appended after in current order; 999 ignored.
    s = fold(s, reordered(5, "2026-07-16T00:00:05Z", [999, 43]));
    expect(s.roster.map((e) => e.checkInId)).toEqual([uuid(43), uuid(42), uuid(44)]);
  });

  it("a non-advancing roster.reordered is inert", () => {
    let s = base();
    s = fold(s, checkin(3, "2026-07-16T00:00:03Z", 43, "N1CCK"));
    expect(fold(s, reordered(3, "x", [43, 42]))).toEqual(s);
  });
});

describe("sessionReducer.fold — worked-station cursor + notes", () => {
  const base = () =>
    replay(
      [
        started(1, "2026-07-16T00:00:01Z", 14_250_000),
        checkin(2, "2026-07-16T00:00:02Z", 42, "W1AW"),
        checkin(3, "2026-07-16T00:00:03Z", 43, "N1CCK"),
      ],
      0,
    );

  it("checkin.added defaults worked to false and notes to null", () => {
    const s = base();
    expect(s.roster[0].worked).toBe(false);
    expect(s.roster[0].notes).toBeNull();
    expect(s.workingCheckInId).toBeNull();
    expect(s.netNote).toBeNull();
  });

  it("station.worked-set moves the cursor to an on-roster entry", () => {
    const s = fold(base(), workedSet(4, "2026-07-16T00:00:04Z", 42));
    expect(s.workingCheckInId).toBe(uuid(42));
    expect(s.roster.every((e) => !e.worked)).toBe(true);
    expect(s.lastSeq).toBe(4);
  });

  it("moving the cursor marks the prior working entry worked", () => {
    let s = fold(base(), workedSet(4, "d", 42));
    s = fold(s, workedSet(5, "e", 43));
    expect(s.workingCheckInId).toBe(uuid(43));
    expect(s.roster.find((e) => e.checkInId === uuid(42))!.worked).toBe(true);
    expect(s.roster.find((e) => e.checkInId === uuid(43))!.worked).toBe(false);
  });

  it("station.worked-set null clears the cursor and marks the prior worked", () => {
    let s = fold(base(), workedSet(4, "d", 42));
    s = fold(s, workedSet(5, "e", null));
    expect(s.workingCheckInId).toBeNull();
    expect(s.roster.find((e) => e.checkInId === uuid(42))!.worked).toBe(true);
  });

  it("an off-roster station.worked-set target is a no-op for the cursor", () => {
    let s = fold(base(), workedSet(4, "d", 42));
    s = fold(s, workedSet(5, "e", 999));
    expect(s.workingCheckInId).toBe(uuid(42));
    expect(s.roster.find((e) => e.checkInId === uuid(42))!.worked).toBe(false);
    expect(s.lastSeq).toBe(5);
  });

  it("the station.worked-set case leaves a sunk entry where it was", () => {
    // The companion case. The pinned test below works B then A on a TWO-station
    // roster, where "sink the entry the cursor left" is a no-op because that
    // entry is already last — measured: mutating the case to sink left it green.
    // Three stations worked front-to-back is the smallest discriminating fixture.
    const three = replay(
      [
        started(1, "2026-07-16T00:00:01Z", 14_250_000),
        checkin(2, "2026-07-16T00:00:02Z", 42, "W1AW"),
        checkin(3, "2026-07-16T00:00:03Z", 43, "N1CCK"),
        checkin(4, "2026-07-16T00:00:04Z", 44, "K1XYZ"),
      ],
      0,
    );
    let s = fold(three, workedSet(5, "e", 42));
    s = fold(s, workedSet(6, "f", 43));
    s = fold(s, workedSet(7, "g", 44));
    expect(s.roster.map((e) => e.checkInId)).toEqual([uuid(42), uuid(43), uuid(44)]);
    expect(s.roster.map((e) => e.worked)).toEqual([true, true, false]);
  });

  it("station.worked-set never reorders, bumps version, or derives corrections", () => {
    let s = fold(base(), workedSet(4, "d", 43));
    s = fold(s, workedSet(5, "e", 42));
    expect(s.roster.map((e) => e.checkInId)).toEqual([uuid(42), uuid(43)]);
    expect(s.roster.every((e) => e.version === 1)).toBe(true);
    expect(s.roster.every((e) => e.corrections.length === 0)).toBe(true);
  });

  it("roster.order-mode-set folds the mode REPLACE / last-write-wins", () => {
    // The ordering mode is durable session state folded from
    // its own kind, mirroring session.note-set -> netNote.
    expect(DEFAULT_SESSION_STATE.rosterOrderMode).toBe("manual");
    let s = fold(base(), orderModeSet(4, "d", "worked-sink"));
    expect(s.rosterOrderMode).toBe("worked-sink");
    s = fold(s, orderModeSet(5, "e", "manual"));
    expect(s.rosterOrderMode).toBe("manual");
  });

  it("an unknown or absent roster order mode folds to the default", () => {
    // The reducer is total. A token from a newer server, or a payload with
    // no `mode` key at all, folds to the legacy mode rather than `undefined`.
    const unknown = fold(base(), orderModeSet(4, "d", "shuffle"));
    expect(unknown.rosterOrderMode).toBe("manual");
    expect(unknown.lastSeq).toBe(4);
    const absent = fold(base(), orderModeSet(4, "d", null));
    expect(absent.rosterOrderMode).toBe("manual");
    expect(absent.lastSeq).toBe(4);
  });

  it("a public viewer's worked-sink mode SURVIVES the public roster.order-mode-set delta", () => {
    // The public snapshot now carries `rosterOrderMode`, and the public
    // page's your-turn selector reads it. That is defeated by the very next
    // frame unless the public DELTA carries the token too: the reducer above
    // folds an absent key to `manual`, so a `{}` public payload would silently
    // overwrite the correct snapshot value while the server kept sinking.
    //
    // The payload below is the PUBLIC projection the backend emits, pinned on
    // the Rust side by `ws::protocol::tests::
    // public_roster_order_mode_set_carries_the_mode_and_nothing_else` — the two
    // ends are asserted independently, and this fixture mirrors that test, not
    // the reducer's own wishes. (At `4ae0239` the projection was `{}`, and this
    // test went red against it.)
    const publicViewer = { ...base(), rosterOrderMode: "worked-sink" as const };
    const publicPayload = { mode: "worked-sink" };
    const after = fold(publicViewer, {
      seq: 4,
      kind: "roster.order-mode-set",
      at: "d",
      payload: publicPayload,
    } as unknown as WireEvent);
    expect(after.rosterOrderMode).toBe("worked-sink");
    expect(after.lastSeq).toBe(4);
  });

  it("the roster order mode never reorders the roster in the reducer", () => {
    // The mirror: the field is INERT in the fold — no arm reads it and no
    // arm sorts, so the order only ever moves on a recorded permutation.
    let s = fold(base(), orderModeSet(4, "d", "worked-sink"));
    s = fold(s, workedSet(5, "e", 42));
    s = fold(s, workedSet(6, "f", 43));
    expect(s.roster.map((e) => e.checkInId)).toEqual([uuid(42), uuid(43)]);
    expect(s.roster.find((e) => e.checkInId === uuid(42))!.worked).toBe(true);
  });

  it("removing the working entry clears the cursor", () => {
    let s = fold(base(), workedSet(4, "d", 42));
    s = fold(s, removed(5, "e", 42));
    expect(s.workingCheckInId).toBeNull();
  });

  it("session.closed clears a lingering working cursor", () => {
    // Review finding: a closed session accepts no further station.worked-set
    // events, so a station left "currently working" at close time would
    // otherwise linger as the cursor forever — mirrors the Rust fold's
    // session_closed_clears_a_lingering_working_cursor test byte-for-byte.
    let s = fold(base(), workedSet(4, "d", 42));
    s = fold(s, closed(5, "e"));
    expect(s.workingCheckInId).toBeNull();
    expect(s.lifecycle).toBe("closed");
  });

  it("a non-advancing station.worked-set is inert", () => {
    const s = fold(base(), workedSet(4, "d", 42));
    expect(fold(s, workedSet(4, "z", null))).toEqual(s);
  });

  it("session.note-set sets and clears the net note", () => {
    let s = fold(base(), noteSet(4, "d", "Weekly traffic net"));
    expect(s.netNote).toBe("Weekly traffic net");
    s = fold(s, noteSet(5, "e", "Net closing"));
    expect(s.netNote).toBe("Net closing");
    s = fold(s, noteSet(6, "f", null));
    expect(s.netNote).toBeNull();
  });

  it("checkin.updated replaces notes and derives NO notes correction", () => {
    const s = fold(
      base(),
      updated(4, "2026-07-16T00:00:04Z", 42, {
        callsign: "W1AW",
        notes: "handling traffic",
      } as never),
    );
    expect(s.roster[0].notes).toBe("handling traffic");
    expect(s.roster[0].corrections.some((c) => (c.field as string) === "notes")).toBe(false);
    expect(s.roster[0].version).toBe(2);
  });

  it("rounds are repeatable without losing prior notes (multi-pass)", () => {
    let s = base();
    s = fold(s, workedSet(4, "d", 42));
    // A single edit setting the note (the modal sends the full field set).
    s = fold(s, updated(5, "e", 42, { callsign: "W1AW", notes: "passing NTS" } as never));
    s = fold(s, workedSet(6, "f", 43));
    // Round 2: revisit 42.
    s = fold(s, workedSet(7, "g", 42));
    expect(s.workingCheckInId).toBe(uuid(42));
    // The round-1 note survives untouched by later cursor moves.
    expect(s.roster.find((e) => e.checkInId === uuid(42))!.notes).toBe("passing NTS");
    // 43 was left behind, so it is worked.
    expect(s.roster.find((e) => e.checkInId === uuid(43))!.worked).toBe(true);
  });
});

describe("control-status axis", () => {
  const stalled = (seq: number, at: string): WireEvent => ({
    seq,
    kind: "ncs.stalled",
    at,
    payload: {},
  });
  const resumed = (seq: number, at: string): WireEvent => ({
    seq,
    kind: "ncs.resumed",
    at,
    payload: {},
  });
  const handedOff = (seq: number, at: string, newNcs: number | null): WireEvent => ({
    seq,
    kind: "control.handed-off",
    actorId: uuid(300),
    at,
    payload: newNcs === null ? {} : { newNcsAccountId: uuid(newNcs) },
  });

  it("defaults to active with no active NCS before any event", () => {
    expect(DEFAULT_SESSION_STATE.controlState).toBe("active");
    expect(DEFAULT_SESSION_STATE.activeNcsAccountId).toBeNull();
    expect(DEFAULT_SESSION_STATE.stalledAt).toBeNull();
  });

  it("session.started seeds the starter as the active NCS", () => {
    const s = replay([started(1, "2026-07-17T00:00:00Z", 14_250_000)], 0);
    expect(s.activeNcsAccountId).toBe(uuid(100));
    expect(s.controlState).toBe("active");
    // Lifecycle axis unaffected by the control fields.
    expect(s.lifecycle).toBe("live");
  });

  it("ncs.stalled moves control to stalled and stamps the instant, lifecycle untouched", () => {
    const s = replay(
      [started(1, "2026-07-17T00:00:00Z", 14_250_000), stalled(2, "2026-07-17T00:01:30Z")],
      0,
    );
    expect(s.controlState).toBe("stalled");
    expect(s.stalledAt).toBe("2026-07-17T00:01:30Z");
    expect(s.lifecycle).toBe("live");
    expect(s.activeNcsAccountId).toBe(uuid(100));
  });

  it("ncs.resumed returns to active under the same NCS and clears the instant", () => {
    const s = replay(
      [
        started(1, "2026-07-17T00:00:00Z", 14_250_000),
        stalled(2, "2026-07-17T00:01:30Z"),
        resumed(3, "2026-07-17T00:02:00Z"),
      ],
      0,
    );
    expect(s.controlState).toBe("active");
    expect(s.stalledAt).toBeNull();
    expect(s.activeNcsAccountId).toBe(uuid(100));
  });

  it("control.handed-off moves the active NCS and returns to active", () => {
    const s = replay(
      [
        started(1, "2026-07-17T00:00:00Z", 14_250_000),
        stalled(2, "2026-07-17T00:01:30Z"),
        handedOff(3, "2026-07-17T00:02:00Z", 555),
      ],
      0,
    );
    expect(s.activeNcsAccountId).toBe(uuid(555));
    expect(s.controlState).toBe("active");
    expect(s.stalledAt).toBeNull();
  });

  it("a redacted public control.handed-off (no newNcsAccountId) folds active with null NCS", () => {
    const s = replay(
      [
        started(1, "2026-07-17T00:00:00Z", 14_250_000),
        stalled(2, "2026-07-17T00:01:30Z"),
        handedOff(3, "2026-07-17T00:02:00Z", null),
      ],
      0,
    );
    expect(s.controlState).toBe("active");
    expect(s.activeNcsAccountId).toBeNull();
  });

  it("closing a stalled session resets the control axis to inert defaults", () => {
    const s = replay(
      [
        started(1, "2026-07-17T00:00:00Z", 14_250_000),
        stalled(2, "2026-07-17T00:01:30Z"),
        closed(3, "2026-07-17T00:15:00Z"),
      ],
      0,
    );
    expect(s.lifecycle).toBe("closed");
    expect(s.controlState).toBe("active");
    expect(s.stalledAt).toBeNull();
  });

  it("a non-advancing seq is inert for the control kinds", () => {
    const base = replay(
      [started(1, "2026-07-17T00:00:00Z", 14_250_000), stalled(2, "2026-07-17T00:01:30Z")],
      0,
    );
    // seq <= lastSeq: absorbed as a no-op (same reference).
    expect(fold(base, resumed(2, "2026-07-17T09:99:99Z"))).toBe(base);
  });
});

describe("`via` — which way in a station came in on", () => {
  const echolink = {
    id: "00000000-0000-0000-0000-0000000016e1",
    position: 1,
    kind: "echolink",
    plannedFrequencyHz: null,
    band: null,
    mode: null,
    repeaterOffsetHz: null,
    toneMode: null,
    toneValue: null,
    node: "12345",
    reflector: null,
    network: null,
    talkgroup: null,
    label: null,
    detail: null,
  };
  // The reducer resolves a `via` LABEL against `state.connections`, which the
  // snapshot seeds — so these tests seed it the same way the socket does.
  const seeded = {
    ...DEFAULT_SESSION_STATE,
    connections: [hfWay(), echolink],
  };

  function addedWithVia(seq: number, at: string, id: number, via: ViaWire): WireEvent {
    return {
      seq,
      kind: "checkin.added",
      actorId: uuid(200),
      at,
      payload: { checkInId: uuid(id), callsign: "W1AW", via },
    };
  }

  it("folds a via captured at add onto the roster row", () => {
    const state = fold(
      seeded,
      addedWithVia(1, "2026-08-30T00:00:00Z", 42, {
        kind: "connection",
        connectionId: echolink.id,
      }),
    );
    expect(state.roster[0].via).toEqual({
      kind: "connection",
      connectionId: echolink.id,
    });
  });

  it("folds an add with no via key to null — nobody recorded it", () => {
    const state = fold(seeded, checkin(1, "2026-08-30T00:00:00Z", 42, "W1AW"));
    expect(state.roster[0].via).toBeNull();
  });

  it("derives a via correction whose from/to are LABELS, never ids", () => {
    let state = fold(
      seeded,
      addedWithVia(1, "2026-08-30T00:00:00Z", 42, {
        kind: "connection",
        connectionId: hfWay().id,
      }),
    );
    state = fold(
      state,
      updated(2, "2026-08-30T00:01:00Z", 42, {
        callsign: "W1AW",
        via: { kind: "connection", connectionId: echolink.id },
      }),
    );
    const correction = state.roster[0].corrections.find((c) => c.field === "via");
    expect(correction).toBeDefined();
    expect(correction?.from).toBe("HF — 14.230 MHz");
    expect(correction?.to).toBe("EchoLink — 12345");
    expect(correction?.from).not.toContain(hfWay().id);
    expect(correction?.to).not.toContain(echolink.id);
  });

  it("carries the STRUCTURED via on the correction so a surface can re-resolve it", () => {
    // The fold-time labels are a snapshot; the structured values are the
    // authority. Without them the browser freezes a label the server resolves at
    // projection time, and after a mid-session QSY the live socket and a reload
    // show different text for the SAME past event.
    let state = fold(
      seeded,
      addedWithVia(1, "2026-08-30T00:00:00Z", 42, {
        kind: "connection",
        connectionId: hfWay().id,
      }),
    );
    state = fold(
      state,
      updated(2, "2026-08-30T00:01:00Z", 42, {
        callsign: "W1AW",
        via: { kind: "connection", connectionId: echolink.id },
      }),
    );
    const correction = state.roster[0].corrections.find((c) => c.field === "via");
    expect(correction?.fromVia).toEqual({
      kind: "connection",
      connectionId: hfWay().id,
    });
    expect(correction?.toVia).toEqual({
      kind: "connection",
      connectionId: echolink.id,
    });
  });

  it("re-labels a via correction after a mid-session QSY rather than freezing it", () => {
    let state = fold(
      seeded,
      addedWithVia(1, "2026-08-30T00:00:00Z", 42, {
        kind: "connection",
        connectionId: echolink.id,
      }),
    );
    state = fold(
      state,
      updated(2, "2026-08-30T00:01:00Z", 42, {
        callsign: "W1AW",
        via: { kind: "connection", connectionId: hfWay().id },
      }),
    );
    // The net moves. `buildRoster` is where a row is rendered, and it resolves
    // the correction's two sides against the connection set as it stands NOW.
    const moved = [hfWay(14_250_000), echolink];
    const rows = buildRoster(
      state.roster,
      new Map(),
      new Map(),
      null,
      Date.now(),
      null,
      moved,
    );
    const correction = rows[0].corrections.find((c) => c.field === "via");
    expect(correction?.to).toBe("HF — 14.250 MHz");
    expect(correction?.from).toBe("EchoLink — 12345");
  });

  it("round-trips a free-text via through from/to byte for byte", () => {
    let state = fold(
      seeded,
      addedWithVia(1, "2026-08-30T00:00:00Z", 42, {
        kind: "unlisted",
        text: "Bill's phone patch — 2m simplex",
      }),
    );
    state = fold(
      state,
      updated(2, "2026-08-30T00:01:00Z", 42, { callsign: "W1AW" }),
    );
    const correction = state.roster[0].corrections.find((c) => c.field === "via");
    expect(correction?.from).toBe("Bill's phone patch — 2m simplex");
    expect(correction?.to).toBeNull();
    expect(state.roster[0].via).toBeNull();
  });

  it("annotates via AFTER the eight fields that shipped before it, mirroring the Rust fold", () => {
    let state = fold(
      seeded,
      addedWithVia(1, "2026-08-30T00:00:00Z", 42, {
        kind: "connection",
        connectionId: hfWay().id,
      }),
    );
    state = fold(
      state,
      updated(2, "2026-08-30T00:01:00Z", 42, {
        callsign: "W1ABC",
        via: { kind: "connection", connectionId: echolink.id },
      }),
    );
    expect(state.roster[0].corrections.map((c) => c.field)).toEqual([
      "callsign",
      "via",
    ]);
  });

  it("derives no via correction when the edit leaves it alone", () => {
    let state = fold(
      seeded,
      addedWithVia(1, "2026-08-30T00:00:00Z", 42, {
        kind: "unlisted",
        text: "phone patch",
      }),
    );
    state = fold(
      state,
      updated(2, "2026-08-30T00:01:00Z", 42, {
        callsign: "W1AW",
        via: { kind: "unlisted", text: "phone patch" },
      }),
    );
    expect(state.roster[0].corrections.some((c) => c.field === "via")).toBe(false);
  });
});

describe("`relayedBy` — WHO passed a station's traffic", () => {
  function addedRelayed(seq: number, id: number, relayedBy: string): WireEvent {
    return {
      seq,
      kind: "checkin.added",
      actorId: null,
      at: "2026-09-02T00:00:00Z",
      payload: { checkInId: uuid(id), callsign: "W1AW", relayedBy },
    } as unknown as WireEvent;
  }

  it("folds a relaying station captured at add onto the roster row", () => {
    const state = fold(DEFAULT_SESSION_STATE, addedRelayed(1, 1, "W3REL"));
    expect(state.roster[0].relayedBy).toBe("W3REL");
  });

  it("folds an add with no relayedBy key to null — not relayed", () => {
    const state = fold(DEFAULT_SESSION_STATE, checkin(1, "2026-09-02T00:00:00Z", 1, "W1AW"));
    expect(state.roster[0].relayedBy).toBeNull();
  });

  it("keeps the way in and the relaying station independent in both directions", () => {
    // The failure ruling #6 exists to prevent: the two facts sharing one field
    // and becoming ambiguous the first time they differ.
    const state = fold(DEFAULT_SESSION_STATE, {
      seq: 1,
      kind: "checkin.added",
      actorId: null,
      at: "2026-09-02T00:00:00Z",
      payload: {
        checkInId: uuid(1),
        callsign: "W1AW",
        via: { kind: "unlisted", text: "Bill's phone patch" },
        relayedBy: "W3REL",
      },
    } as unknown as WireEvent);
    expect(state.roster[0].via).toEqual({ kind: "unlisted", text: "Bill's phone patch" });
    expect(state.roster[0].relayedBy).toBe("W3REL");
  });

  it("derives a relayedBy correction when an edit changes it, after the via one", () => {
    // The derived ORDER mirrors the Rust fold byte-for-byte: `relayedBy`
    // annotates LAST, after `via`, which annotates after the eight before it.
    const state = [
      addedRelayed(1, 1, "W1ABC"),
      {
        seq: 2,
        kind: "checkin.updated",
        actorId: null,
        at: "2026-09-02T00:01:00Z",
        payload: {
          checkInId: uuid(1),
          callsign: "W1AW",
          via: { kind: "unlisted", text: "a phone patch" },
          relayedBy: "W1ABD",
        },
      } as unknown as WireEvent,
    ].reduce(fold, DEFAULT_SESSION_STATE);
    const fields = state.roster[0].corrections.map((c) => c.field);
    expect(fields).toEqual(["via", "relayedBy"]);
    const correction = state.roster[0].corrections.find((c) => c.field === "relayedBy");
    expect(correction?.from).toBe("W1ABC");
    expect(correction?.to).toBe("W1ABD");
    expect(state.roster[0].relayedBy).toBe("W1ABD");
  });

  it("treats a cleared relayedBy as a correction with no `to` side", () => {
    const state = [
      addedRelayed(1, 1, "W3REL"),
      {
        seq: 2,
        kind: "checkin.updated",
        actorId: null,
        at: "2026-09-02T00:01:00Z",
        payload: { checkInId: uuid(1), callsign: "W1AW" },
      } as unknown as WireEvent,
    ].reduce(fold, DEFAULT_SESSION_STATE);
    const correction = state.roster[0].corrections.find((c) => c.field === "relayedBy");
    expect(correction?.from).toBe("W3REL");
    expect(correction?.to).toBeNull();
    expect(state.roster[0].relayedBy).toBeNull();
  });
});
