// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
import { describe, expect, it } from "vitest";

import {
  buildRoster,
  countDistinctStates,
  createSessionStore,
  displayedConnection,
  selectDuplicateCheckIn,
  selectRoster,
  selectYourTurnCheckInId,
} from "./sessionStore";
import { UNRESOLVABLE_VIA_LABEL } from "../nets/connectionPresentation";
import type { DisplayRosterEntry } from "./sessionStore";
import type { RosterEntry } from "./sessionReducer";
import type { CheckinAddedEvent, SessionSummaryBody } from "./sessionWire";

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
 * Store-mechanism tests. These assert STATE and the optimistic
 * pending→reconcile machinery, never rendered text. The store is a vanilla
 * Zustand store built from a factory so each test gets an isolated instance.
 */

const uuid = (n: number): string =>
  `00000000-0000-0000-0000-${n.toString(16).padStart(12, "0")}`;

function summary(overrides: Partial<SessionSummaryBody> = {}): SessionSummaryBody {
  return {
    id: uuid(1),
    definitionId: uuid(7),
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
    ...overrides,
  };
}

function checkinEvent(
  seq: number,
  checkInId: number,
  callsign: string,
  clientEventId?: number,
): CheckinAddedEvent {
  const payload: CheckinAddedEvent["payload"] = {
    checkInId: uuid(checkInId),
    callsign,
  };
  return {
    seq,
    kind: "checkin.added",
    actorId: uuid(200),
    at: "2026-07-16T00:00:05Z",
    payload:
      clientEventId === undefined
        ? payload
        : { ...payload, clientEventId: uuid(clientEventId) },
  };
}

describe("sessionStore", () => {
  it("starts catching-up with an empty pending map and default session", () => {
    const store = createSessionStore();
    const s = store.getState();
    expect(s.connection).toBe("catching-up");
    expect(s.pending.size).toBe(0);
    expect(s.session.lastSeq).toBe(0);
    expect(s.session.lifecycle).toBe("scheduled");
  });

  it("seeds session and frozen metadata from a snapshot summary", () => {
    const store = createSessionStore();
    store.getState().seedFromSnapshot(
      summary({
        latestSeq: 5,
        roster: [
          { checkInId: uuid(42), callsign: "W1AW", addedAt: "t", addedBy: uuid(200), source: "staff", staying: "in-and-out", precedence: "routine", worked: false, version: 1, corrections: [] },
        ],
      }),
    );
    const s = store.getState();
    expect(s.session.lifecycle).toBe("live");
    expect(s.session.connections[0]?.plannedFrequencyHz).toBe(14_250_000);
    expect(s.session.lastSeq).toBe(5);
    expect(s.session.roster).toHaveLength(1);
    expect(s.meta?.definition.title).toBe("Sunday Traffic Net");
    expect(s.meta?.id).toBe(uuid(1));
  });

  it("surfaces the viewer's own role from the staff snapshot onto meta", () => {
    const store = createSessionStore();
    store.getState().seedFromSnapshot(summary({ viewerRole: "relay" }));
    expect(store.getState().meta?.viewerRole).toBe("relay");
  });

  it("defaults meta.viewerRole to null when the snapshot omits it (public view)", () => {
    const store = createSessionStore();
    // A redacted public snapshot carries no viewerRole; the store must not leave
    // meta without the field.
    store.getState().seedFromSnapshot(summary());
    expect(store.getState().meta?.viewerRole).toBeNull();
  });

  it("folds a live delta onto the seeded state and advances the cursor", () => {
    const store = createSessionStore();
    store.getState().seedFromSnapshot(summary({ latestSeq: 1 }));
    store.getState().applyEvent(checkinEvent(2, 42, "W1AW"));
    const s = store.getState();
    expect(s.session.roster).toHaveLength(1);
    expect(s.session.lastSeq).toBe(2);
  });

  it("drops a delta whose seq does not advance the cursor", () => {
    const store = createSessionStore();
    store.getState().seedFromSnapshot(summary({ latestSeq: 5 }));
    store.getState().applyEvent(checkinEvent(3, 42, "W1AW"));
    expect(store.getState().session.roster).toHaveLength(0);
    expect(store.getState().session.lastSeq).toBe(5);
  });

  it("folds a catch-up batch onto the current state", () => {
    const store = createSessionStore();
    store.getState().seedFromSnapshot(summary({ latestSeq: 1 }));
    store.getState().applyCatchUp([
      checkinEvent(2, 42, "W1AW"),
      checkinEvent(3, 43, "N1CCK"),
    ]);
    const s = store.getState();
    expect(s.session.roster.map((r) => r.callsign)).toEqual(["W1AW", "N1CCK"]);
    expect(s.session.lastSeq).toBe(3);
  });

  it("renders a pending optimistic entry until its authoritative event echoes", () => {
    const store = createSessionStore();
    store.getState().seedFromSnapshot(summary({ latestSeq: 1 }));
    store.getState().addPending(uuid(9), "K2ABC");

    const withPending = selectRoster(store.getState());
    expect(withPending).toHaveLength(1);
    expect(withPending[0]).toMatchObject({ callsign: "K2ABC", optimistic: true });
  });

  it("reconciles: the authoritative echo clears the pending entry with no duplicate", () => {
    const store = createSessionStore();
    store.getState().seedFromSnapshot(summary({ latestSeq: 1 }));
    store.getState().addPending(uuid(9), "K2ABC");

    // The server echoes the same clientEventId on the authoritative event.
    store.getState().applyEvent(checkinEvent(2, 42, "K2ABC", 9));

    const s = store.getState();
    expect(s.pending.size).toBe(0);
    const rows = selectRoster(s);
    expect(rows).toHaveLength(1);
    expect(rows[0]).toMatchObject({ callsign: "K2ABC", optimistic: false });
    expect(rows[0].key).toBe(uuid(42));
  });

  it("a viewer (no pending) renders the folded roster only — never a foreign pending", () => {
    const store = createSessionStore();
    store.getState().seedFromSnapshot(summary({ latestSeq: 1 }));
    // A check-in that carries SOME OTHER client's clientEventId: a viewer has
    // no pending map, so it only ever sees the authoritative row.
    store.getState().applyEvent(checkinEvent(2, 42, "W1AW", 99));

    const rows = selectRoster(store.getState());
    expect(rows).toHaveLength(1);
    expect(rows.every((r) => !r.optimistic)).toBe(true);
  });

  it("keeps a pending entry whose clientEventId has not yet echoed", () => {
    const store = createSessionStore();
    store.getState().seedFromSnapshot(summary({ latestSeq: 1 }));
    store.getState().addPending(uuid(9), "K2ABC");
    // A DIFFERENT authoritative check-in (no matching clientEventId) folds in.
    store.getState().applyEvent(checkinEvent(2, 42, "W1AW", 88));

    const s = store.getState();
    expect(s.pending.size).toBe(1);
    const rows = selectRoster(s);
    expect(rows).toHaveLength(2);
    expect(rows.filter((r) => r.optimistic)).toHaveLength(1);
  });

  it("a fresh re-snapshot clears any un-echoed pending entry (no orphaned optimistic row)", () => {
    const store = createSessionStore();
    store.getState().seedFromSnapshot(summary({ latestSeq: 1 }));
    store.getState().addPending(uuid(9), "K2ABC");
    expect(store.getState().pending.size).toBe(1);

    // A hard reset (initial load re-run, or a post-1008 re-snapshot) is
    // authoritative — it is not an incremental fold the pending entry could
    // still reconcile against.
    store.getState().seedFromSnapshot(summary({ latestSeq: 5 }));

    const s = store.getState();
    expect(s.pending.size).toBe(0);
    expect(selectRoster(s).every((r) => !r.optimistic)).toBe(true);
  });

  it("removePending drops a pending entry, leaving the authoritative roster intact", () => {
    const store = createSessionStore();
    store.getState().seedFromSnapshot(
      summary({
        latestSeq: 1,
        roster: [
          { checkInId: uuid(42), callsign: "W1AW", addedAt: "t", addedBy: uuid(200), source: "staff", staying: "in-and-out", precedence: "routine", worked: false, version: 1, corrections: [] },
        ],
      }),
    );
    store.getState().addPending(uuid(9), "K2ABC");
    expect(store.getState().pending.size).toBe(1);

    store.getState().removePending(uuid(9));

    const s = store.getState();
    expect(s.pending.size).toBe(0);
    // The authoritative roster is untouched by the optimistic rollback.
    const rows = selectRoster(s);
    expect(rows).toHaveLength(1);
    expect(rows[0]).toMatchObject({ callsign: "W1AW", optimistic: false });
  });

  it("removePending returns the SAME map reference when the key is absent (no needless re-notify)", () => {
    const store = createSessionStore();
    store.getState().seedFromSnapshot(summary({ latestSeq: 1 }));
    store.getState().addPending(uuid(9), "K2ABC");

    const before = store.getState().pending;
    store.getState().removePending(uuid(77)); // a key that was never added
    const after = store.getState().pending;

    expect(after).toBe(before);
    expect(after.size).toBe(1);
  });

  it("removePending on an empty pending map is an inert same-ref no-op", () => {
    const store = createSessionStore();
    store.getState().seedFromSnapshot(summary({ latestSeq: 1 }));

    const before = store.getState().pending;
    store.getState().removePending(uuid(9));
    expect(store.getState().pending).toBe(before);
    expect(store.getState().pending.size).toBe(0);
  });

  it("still reconciles on echo after removePending is added (reconcile-on-echo regression)", () => {
    const store = createSessionStore();
    store.getState().seedFromSnapshot(summary({ latestSeq: 1 }));
    store.getState().addPending(uuid(9), "K2ABC");
    store.getState().applyEvent(checkinEvent(2, 42, "K2ABC", 9));

    expect(store.getState().pending.size).toBe(0);
    const rows = selectRoster(store.getState());
    expect(rows).toHaveLength(1);
    expect(rows[0]).toMatchObject({ optimistic: false });
  });

  it("remembers which checkInId an echoed clientEventId became", () => {
    // The quick-add's live duplicate warning needs to keep excluding the row
    // its own commit produced AFTER the echo replaces the pending entry with an
    // authoritative one — and the fold drops `clientEventId`, so only the
    // store's reconcile step ever sees both ids together.
    const store = createSessionStore();
    store.getState().seedFromSnapshot(summary({ latestSeq: 1 }));
    store.getState().addPending(uuid(9), "K2ABC");

    store.getState().applyEvent(checkinEvent(2, 42, "K2ABC", 9));

    expect(store.getState().echoed.get(uuid(9))).toBe(uuid(42));
    expect(store.getState().echoed.has(uuid(42))).toBe(false);
  });

  it("addPending carries signalReport and staying onto the optimistic display row", () => {
    const store = createSessionStore();
    store.getState().seedFromSnapshot(summary({ latestSeq: 1 }));
    store.getState().addPending(uuid(9), "K2ABC", "599", "staying-for-comments");

    const rows = selectRoster(store.getState());
    expect(rows).toHaveLength(1);
    expect(rows[0]).toMatchObject({
      callsign: "K2ABC",
      optimistic: true,
      signalReport: "599",
      staying: "staying-for-comments",
    });
  });

  it("addPending defaults staying to in-and-out and signalReport to null when omitted", () => {
    const store = createSessionStore();
    store.getState().seedFromSnapshot(summary({ latestSeq: 1 }));
    store.getState().addPending(uuid(9), "K2ABC");

    const rows = selectRoster(store.getState());
    expect(rows[0].staying).toBe("in-and-out");
    expect(rows[0].signalReport).toBeNull();
  });

  it("buildRoster surfaces addedBy on an authoritative row and null on a pending one (post-net summary's NCS-callsign derivation)", () => {
    const store = createSessionStore();
    store.getState().seedFromSnapshot(
      summary({
        latestSeq: 1,
        roster: [
          {
            checkInId: uuid(42),
            callsign: "W1AW",
            addedAt: "t",
            addedBy: uuid(200),
            source: "staff",
            staying: "in-and-out",
            precedence: "routine",
            worked: false,
            version: 1,
            corrections: [],
          },
        ],
      }),
    );
    store.getState().addPending(uuid(9), "K2ABC");
    const rows = selectRoster(store.getState());
    expect(rows.find((r) => r.callsign === "W1AW")).toMatchObject({ addedBy: uuid(200) });
    expect(rows.find((r) => r.callsign === "K2ABC")).toMatchObject({ addedBy: null });
  });

  it("buildRoster surfaces addedAt on an authoritative row (Heard column)", () => {
    const store = createSessionStore();
    store.getState().seedFromSnapshot(
      summary({
        latestSeq: 1,
        roster: [
          {
            checkInId: uuid(42),
            callsign: "W1AW",
            addedAt: "2026-07-16T00:00:00Z",
            addedBy: uuid(200),
            source: "staff",
            staying: "in-and-out",
            precedence: "routine",
            worked: false,
            version: 1,
            corrections: [],
          },
        ],
      }),
    );
    const rows = selectRoster(store.getState());
    expect(rows[0]).toMatchObject({ addedAt: "2026-07-16T00:00:00Z" });
  });

  it("addPending stamps the optimistic row's addedAt at the given instant (Heard column)", () => {
    const store = createSessionStore();
    store.getState().seedFromSnapshot(summary({ latestSeq: 1 }));
    store.getState().addPending(uuid(9), "K2ABC");

    const rows = selectRoster(store.getState(), Date.parse("2026-07-16T00:05:00Z"));
    expect(rows[0].addedAt).toBe("2026-07-16T00:05:00.000Z");
  });

  it("buildRoster surfaces signalReport and staying on authoritative rows", () => {
    const store = createSessionStore();
    store.getState().seedFromSnapshot(
      summary({
        latestSeq: 1,
        roster: [
          {
            checkInId: uuid(42),
            callsign: "W1AW",
            addedAt: "t",
            addedBy: uuid(200),
            source: "staff",
            signalReport: "579",
            staying: "in-and-out",
            precedence: "routine",
            worked: false,
            version: 1,
            corrections: [],
          },
        ],
      }),
    );
    const rows = selectRoster(store.getState());
    expect(rows[0]).toMatchObject({
      callsign: "W1AW",
      optimistic: false,
      signalReport: "579",
      staying: "in-and-out",
    });
  });

  it("buildRoster derives working/worked/notes and stateFromSummary reads the cursor + net note", () => {
    const store = createSessionStore();
    store.getState().seedFromSnapshot(
      summary({
        latestSeq: 3,
        workingCheckInId: uuid(42),
        netNote: "Weekly traffic net",
        roster: [
          {
            checkInId: uuid(42),
            callsign: "W1AW",
            addedAt: "t",
            addedBy: uuid(200),
            source: "staff",
            staying: "in-and-out",
            precedence: "routine",
            worked: false,
            version: 1,
            corrections: [],
          },
          {
            checkInId: uuid(43),
            callsign: "N1CCK",
            addedAt: "t",
            addedBy: uuid(200),
            source: "staff",
            staying: "in-and-out",
            precedence: "routine",
            notes: "handled 2 pieces",
            worked: true,
            version: 2,
            corrections: [],
          },
        ],
      }),
    );
    // The session-level cursor + net note fold onto SessionState.
    expect(store.getState().session.workingCheckInId).toBe(uuid(42));
    expect(store.getState().session.netNote).toBe("Weekly traffic net");
    const rows = selectRoster(store.getState());
    expect(rows[0]).toMatchObject({ working: true, worked: false, notes: null });
    // 43 has been worked (cursor left it) and carries a note.
    expect(rows[1]).toMatchObject({ working: false, worked: true, notes: "handled 2 pieces" });
  });

  it("a snapshot with no connections key seeds an empty set rather than throwing the roster render", () => {
    // `connections` was once read straight through where
    // every neighbour in `stateFromSummary` is defensive. An absent key made
    // `viaLabel` call `.find` on `undefined`, which throws — taking the WHOLE
    // roster render down, not one cell.
    const store = createSessionStore();
    const { connections: _dropped, ...withoutConnections } = summary({
      latestSeq: 5,
      roster: [
        {
          checkInId: uuid(42),
          callsign: "W1AW",
          addedAt: "t",
          source: "staff",
          via: { kind: "connection", connectionId: uuid(99) },
        },
      ],
    } as unknown as Partial<SessionSummaryBody>);
    store
      .getState()
      .seedFromSnapshot(withoutConnections as unknown as SessionSummaryBody);
    expect(store.getState().session.connections).toEqual([]);
    const rows = selectRoster(store.getState());
    // And the row still renders — as UNRESOLVABLE, which is the truthful answer
    // for a `via` that cannot be resolved against the set on hand.
    expect(rows[0].viaLabel).toBe(UNRESOLVABLE_VIA_LABEL);
  });

  it("a PUBLIC snapshot carrying staying/precedence/traffic/publicNote seeds THOSE values, not the absent-key defaults", () => {
    // The snapshot half of the frontend confinement. The
    // account-less `GET /…/live` body is what seeds this store on first paint;
    // if the server's projection is reverted, the fields arrive absent and
    // `stateFromSummary`'s historical defaults quietly supply `in-and-out` /
    // `routine` / `null`. Seeded NON-DEFAULT so the two cases are
    // distinguishable.
    const store = createSessionStore();
    store.getState().seedFromSnapshot(
      summary({
        latestSeq: 5,
        roster: [
          {
            checkInId: uuid(42),
            callsign: "W1AW",
            addedAt: "t",
            source: "staff",
            staying: "staying-for-comments",
            precedence: "emergency",
            traffic: 3,
            publicNote: "relaying for the county EOC",
            via: null,
          },
        ],
      } as unknown as Partial<SessionSummaryBody>),
    );
    const rows = selectRoster(store.getState());
    expect(rows[0]).toMatchObject({
      staying: "staying-for-comments",
      precedence: "emergency",
      traffic: 3,
      publicNote: "relaying for the county EOC",
      via: null,
    });
    // The redacted-at-the-server fields still fold to their safe absentee values.
    expect(rows[0].signalReport).toBeNull();
    expect(rows[0].notes).toBeNull();
  });

  it("a public snapshot with no netNote folds netNote to null", () => {
    const store = createSessionStore();
    // The public view carries workingCheckInId (public radio data) but never
    // netNote — it must fold cleanly to null, not undefined.
    store.getState().seedFromSnapshot(
      summary({ latestSeq: 1, workingCheckInId: uuid(42), netNote: undefined as never }),
    );
    expect(store.getState().session.netNote).toBeNull();
    expect(store.getState().session.workingCheckInId).toBe(uuid(42));
  });

  it("setConnection updates the connection state", () => {
    const store = createSessionStore();
    store.getState().setConnection("out-of-sync");
    expect(store.getState().connection).toBe("out-of-sync");
    store.getState().setConnection("live");
    expect(store.getState().connection).toBe("live");
  });
});

describe("sessionStore — ephemeral soft-lock slice", () => {
  const uuidLocal = uuid;

  it("applyLock adds a lock entry to the locks slice WITHOUT touching SessionState", () => {
    const store = createSessionStore();
    store.getState().seedFromSnapshot(
      summary({
        latestSeq: 1,
        roster: [
          { checkInId: uuid(42), callsign: "W1AW", addedAt: "t", addedBy: uuidLocal(200), source: "staff", staying: "in-and-out", precedence: "routine", worked: false, version: 1, corrections: [] },
        ],
      }),
    );
    const sessionBefore = store.getState().session;
    store.getState().applyLock({
      checkInId: uuid(42),
      holderCallsign: "W2BCD",
      expiresAt: "2026-07-16T00:00:15Z",
    });
    const s = store.getState();
    expect(s.locks.get(uuid(42))).toEqual({
      holderCallsign: "W2BCD",
      expiresAt: "2026-07-16T00:00:15Z",
    });
    // The fold state is untouched — a lock is not a session-state transition.
    expect(s.session).toBe(sessionBefore);
  });

  it("applyLock with a null holder (release) drops the lock entry", () => {
    const store = createSessionStore();
    store.getState().applyLock({ checkInId: uuid(42), holderCallsign: "W2BCD", expiresAt: "t" });
    store.getState().applyLock({ checkInId: uuid(42), holderCallsign: null, expiresAt: null });
    expect(store.getState().locks.has(uuid(42))).toBe(false);
  });

  it("releasing an absent lock returns the SAME map reference (no needless re-notify)", () => {
    const store = createSessionStore();
    const before = store.getState().locks;
    store.getState().applyLock({ checkInId: uuidLocal(99), holderCallsign: null, expiresAt: null });
    expect(store.getState().locks).toBe(before);
  });

  it("a snapshot re-seed clears the ephemeral locks slice", () => {
    const store = createSessionStore();
    store.getState().applyLock({ checkInId: uuid(42), holderCallsign: "W2BCD", expiresAt: "t" });
    expect(store.getState().locks.size).toBe(1);
    store.getState().seedFromSnapshot(summary({ latestSeq: 2 }));
    expect(store.getState().locks.size).toBe(0);
  });

  it("selectRoster marks a locked-by-another row read-only and carries corrections/version", () => {
    const store = createSessionStore();
    store.getState().seedFromSnapshot(
      summary({
        latestSeq: 1,
        roster: [
          {
            checkInId: uuid(42),
            callsign: "W1AW",
            addedAt: "t",
            addedBy: uuidLocal(200),
            source: "staff",
            staying: "in-and-out",
            precedence: "routine",
            worked: false,
            version: 3,
            corrections: [{ field: "callsign", from: "W1AV", to: "W1AW", at: "t" }],
          },
        ],
      }),
    );
    store.getState().applyLock({ checkInId: uuid(42), holderCallsign: "W2BCD", expiresAt: "t" });
    const rows = selectRoster(store.getState());
    expect(rows[0].lockedBy).toBe("W2BCD");
    expect(rows[0].version).toBe(3);
    expect(rows[0].corrections).toHaveLength(1);
  });

  it("a lock entry self-expires client-side once its own expiresAt has passed", () => {
    // The lock slice is never event-sourced/resynced — a lost release
    // frame (broadcast lag, or a resumed WS connection) could otherwise leave
    // a "locked by another" flag stuck forever. Honoring the lease's own
    // expiresAt client-side bounds that staleness to the TTL instead.
    const store = createSessionStore();
    store.getState().seedFromSnapshot(
      summary({
        latestSeq: 1,
        roster: [
          { checkInId: uuid(42), callsign: "W1AW", addedAt: "t", addedBy: uuidLocal(200), source: "staff", staying: "in-and-out", precedence: "routine", worked: false, version: 1, corrections: [] },
        ],
      }),
    );
    store.getState().applyLock({
      checkInId: uuid(42),
      holderCallsign: "W2BCD",
      expiresAt: "2026-01-01T00:00:15.000Z",
    });

    const beforeExpiry = selectRoster(store.getState(), Date.parse("2026-01-01T00:00:14.000Z"));
    expect(beforeExpiry[0].lockedBy).toBe("W2BCD");

    const afterExpiry = selectRoster(store.getState(), Date.parse("2026-01-01T00:00:16.000Z"));
    expect(afterExpiry[0].lockedBy).toBeNull();

    // The raw slice itself is untouched — only the DERIVED render view treats
    // it as expired (no frame is needed to correct it later; the next real
    // `lock` frame, if any, still applies normally).
    expect(store.getState().locks.get(uuid(42))?.holderCallsign).toBe("W2BCD");
  });

  it("buildRoster treats an unparseable expiresAt as not-expired (defensive, matches existing lock fixtures)", () => {
    const roster = [
      { checkInId: uuid(42), callsign: "W1AW", addedAt: "t", addedBy: null, signalReport: null, source: "staff" as const, staying: "in-and-out" as const, name: null, location: null, grid: null, precedence: "routine" as const, traffic: null, notes: null, publicNote: null, via: null, relayedBy: null, worked: false, version: 1, corrections: [] },
    ];
    const locks = new Map([[uuid(42), { holderCallsign: "W2BCD", expiresAt: "not-a-date" }]]);
    const rows = buildRoster(
      roster,
      new Map(),
      locks,
      null,
      Date.parse("2026-01-01T00:00:00Z"),
      null,
      [],
    );
    expect(rows[0].lockedBy).toBe("W2BCD");
  });
});

describe("the roster ordering mode", () => {
  const modeEntry = (id: number, callsign: string, worked = false) => ({
    checkInId: uuid(id),
    callsign,
    addedAt: "t",
    addedBy: null,
    source: "staff" as const,
    signalReport: null,
    staying: "in-and-out" as const,
    name: null,
    location: null,
    grid: null,
    precedence: "routine" as const,
    traffic: null,
    notes: null,
    publicNote: null,
    via: null,
    relayedBy: null,
    worked,
    version: 1,
    corrections: [],
  });

  it("stateFromSummary reads the ordering mode off the folded summary", () => {
    const store = createSessionStore();
    store
      .getState()
      .seedFromSnapshot(
        summary({ rosterOrderMode: "worked-sink" } as Partial<SessionSummaryBody>),
      );
    expect(store.getState().session.rosterOrderMode).toBe("worked-sink");
  });

  it("a summary with no ordering mode (the public view) seeds the legacy default", () => {
    const store = createSessionStore();
    store.getState().seedFromSnapshot(summary());
    expect(store.getState().session.rosterOrderMode).toBe("manual");
  });

  it("buildRoster performs NO sort and NO partition of its own", () => {
    // Order is a SHARED, server-authoritative fact. Handed a
    // roster with worked entries interleaved, buildRoster must hand it back in
    // exactly that order, with this client's optimistic rows appended last.
    const interleaved = [
      modeEntry(1, "W1AAA", true),
      modeEntry(2, "W1BBB"),
      modeEntry(3, "W1CCC", true),
      modeEntry(4, "W1DDD"),
    ];
    const pending = new Map([
      [
        uuid(9),
        {
          clientEventId: uuid(9),
          callsign: "W1EEE",
          source: "staff" as const,
          signalReport: null,
          staying: "in-and-out" as const,
          via: null,
          relayedBy: null,
        },
      ],
    ]);
    const rows = buildRoster(interleaved, pending, new Map(), null, Date.now(), null, []);
    expect(rows.map((r) => r.callsign)).toEqual([
      "W1AAA",
      "W1BBB",
      "W1CCC",
      "W1DDD",
      "W1EEE",
    ]);
    expect(rows.map((r) => r.optimistic)).toEqual([false, false, false, false, true]);
  });
});

describe("selectYourTurnCheckInId (the YourTurnIndicator selector)", () => {
  const entry = (id: number, callsign: string, worked = false): RosterEntry => ({
    checkInId: uuid(id),
    callsign,
    addedAt: "t",
    addedBy: null,
    source: "self",
    signalReport: null,
    staying: "in-and-out",
    name: null,
    location: null,
    grid: null,
    precedence: "routine",
    traffic: null,
    notes: null,
    publicNote: null,
    via: null,
    relayedBy: null,
    worked,
    version: 1,
    corrections: [],
  });

  const roster: RosterEntry[] = [
    entry(1, "W1AW"),
    entry(2, "N1CCK"),
    entry(3, "K1XYZ"),
  ];

  it("returns null when no station is being worked (cursor unset)", () => {
    expect(selectYourTurnCheckInId(roster, null, "N1CCK", "manual")).toBeNull();
  });

  it("returns null when the viewer has no callsign", () => {
    expect(selectYourTurnCheckInId(roster, uuid(1), null, "manual")).toBeNull();
  });

  it("lights the viewer's own row when it is the first non-worked entry after the cursor", () => {
    // Working W1AW (index 0); the next non-worked entry is N1CCK — the viewer.
    expect(selectYourTurnCheckInId(roster, uuid(1), "N1CCK", "manual")).toBe(uuid(2));
    // Case-insensitive callsign match (account callsign may be stored lowercase).
    expect(selectYourTurnCheckInId(roster, uuid(1), "n1cck", "manual")).toBe(uuid(2));
  });

  it("returns null when the viewer's own row is NOT the next up (another precedes it)", () => {
    // Working W1AW; next up is N1CCK, so K1XYZ's viewer is not next.
    expect(selectYourTurnCheckInId(roster, uuid(1), "K1XYZ", "manual")).toBeNull();
  });

  it("returns null when the viewer's own row is the one currently being worked (not 'next')", () => {
    // The cursor entry is the viewer's own — being worked NOW, not next up.
    expect(selectYourTurnCheckInId(roster, uuid(2), "N1CCK", "manual")).toBeNull();
  });

  it("skips worked entries when computing next up", () => {
    // W1AW worked-and-left is the cursor; N1CCK is already worked; K1XYZ is next.
    const withWorked: RosterEntry[] = [
      entry(1, "W1AW"),
      entry(2, "N1CCK", true),
      entry(3, "K1XYZ"),
    ];
    expect(selectYourTurnCheckInId(withWorked, uuid(1), "K1XYZ", "manual")).toBe(uuid(3));
    expect(selectYourTurnCheckInId(withWorked, uuid(1), "N1CCK", "manual")).toBeNull();
  });

  it("names the top of the UNWORKED group under a sunk roster", () => {
    // Under worked-sink the server has already partitioned the shared order:
    // the working station heads the roster, the rest of the unworked group
    // follows, and the worked block sits last. "Next up" must therefore be the
    // entry immediately after the cursor, and never a station in the worked
    // block below it.
    //
    // This fixture answers the same under BOTH modes — the cursor
    // is at the head — so it is asserted under its own premise, worked-sink.
    const sunk: RosterEntry[] = [
      entry(1, "W1AW"),
      entry(2, "N1CCK"),
      entry(3, "K1XYZ"),
      entry(4, "W2ABC", true),
      entry(5, "K5DEF", true),
    ];
    expect(selectYourTurnCheckInId(sunk, uuid(1), "N1CCK", "worked-sink")).toBe(uuid(2));
    expect(selectYourTurnCheckInId(sunk, uuid(1), "K1XYZ", "worked-sink")).toBeNull();
    expect(selectYourTurnCheckInId(sunk, uuid(1), "W2ABC", "worked-sink")).toBeNull();
  });

  // The two tests that follow were RE-AUTHORED from
  // "scans from a cursor at a NON-ZERO index under a sunk roster"
  // and "declines rather than guessing when only worked rows follow the cursor".
  // Both asserted the strictly-forward scan as correct under a SUNK roster; that
  // was the shipped narrowing, and it is what is now reversed for the
  // worked-sink mode only. Under manual ordering every original assertion still
  // stands, so each is split by mode rather than deleted.
  //
  // The round-2 fixture. From round 2 on, the entry holding the cursor is itself
  // `worked` — the server exempts it from the sink but does not PROMOTE it, so
  // a station still awaiting its turn can sit ABOVE the cursor.
  const round2: RosterEntry[] = [
    entry(1, "W1AW"), // C: still awaiting its turn, ABOVE the cursor
    entry(2, "N1CCK", true), // A: worked in round 1, holds the cursor in round 2
    entry(3, "K1XYZ"), // F: checked in after the cursor moved — awaiting its turn
    entry(4, "W2ABC", true), // B: sunk
  ];

  it("under worked-sink, 'next up' is the top of the UNWORKED group — even ABOVE the cursor", () => {
    // The epic's own worked example, [C, A(cursor), F, B]: the NCS will call C
    // next, because the sink has put C at the top of the unworked group. Naming
    // F here would tell F "You're next up" while C is called — a wrong positive
    // delivered to a named participant, not a narrowing.
    expect(selectYourTurnCheckInId(round2, uuid(2), "W1AW", "worked-sink")).toBe(uuid(1));
    expect(selectYourTurnCheckInId(round2, uuid(2), "K1XYZ", "worked-sink")).toBeNull();
    expect(selectYourTurnCheckInId(round2, uuid(2), "W2ABC", "worked-sink")).toBeNull();
  });

  it("under manual ordering, the same fixture keeps the strictly-forward scan byte-for-byte", () => {
    // Under manual ordering position IS the running order, and a station above
    // the cursor was deliberately passed. Promoting it would start telling
    // skipped stations they are next. F is named; C is not.
    expect(selectYourTurnCheckInId(round2, uuid(2), "K1XYZ", "manual")).toBe(uuid(3));
    expect(selectYourTurnCheckInId(round2, uuid(2), "W1AW", "manual")).toBeNull();
    expect(selectYourTurnCheckInId(round2, uuid(2), "W2ABC", "manual")).toBeNull();
  });

  // The bare round-2 permutation: C awaits its turn ABOVE the re-worked station
  // holding the cursor, and everything below the cursor is worked.
  const onlyWorkedBelow: RosterEntry[] = [
    entry(1, "W1AW"),
    entry(2, "N1CCK", true),
    entry(3, "K1XYZ", true),
  ];

  it("under worked-sink, names the station above the cursor when only worked rows follow it", () => {
    // This is the null-shaped half of the same defect: the forward scan found
    // only worked rows and declined, while the NCS was about to call W1AW.
    expect(selectYourTurnCheckInId(onlyWorkedBelow, uuid(2), "W1AW", "worked-sink")).toBe(
      uuid(1),
    );
    expect(selectYourTurnCheckInId(onlyWorkedBelow, uuid(2), "K1XYZ", "worked-sink")).toBeNull();
  });

  it("under manual ordering, declines rather than guessing when only worked rows follow the cursor", () => {
    // No participant is named — under manual ordering no indicator beats a
    // wrong one, and the station above the cursor was passed on purpose.
    expect(selectYourTurnCheckInId(onlyWorkedBelow, uuid(2), "W1AW", "manual")).toBeNull();
    expect(selectYourTurnCheckInId(onlyWorkedBelow, uuid(2), "K1XYZ", "manual")).toBeNull();
  });

  it("under worked-sink in round 1, the UNWORKED cursor entry at the head is never 'next up'", () => {
    // In round 1 the cursor entry has not been worked yet, and under the sink it
    // heads the roster. `!worked` alone would name the station being worked
    // RIGHT NOW as next up; the cursor exemption is the other half of the rule.
    const round1: RosterEntry[] = [
      entry(1, "W1AW"), // the cursor, unworked, at the head
      entry(2, "N1CCK"),
      entry(3, "K1XYZ", true),
    ];
    expect(selectYourTurnCheckInId(round1, uuid(1), "W1AW", "worked-sink")).toBeNull();
    expect(selectYourTurnCheckInId(round1, uuid(1), "N1CCK", "worked-sink")).toBe(uuid(2));
  });

  it("returns null when the cursor points at an entry not on the roster", () => {
    expect(selectYourTurnCheckInId(roster, uuid(99), "N1CCK", "manual")).toBeNull();
  });
});

describe("displayedConnection", () => {
  it("surfaces net-paused when the transport is live and control is stalled", () => {
    expect(displayedConnection("live", "stalled")).toBe("net-paused");
  });

  it("stays live when the transport is live and control is active", () => {
    expect(displayedConnection("live", "active")).toBe("live");
  });

  it("lets transport degradation dominate a stalled control status", () => {
    // I'm not even seeing live data — my degraded transport wins over the pause.
    expect(displayedConnection("catching-up", "stalled")).toBe("catching-up");
    expect(displayedConnection("out-of-sync", "stalled")).toBe("out-of-sync");
    expect(displayedConnection("catching-up", "active")).toBe("catching-up");
  });
});

describe("countDistinctStates (post-net summary's States/provinces stat)", () => {
  // There is no structured per-check-in state field (only the net DEFINITION
  // carries one) — this is a best-effort parse of each entry's free-text
  // `location` ("Atlanta GA", "Ottawa, ON"): the trailing two-letter token.
  it("counts distinct trailing two-letter state/province codes", () => {
    expect(countDistinctStates(["Atlanta GA", "Boise ID", "Provo UT"])).toBe(3);
  });

  it("de-duplicates the same code across multiple entries", () => {
    expect(countDistinctStates(["Atlanta GA", "Savannah GA"])).toBe(1);
  });

  it("is case-insensitive", () => {
    expect(countDistinctStates(["Atlanta ga", "Boise ID"])).toBe(2);
  });

  it("ignores a null location and a string with no trailing two-letter code", () => {
    expect(countDistinctStates(["Atlanta GA", null, "unknown"])).toBe(1);
  });

  it("returns 0 for an empty roster", () => {
    expect(countDistinctStates([])).toBe(0);
  });
});


describe("selectDuplicateCheckIn (the duplicate check-in derivation)", () => {
  /**
   * A display roster built through the real snapshot → `selectRoster` path, so
   * the selector meets the exact shape the console renders rather than a
   * hand-rolled literal that could drift from it.
   */
  function rosterOf(
    entries: readonly {
      readonly callsign: string;
      readonly worked: boolean;
      readonly working?: boolean;
    }[],
  ): DisplayRosterEntry[] {
    const store = createSessionStore();
    const cursorIndex = entries.findIndex((entry) => entry.working === true);
    store.getState().seedFromSnapshot(
      summary({
        workingCheckInId: cursorIndex === -1 ? null : uuid(500 + cursorIndex),
        roster: entries.map((entry, index) => ({
          checkInId: uuid(500 + index),
          callsign: entry.callsign,
          addedAt: "t",
          addedBy: uuid(200),
          source: "staff" as const,
          staying: "in-and-out" as const,
          precedence: "routine" as const,
          worked: entry.worked,
          version: 1,
          corrections: [],
        })),
      }),
    );
    return selectRoster(store.getState());
  }

  it("reports unworked when a roster entry carries the callsign and has not been worked", () => {
    const roster = rosterOf([{ callsign: "W1AW", worked: false }]);
    expect(selectDuplicateCheckIn(roster, "W1AW")).toBe("unworked");
  });

  it("reports worked when the only matching entry has already been worked", () => {
    const roster = rosterOf([{ callsign: "W1AW", worked: true }]);
    expect(selectDuplicateCheckIn(roster, "W1AW")).toBe("worked");
  });

  it("prefers the outstanding entry when the same callsign is on the roster both worked and unworked", () => {
    // First-match order deliberately puts the WORKED one first: a `find`-first
    // implementation would report the station finished while one entry still
    // awaits its turn.
    const roster = rosterOf([
      { callsign: "W1AW", worked: true },
      { callsign: "W1AW", worked: false },
    ]);
    expect(selectDuplicateCheckIn(roster, "W1AW")).toBe("unworked");
  });

  it("reports unworked for a match that currently holds the working cursor", () => {
    // The cursor sitting ON a station means it has not been completed.
    const roster = rosterOf([{ callsign: "W1AW", worked: false, working: true }]);
    expect(selectDuplicateCheckIn(roster, "W1AW")).toBe("unworked");
  });

  it("reports none for a callsign absent from the roster", () => {
    const roster = rosterOf([{ callsign: "W1AW", worked: false }]);
    expect(selectDuplicateCheckIn(roster, "K2ABC")).toBe("none");
  });

  it("reports none for an empty roster", () => {
    expect(selectDuplicateCheckIn([], "W1AW")).toBe("none");
  });

  it("reports none for an empty or whitespace-only callsign", () => {
    const roster = rosterOf([{ callsign: "W1AW", worked: false }]);
    expect(selectDuplicateCheckIn(roster, "")).toBe("none");
    expect(selectDuplicateCheckIn(roster, "   ")).toBe("none");
  });

  it("matches the callsign case-insensitively, as the server's normalization does", () => {
    const roster = rosterOf([{ callsign: "N1CCK", worked: false }]);
    expect(selectDuplicateCheckIn(roster, "n1cck")).toBe("unworked");
  });

  it("matches the callsign after trimming, as the server's normalization does", () => {
    const roster = rosterOf([{ callsign: "W1AW", worked: false }]);
    expect(selectDuplicateCheckIn(roster, "  W1AW  ")).toBe("unworked");
  });

  it("matches a portable-designator callsign against the bare roster entry", () => {
    // `parse_callsign` strips every `/`-segment, so W1AW/P and DL/W1AW/P both
    // land on the roster as W1AW — they ARE this station, one round-trip later.
    const roster = rosterOf([{ callsign: "W1AW", worked: false }]);
    expect(selectDuplicateCheckIn(roster, "W1AW/P")).toBe("unworked");
    expect(selectDuplicateCheckIn(roster, "DL/W1AW/P")).toBe("unworked");
  });

  it("carries the worked state through a portable-designator match", () => {
    const roster = rosterOf([{ callsign: "W1AW", worked: true }]);
    expect(selectDuplicateCheckIn(roster, "DL/W1AW/P")).toBe("worked");
  });

  it("does not match a station whose base is absent from the roster", () => {
    // Renamed 2026-08-29: this fixture shares NO segment with `W1AW/P`, so it
    // never exercised over-matching — the genuine collision cases below do.
    const roster = rosterOf([{ callsign: "K2ABC", worked: false }]);
    expect(selectDuplicateCheckIn(roster, "W1AW/P")).toBe("none");
  });

  /**
   * Optimistic pending rows carry the operator's RAW typed text — `addPending`
   * stores it unnormalized, and `selectRoster` folds it into the same display
   * roster the selector reads. Every case below therefore meets an entry the
   * server has never seen: arbitrary case, designators intact, and not
   * necessarily a valid callsign at all.
   */
  function rosterWithPending(typedText: string): DisplayRosterEntry[] {
    const store = createSessionStore();
    store.getState().seedFromSnapshot(summary({ roster: [] }));
    store.getState().addPending(uuid(900), typedText);
    return selectRoster(store.getState());
  }

  it("matches a pending row typed with a designator against the bare base typed next", () => {
    // The portable station double-entered inside the echo window: the pending
    // row still reads `W1AW/P`, the server will store both as `W1AW`.
    expect(selectDuplicateCheckIn(rosterWithPending("W1AW/P"), "W1AW")).toBe("unworked");
  });

  it("matches a pending row against a byte-identical repeat of what was typed", () => {
    expect(selectDuplicateCheckIn(rosterWithPending("W1AW/P"), "W1AW/P")).toBe("unworked");
    expect(selectDuplicateCheckIn(rosterWithPending("w1aw/p"), "W1AW/P")).toBe("unworked");
  });

  it("does not match a pending row that collides only on a designator segment", () => {
    // Two different portable stations, both `/P`. The shared segment is a
    // designator on BOTH sides, so nothing here is the same station.
    expect(selectDuplicateCheckIn(rosterWithPending("K2ABC/P"), "W1AW/P")).toBe("none");
    expect(selectDuplicateCheckIn(rosterWithPending("K2ABC/MM"), "W1AW/MM")).toBe("none");
  });

  it("does not match a pending row whose whole text is a designator, not a base", () => {
    // `DL` and `KP4` are prefixes, not assigned base calls — the server refuses
    // both, and `DL/N1CCK` is N1CCK checking in, not a repeat of either.
    expect(selectDuplicateCheckIn(rosterWithPending("DL"), "DL/N1CCK")).toBe("none");
    expect(selectDuplicateCheckIn(rosterWithPending("KP4"), "KP4/N1CCK")).toBe("none");
  });
});

describe("the way-in stamp slice", () => {
  const stampedConnection = { kind: "connection", connectionId: hfWay().id } as const;

  it("starts unset — nothing seeds it from the session's connections", () => {
    // Asserted as an EQUALITY against the observed seeded state, not as
    // "it isn't connections[0]": a criterion guarding an OMISSION goes slack the
    // moment it only refuses one particular substitution. The stamp is `null`
    // and nothing else.
    const store = createSessionStore();
    expect(store.getState().viaStamp).toBeNull();

    store.getState().seedFromSnapshot(summary());
    expect(store.getState().viaStamp).toBeNull();
  });

  it("stays unset on a ONE-connection session, where defaulting looks unarguable", () => {
    // The named hazard. A single-connection net already exports correctly
    // with `via` absent, through the ADIF-only fallback, so a default buys
    // nothing and costs the "not recorded" fact. Every other fixture leaves
    // this shortcut unexercised.
    const store = createSessionStore();
    store.getState().seedFromSnapshot(summary({ connections: [hfWay()] }));
    expect(store.getState().viaStamp).toBeNull();
  });

  it("survives seedFromSnapshot — it is a SIBLING slice, not part of `session`", () => {
    // `seedFromSnapshot` replaces `session` wholesale on every
    // authoritative reset (initial load, catch-up, post-1008 re-snapshot), so a
    // stamp held inside it is silently wiped the first time the socket resyncs
    // mid-net — which the operator would discover only from a run of check-ins
    // that recorded nothing.
    const store = createSessionStore();
    store.getState().seedFromSnapshot(summary());
    store.getState().setViaStamp(stampedConnection);

    store.getState().seedFromSnapshot(summary({ latestSeq: 9 }));

    expect(store.getState().viaStamp).toEqual(stampedConnection);
  });

  it("returns to unset when a reseed no longer lists the stamped connection", () => {
    // The stamp deliberately SURVIVES a
    // resync (the test above), which is what strands it when the owner edits
    // the connection list mid-net: `sendableVia` then refuses the dangling id
    // and EVERY subsequent check-in silently records no way in, while the
    // control still claims a stamp is set. Clearing it here is the only place
    // that sees both the surviving stamp and the incoming connection set.
    const store = createSessionStore();
    store.getState().seedFromSnapshot(summary());
    store.getState().setViaStamp(stampedConnection);

    store.getState().seedFromSnapshot(
      summary({ connections: [{ ...hfWay(), id: uuid(0x99) }] }),
    );

    expect(store.getState().viaStamp).toBeNull();
  });

  it("keeps a FREE-TEXT stamp across the same reseed — it resolves against nothing", () => {
    // The clearing rule is narrower than "clear on reseed". Free text names no
    // connection, so no snapshot can strand it, and wiping it would take back
    // the very thing `viaStamp`'s exclusion from the reset exists to protect.
    const store = createSessionStore();
    store.getState().seedFromSnapshot(summary());
    store.getState().setViaStamp({ kind: "unlisted", text: "Bob's hotspot" });

    store.getState().seedFromSnapshot(
      summary({ connections: [{ ...hfWay(), id: uuid(0x99) }] }),
    );

    expect(store.getState().viaStamp).toEqual({ kind: "unlisted", text: "Bob's hotspot" });
  });

  it("clears back to unset, so the operator can return to recording nothing", () => {
    const store = createSessionStore();
    store.getState().setViaStamp(stampedConnection);
    store.getState().setViaStamp(null);
    expect(store.getState().viaStamp).toBeNull();
  });

  it("holds free text as its own shape, never collapsed to the connection one", () => {
    const store = createSessionStore();
    store.getState().setViaStamp({ kind: "unlisted", text: "Bob's hotspot" });
    expect(store.getState().viaStamp).toEqual({
      kind: "unlisted",
      text: "Bob's hotspot",
    });
  });
});

describe("the optimistic row's way in", () => {
  it("renders the way-in LABEL on a pending row, before any echo arrives", () => {
    // Without this the operator stamps HF, commits, and watches a row appear
    // with no way in — which teaches them the stamp did not take, on the one
    // surface whose whole job is to reassure them it did.
    const store = createSessionStore();
    store.getState().seedFromSnapshot(summary());
    store
      .getState()
      .addPending(uuid(900), "N1CCK", null, "in-and-out", "staff", {
        kind: "connection",
        connectionId: hfWay().id,
      });

    const [row] = selectRoster(store.getState());
    expect(row.optimistic).toBe(true);
    expect(row.via).toEqual({ kind: "connection", connectionId: hfWay().id });
    expect(row.viaLabel).toBe("HF — 14.250 MHz");
  });

  it("shows free text on a pending row as the operator's own words", () => {
    const store = createSessionStore();
    store.getState().seedFromSnapshot(summary());
    store
      .getState()
      .addPending(uuid(901), "W1AW", null, "in-and-out", "staff", {
        kind: "unlisted",
        text: "Bob's hotspot",
      });

    const [row] = selectRoster(store.getState());
    expect(row.viaLabel).toBe("Bob's hotspot");
  });

  it("still shows NOT RECORDED on a pending row nobody stamped a way in for", () => {
    const store = createSessionStore();
    store.getState().seedFromSnapshot(summary());
    store.getState().addPending(uuid(902), "K2ABC");

    const [row] = selectRoster(store.getState());
    expect(row.via).toBeNull();
    expect(row.viaLabel).toBeNull();
  });

  it("resolves a pending row's way in against the LIVE connection set, like an echoed one", () => {
    // A mid-net QSY re-labels the row it is already on; a pending row must not
    // be the one place that keeps the old words.
    const store = createSessionStore();
    store.getState().seedFromSnapshot(summary());
    store
      .getState()
      .addPending(uuid(903), "N1CCK", null, "in-and-out", "staff", {
        kind: "connection",
        connectionId: hfWay().id,
      });

    const moved = { ...hfWay(), plannedFrequencyHz: 7_180_000, band: "40m" };
    const [row] = buildRoster(
      store.getState().session.roster,
      store.getState().pending,
      new Map(),
      null,
      Date.now(),
      null,
      [moved],
    );
    expect(row.viaLabel).toBe("HF — 7.180 MHz");
  });
});

describe("the relaying station reaches the roster row", () => {
  it("carries relayedBy from the folded entry onto the display row", () => {
    const store = createSessionStore();
    store.getState().seedFromSnapshot({
      ...summary(),
      roster: [
        {
          checkInId: uuid(42),
          callsign: "W1AW",
          addedAt: "2026-09-02T00:00:00Z",
          addedBy: null,
          source: "staff" as const,
          signalReport: null,
          staying: "in-and-out" as const,
          name: null,
          location: null,
          grid: null,
          precedence: "routine" as const,
          traffic: null,
          notes: null,
          publicNote: null,
          via: null,
          relayedBy: "W3REL",
          worked: false,
          version: 1,
          corrections: [],
        },
      ],
    } as never);
    const rows = selectRoster(store.getState());
    expect(rows[0].relayedBy).toBe("W3REL");
  });

  it("folds a snapshot entry with no relayedBy key to null, not to a blank string", () => {
    // The PUBLIC snapshot never carries the key, and neither does an older
    // entry. Both mean NOT RELAYED, and the row must say so by being
    // null rather than by rendering an empty relay line.
    const store = createSessionStore();
    store.getState().seedFromSnapshot({
      ...summary(),
      roster: [
        {
          checkInId: uuid(42),
          callsign: "W1AW",
          addedAt: "2026-09-02T00:00:00Z",
          addedBy: null,
          source: "staff" as const,
          signalReport: null,
          staying: "in-and-out" as const,
          name: null,
          location: null,
          grid: null,
          precedence: "routine" as const,
          traffic: null,
          notes: null,
          publicNote: null,
          via: null,
          worked: false,
          version: 1,
          corrections: [],
        },
      ],
    } as never);
    const rows = selectRoster(store.getState());
    expect(rows[0].relayedBy).toBeNull();
  });

  it("shows the operator's own answer on the optimistic row before the echo lands", () => {
    const store = createSessionStore();
    store.getState().seedFromSnapshot(summary() as never);
    store
      .getState()
      .addPending("ce-1", "K9XYZ", null, "in-and-out", "staff", null, "W3REL");
    const [pending] = selectRoster(store.getState()).filter((entry) => entry.optimistic);
    expect(pending?.relayedBy).toBe("W3REL");
  });
});
