// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
import { afterEach, describe, expect, it, vi } from "vitest";

import { ProblemError } from "../auth/authApi";
import {
  addCheckIn,
  changeFrequency,
  closeSession,
  exportUrl,
  getEventsSince,
  getPublicEventsSince,
  getPublicSession,
  getSession,
  grantRole,
  listRoles,
  lookupCheckInAutofill,
  lookupRosterMemory,
  releaseLock,
  reorderRoster,
  revokeRole,
  setNetNote,
  setWorkedStation,
  startSession,
  updateCheckIn,
} from "./sessionApi";

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


function jsonResponse(status: number, body: unknown): Response {
  return new Response(JSON.stringify(body), {
    status,
    headers: {
      "content-type":
        status >= 400 ? "application/problem+json" : "application/json",
    },
  });
}

afterEach(() => {
  vi.unstubAllGlobals();
});

const summaryBody = {
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
  roster: [],
  definition: {
    title: "Sunday Traffic Net",
    connections: [hfWay()],
    netCategory: "traffic",
    netType: "open",
  },
};

describe("sessionApi.getSession", () => {
  it("GETs the folded summary same-origin and parses it", async () => {
    const fetchMock = vi.fn().mockResolvedValue(jsonResponse(200, summaryBody));
    vi.stubGlobal("fetch", fetchMock);

    const result = await getSession("abc");

    expect(result?.latestSeq).toBe(1);
    const [url, init] = fetchMock.mock.calls[0] as [string, RequestInit];
    expect(url).toBe("/api/net-sessions/abc");
    expect(init.credentials).toBe("same-origin");
  });

  it("returns null on 401 (signed out), mirroring fetchCurrentAccount", async () => {
    const fetchMock = vi.fn().mockResolvedValue(jsonResponse(401, {}));
    vi.stubGlobal("fetch", fetchMock);

    expect(await getSession("abc")).toBeNull();
  });

  it("throws ProblemError on a non-401 failure (403 non-owner)", async () => {
    const fetchMock = vi
      .fn()
      .mockResolvedValue(jsonResponse(403, { type: "/errors/forbidden", status: 403 }));
    vi.stubGlobal("fetch", fetchMock);

    await expect(getSession("abc")).rejects.toBeInstanceOf(ProblemError);
  });
});

describe("sessionApi.getEventsSince", () => {
  it("GETs the catch-up gap with a ?since= cursor and parses the array", async () => {
    const events = [
      {
        seq: 2,
        kind: "checkin.added",
        at: "2026-07-16T00:00:05Z",
        payload: { checkInId: "00000000-0000-0000-0000-00000000002a", callsign: "W1AW" },
      },
    ];
    const fetchMock = vi.fn().mockResolvedValue(jsonResponse(200, events));
    vi.stubGlobal("fetch", fetchMock);

    const result = await getEventsSince("abc", 1);

    expect(result).toHaveLength(1);
    expect(result[0].seq).toBe(2);
    const [url] = fetchMock.mock.calls[0] as [string];
    expect(url).toBe("/api/net-sessions/abc/events?since=1");
  });

  it("throws ProblemError on failure", async () => {
    const fetchMock = vi
      .fn()
      .mockResolvedValue(jsonResponse(404, { type: "/errors/net-session-not-found", status: 404 }));
    vi.stubGlobal("fetch", fetchMock);

    await expect(getEventsSince("abc", 0)).rejects.toBeInstanceOf(ProblemError);
  });
});

describe("sessionApi.startSession", () => {
  it("POSTs the start body and returns the created summary", async () => {
    const fetchMock = vi.fn().mockResolvedValue(jsonResponse(201, summaryBody));
    vi.stubGlobal("fetch", fetchMock);

    const result = await startSession({
      definitionId: "00000000-0000-0000-0000-000000000007",
    });

    expect(result.id).toBe(summaryBody.id);
    const [url, init] = fetchMock.mock.calls[0] as [string, RequestInit];
    expect(url).toBe("/api/net-sessions");
    expect(init.method).toBe("POST");
    // The start body is the definition id and nothing else.
    expect(JSON.parse(init.body as string)).toEqual({
      definitionId: "00000000-0000-0000-0000-000000000007",
    });
  });

  it("throws ProblemError on 409 already-live", async () => {
    const fetchMock = vi
      .fn()
      .mockResolvedValue(jsonResponse(409, { type: "/errors/session-already-live", status: 409 }));
    vi.stubGlobal("fetch", fetchMock);

    await expect(
      startSession({ definitionId: "x" }),
    ).rejects.toBeInstanceOf(ProblemError);
  });
});

describe("sessionApi.changeFrequency", () => {
  it("POSTs the camelCase frequency body same-origin and returns the summary", async () => {
    const fetchMock = vi.fn().mockResolvedValue(jsonResponse(200, summaryBody));
    vi.stubGlobal("fetch", fetchMock);

    const result = await changeFrequency("abc", "conn-1", "146.52");

    expect(result.id).toBe(summaryBody.id);
    const [url, init] = fetchMock.mock.calls[0] as [string, RequestInit];
    expect(url).toBe("/api/net-sessions/abc/frequency");
    expect(init.method).toBe("POST");
    expect(init.credentials).toBe("same-origin");
    // WHICH way in moved rides the body beside the new frequency.
    expect(JSON.parse(init.body as string)).toEqual({
      connectionId: "conn-1",
      operatingFrequency: "146.52",
    });
  });

  it("throws ProblemError on a 409 already-closed (race-close during edit)", async () => {
    const fetchMock = vi
      .fn()
      .mockResolvedValue(
        jsonResponse(409, { type: "/errors/session-already-closed", status: 409 }),
      );
    vi.stubGlobal("fetch", fetchMock);

    await expect(changeFrequency("abc", "conn-1", "146.52")).rejects.toBeInstanceOf(
      ProblemError,
    );
  });
});

describe("sessionApi.closeSession", () => {
  it("POSTs to the close endpoint and returns the summary", async () => {
    const fetchMock = vi.fn().mockResolvedValue(jsonResponse(200, summaryBody));
    vi.stubGlobal("fetch", fetchMock);

    const result = await closeSession("abc");

    expect(result.id).toBe(summaryBody.id);
    const [url, init] = fetchMock.mock.calls[0] as [string, RequestInit];
    expect(url).toBe("/api/net-sessions/abc/close");
    expect(init.method).toBe("POST");
  });
});

describe("sessionApi.addCheckIn", () => {
  it("POSTs the callsign body same-origin and returns the summary", async () => {
    const fetchMock = vi.fn().mockResolvedValue(jsonResponse(201, summaryBody));
    vi.stubGlobal("fetch", fetchMock);

    const result = await addCheckIn("abc", { callsign: "N1CCK" });

    expect(result.id).toBe(summaryBody.id);
    const [url, init] = fetchMock.mock.calls[0] as [string, RequestInit];
    expect(url).toBe("/api/net-sessions/abc/check-ins");
    expect(init.method).toBe("POST");
    expect(init.credentials).toBe("same-origin");
    expect(JSON.parse(init.body as string)).toEqual({ callsign: "N1CCK" });
  });

  it("includes signalReport and staying in the body when supplied", async () => {
    const fetchMock = vi.fn().mockResolvedValue(jsonResponse(201, summaryBody));
    vi.stubGlobal("fetch", fetchMock);

    await addCheckIn("abc", {
      callsign: "N1CCK",
      signalReport: "599",
      staying: "staying-for-comments",
    });

    const [, init] = fetchMock.mock.calls[0] as [string, RequestInit];
    expect(JSON.parse(init.body as string)).toEqual({
      callsign: "N1CCK",
      signalReport: "599",
      staying: "staying-for-comments",
    });
  });

  it("throws ProblemError on a 400 callsign-invalid", async () => {
    const fetchMock = vi
      .fn()
      .mockResolvedValue(jsonResponse(400, { type: "/errors/callsign-invalid", status: 400 }));
    vi.stubGlobal("fetch", fetchMock);

    await expect(addCheckIn("abc", { callsign: "!!!" })).rejects.toBeInstanceOf(ProblemError);
  });

  it("includes name and location in the body when supplied", async () => {
    const fetchMock = vi.fn().mockResolvedValue(jsonResponse(201, summaryBody));
    vi.stubGlobal("fetch", fetchMock);

    await addCheckIn("abc", {
      callsign: "N1CCK",
      name: "Maria",
      location: "Hartford, CT",
    });

    const [, init] = fetchMock.mock.calls[0] as [string, RequestInit];
    expect(JSON.parse(init.body as string)).toEqual({
      callsign: "N1CCK",
      name: "Maria",
      location: "Hartford, CT",
    });
  });

  it("omits name/location from the body when absent (callsign-only add is byte-identical)", async () => {
    const fetchMock = vi.fn().mockResolvedValue(jsonResponse(201, summaryBody));
    vi.stubGlobal("fetch", fetchMock);

    await addCheckIn("abc", { callsign: "N1CCK" });

    const [, init] = fetchMock.mock.calls[0] as [string, RequestInit];
    const parsed = JSON.parse(init.body as string) as Record<string, unknown>;
    expect(parsed).toEqual({ callsign: "N1CCK" });
    expect("name" in parsed).toBe(false);
    expect("location" in parsed).toBe(false);
  });

  it("sends the structured `via` when the operator recorded a way in", async () => {
    const fetchMock = vi.fn().mockResolvedValue(jsonResponse(201, summaryBody));
    vi.stubGlobal("fetch", fetchMock);

    await addCheckIn("abc", {
      callsign: "N1CCK",
      via: { kind: "connection", connectionId: hfWay().id },
    });

    const [, init] = fetchMock.mock.calls[0] as [string, RequestInit];
    expect(JSON.parse(init.body as string)).toEqual({
      callsign: "N1CCK",
      via: { kind: "connection", connectionId: hfWay().id },
    });
  });

  it("sends a free-text `via` under its own shape, never flattened to a label", async () => {
    const fetchMock = vi.fn().mockResolvedValue(jsonResponse(201, summaryBody));
    vi.stubGlobal("fetch", fetchMock);

    await addCheckIn("abc", {
      callsign: "N1CCK",
      via: { kind: "unlisted", text: "Bob's hotspot" },
    });

    const [, init] = fetchMock.mock.calls[0] as [string, RequestInit];
    expect(JSON.parse(init.body as string)).toEqual({
      callsign: "N1CCK",
      via: { kind: "unlisted", text: "Bob's hotspot" },
    });
  });

  it("omits the `via` KEY ENTIRELY when no way in was recorded", async () => {
    // `Object.hasOwn`, never `toBeNull`: the failure shape is an absence
    // assertion that passes for a missing key AND for an explicit null alike,
    // and the two mean different things here — absent is "not recorded", null
    // on the ADD path is not a value the client ever sends.
    const fetchMock = vi.fn().mockResolvedValue(jsonResponse(201, summaryBody));
    vi.stubGlobal("fetch", fetchMock);

    await addCheckIn("abc", { callsign: "N1CCK" });

    const [, init] = fetchMock.mock.calls[0] as [string, RequestInit];
    const parsed = JSON.parse(init.body as string) as Record<string, unknown>;
    expect(Object.hasOwn(parsed, "via")).toBe(false);
  });
});

describe("sessionApi.lookupRosterMemory", () => {
  it("GETs the definition-scoped memory for the URL-encoded callsign and returns it", async () => {
    const fetchMock = vi
      .fn()
      .mockResolvedValue(jsonResponse(200, { name: "Maria", location: "Hartford, CT" }));
    vi.stubGlobal("fetch", fetchMock);

    const result = await lookupRosterMemory("abc", "N1CCK");

    expect(result).toEqual({ name: "Maria", location: "Hartford, CT" });
    const [url, init] = fetchMock.mock.calls[0] as [string, RequestInit];
    expect(url).toBe("/api/net-sessions/abc/roster-memory?callsign=N1CCK");
    expect(init.credentials).toBe("same-origin");
  });

  it("returns the empty shape on a miss (both null)", async () => {
    const fetchMock = vi
      .fn()
      .mockResolvedValue(jsonResponse(200, { name: null, location: null }));
    vi.stubGlobal("fetch", fetchMock);

    const result = await lookupRosterMemory("abc", "K9NEVER");
    expect(result).toEqual({ name: null, location: null });
  });

  it("is a silent no-op returning null on a non-ok response (never throws)", async () => {
    const fetchMock = vi
      .fn()
      .mockResolvedValue(jsonResponse(403, { type: "/errors/forbidden", status: 403 }));
    vi.stubGlobal("fetch", fetchMock);

    await expect(lookupRosterMemory("abc", "N1CCK")).resolves.toBeNull();
  });

  it("is a silent no-op returning null when the fetch itself rejects (never throws)", async () => {
    const fetchMock = vi.fn().mockRejectedValue(new Error("network down"));
    vi.stubGlobal("fetch", fetchMock);

    await expect(lookupRosterMemory("abc", "N1CCK")).resolves.toBeNull();
  });
});

describe("sessionApi.lookupCheckInAutofill", () => {
  it("GETs the merged autofill for the URL-encoded callsign and returns it", async () => {
    const fetchMock = vi
      .fn()
      .mockResolvedValue(jsonResponse(200, { name: "Fred", location: "Scottsdale, AZ" }));
    vi.stubGlobal("fetch", fetchMock);

    const result = await lookupCheckInAutofill("abc", "AA7BQ");

    expect(result).toEqual({ name: "Fred", location: "Scottsdale, AZ" });
    const [url, init] = fetchMock.mock.calls[0] as [string, RequestInit];
    expect(url).toBe("/api/net-sessions/abc/check-in-autofill?callsign=AA7BQ");
    expect(init.credentials).toBe("same-origin");
  });

  it("returns the empty shape on a total miss (both null)", async () => {
    const fetchMock = vi
      .fn()
      .mockResolvedValue(jsonResponse(200, { name: null, location: null }));
    vi.stubGlobal("fetch", fetchMock);

    const result = await lookupCheckInAutofill("abc", "K9NEVER");
    expect(result).toEqual({ name: null, location: null });
  });

  it("is a silent no-op returning null on a non-ok response (never throws)", async () => {
    const fetchMock = vi
      .fn()
      .mockResolvedValue(jsonResponse(403, { type: "/errors/forbidden", status: 403 }));
    vi.stubGlobal("fetch", fetchMock);

    await expect(lookupCheckInAutofill("abc", "AA7BQ")).resolves.toBeNull();
  });

  it("is a silent no-op returning null when the fetch itself rejects (never throws)", async () => {
    const fetchMock = vi.fn().mockRejectedValue(new Error("network down"));
    vi.stubGlobal("fetch", fetchMock);

    await expect(lookupCheckInAutofill("abc", "AA7BQ")).resolves.toBeNull();
  });

  it("includes the clientEventId in the body when the optimistic quick-add supplies one", async () => {
    const fetchMock = vi.fn().mockResolvedValue(jsonResponse(201, summaryBody));
    vi.stubGlobal("fetch", fetchMock);

    const clientEventId = "00000000-0000-0000-0000-0000000000c1";
    await addCheckIn("abc", { callsign: "N1CCK", clientEventId });

    const [, init] = fetchMock.mock.calls[0] as [string, RequestInit];
    expect(JSON.parse(init.body as string)).toEqual({
      callsign: "N1CCK",
      clientEventId,
    });
  });

  it("omits clientEventId from the body when the non-optimistic caller passes none", async () => {
    const fetchMock = vi.fn().mockResolvedValue(jsonResponse(201, summaryBody));
    vi.stubGlobal("fetch", fetchMock);

    await addCheckIn("abc", { callsign: "N1CCK" });

    const [, init] = fetchMock.mock.calls[0] as [string, RequestInit];
    const parsed = JSON.parse(init.body as string) as Record<string, unknown>;
    expect(parsed).toEqual({ callsign: "N1CCK" });
    expect("clientEventId" in parsed).toBe(false);
  });
});

const publicSummaryBody = {
  id: "00000000-0000-0000-0000-000000000001",
  lifecycle: "live",
  connections: [hfWay(14_250_000)],
  startedAt: "2026-07-16T00:00:00Z",
  closedAt: null,
  durationSeconds: null,
  latestSeq: 1,
  participantCount: 0,
  roster: [],
  definition: {
    title: "Sunday Traffic Net",
    connections: [hfWay()],
    netCategory: "traffic",
    netType: "open",
  },
};

describe("sessionApi.updateCheckIn", () => {
  it("includes precedence and traffic in the PUT body when supplied", async () => {
    const fetchMock = vi.fn().mockResolvedValue(jsonResponse(200, summaryBody));
    vi.stubGlobal("fetch", fetchMock);

    await updateCheckIn("abc", "ci-1", {
      callsign: "N1CCK",
      precedence: "emergency",
      traffic: 3,
      expectedVersion: 1,
    });

    const [url, init] = fetchMock.mock.calls[0] as [string, RequestInit];
    expect(url).toBe("/api/net-sessions/abc/check-ins/ci-1");
    expect(init.method).toBe("PUT");
    const body = JSON.parse(init.body as string);
    expect(body.precedence).toBe("emergency");
    expect(body.traffic).toBe(3);
    expect(body.expectedVersion).toBe(1);
  });

  it("includes notes in the PUT body when supplied", async () => {
    const fetchMock = vi.fn().mockResolvedValue(jsonResponse(200, summaryBody));
    vi.stubGlobal("fetch", fetchMock);

    await updateCheckIn("abc", "ci-1", {
      callsign: "N1CCK",
      notes: "handled 2 pieces",
      expectedVersion: 1,
    });

    const [, init] = fetchMock.mock.calls[0] as [string, RequestInit];
    expect(JSON.parse(init.body as string).notes).toBe("handled 2 pieces");
  });

  it("sends an explicitly cleared report/traffic as null, not as an absent key", async () => {
    // The server now reads an ABSENT key as "keep the stored value" so a partial
    // body from the public surface stops wiping fields. A deliberate clear must
    // therefore be SENT: `undefined` drops out of JSON.stringify and would now
    // be read as "leave it alone", silently turning a clear into a no-op.
    const fetchMock = vi.fn().mockResolvedValue(jsonResponse(200, summaryBody));
    vi.stubGlobal("fetch", fetchMock);

    await updateCheckIn("abc", "ci-1", {
      callsign: "N1CCK",
      signalReport: null,
      traffic: null,
      expectedVersion: 1,
    });

    const [, init] = fetchMock.mock.calls[0] as [string, RequestInit];
    const body = JSON.parse(init.body as string) as Record<string, unknown>;
    expect(Object.hasOwn(body, "signalReport")).toBe(true);
    expect(body.signalReport).toBeNull();
    expect(Object.hasOwn(body, "traffic")).toBe(true);
    expect(body.traffic).toBeNull();
  });

  it("omits a field the caller left undefined, so a partial body stays partial", async () => {
    // The mirror of the above: the public staying toggle passes no report or
    // traffic at all, and those keys must NOT appear on the wire — their absence
    // is what tells the server to preserve what staff logged.
    const fetchMock = vi.fn().mockResolvedValue(jsonResponse(200, summaryBody));
    vi.stubGlobal("fetch", fetchMock);

    await updateCheckIn("abc", "ci-1", {
      callsign: "N1CCK",
      staying: "in-and-out",
      expectedVersion: 1,
    });

    const body = JSON.parse(
      (fetchMock.mock.calls[0] as [string, RequestInit])[1].body as string,
    ) as Record<string, unknown>;
    for (const field of ["name", "location", "grid", "signalReport", "precedence", "traffic", "notes"]) {
      expect(Object.hasOwn(body, field)).toBe(false);
    }
    expect(body.staying).toBe("in-and-out");
  });
});

describe("sessionApi.setWorkedStation", () => {
  it("POSTs the checkInId to the worked-station endpoint and returns the summary", async () => {
    const fetchMock = vi.fn().mockResolvedValue(jsonResponse(200, summaryBody));
    vi.stubGlobal("fetch", fetchMock);

    const result = await setWorkedStation("abc", "ci-1");

    expect(result.id).toBe(summaryBody.id);
    const [url, init] = fetchMock.mock.calls[0] as [string, RequestInit];
    expect(url).toBe("/api/net-sessions/abc/worked-station");
    expect(init.method).toBe("POST");
    expect(init.credentials).toBe("same-origin");
    expect(JSON.parse(init.body as string)).toEqual({ checkInId: "ci-1" });
  });

  it("sends a null checkInId to clear the cursor (toggle-off)", async () => {
    const fetchMock = vi.fn().mockResolvedValue(jsonResponse(200, summaryBody));
    vi.stubGlobal("fetch", fetchMock);

    await setWorkedStation("abc", null);

    const [, init] = fetchMock.mock.calls[0] as [string, RequestInit];
    expect(JSON.parse(init.body as string)).toEqual({ checkInId: null });
  });

  it("throws ProblemError on a 403 (non-NCS)", async () => {
    const fetchMock = vi
      .fn()
      .mockResolvedValue(jsonResponse(403, { type: "/errors/forbidden" }));
    vi.stubGlobal("fetch", fetchMock);

    await expect(setWorkedStation("abc", "ci-1")).rejects.toBeInstanceOf(ProblemError);
  });
});

describe("sessionApi.setNetNote", () => {
  it("PUTs the note to the net-note endpoint and returns the summary", async () => {
    const fetchMock = vi.fn().mockResolvedValue(jsonResponse(200, summaryBody));
    vi.stubGlobal("fetch", fetchMock);

    await setNetNote("abc", "Weekly traffic net");

    const [url, init] = fetchMock.mock.calls[0] as [string, RequestInit];
    expect(url).toBe("/api/net-sessions/abc/net-note");
    expect(init.method).toBe("PUT");
    expect(JSON.parse(init.body as string)).toEqual({ note: "Weekly traffic net" });
  });

  it("throws ProblemError on a 400 note-invalid", async () => {
    const fetchMock = vi
      .fn()
      .mockResolvedValue(jsonResponse(400, { type: "/errors/note-invalid" }));
    vi.stubGlobal("fetch", fetchMock);

    await expect(setNetNote("abc", "x")).rejects.toBeInstanceOf(ProblemError);
  });
});

describe("sessionApi.reorderRoster", () => {
  it("POSTs the precedence strategy to the reorder endpoint and returns the summary", async () => {
    const fetchMock = vi.fn().mockResolvedValue(jsonResponse(200, summaryBody));
    vi.stubGlobal("fetch", fetchMock);

    const result = await reorderRoster("abc");

    expect(result.id).toBe(summaryBody.id);
    const [url, init] = fetchMock.mock.calls[0] as [string, RequestInit];
    expect(url).toBe("/api/net-sessions/abc/reorder");
    expect(init.method).toBe("POST");
    expect(init.credentials).toBe("same-origin");
    expect(JSON.parse(init.body as string)).toEqual({ by: "precedence" });
  });

  it("throws ProblemError on a 403 (non-NCS)", async () => {
    const fetchMock = vi
      .fn()
      .mockResolvedValue(jsonResponse(403, { type: "/errors/forbidden" }));
    vi.stubGlobal("fetch", fetchMock);

    await expect(reorderRoster("abc")).rejects.toBeInstanceOf(ProblemError);
  });
});

describe("sessionApi.getPublicSession", () => {
  it("GETs the redacted public view and parses it (account-less)", async () => {
    const fetchMock = vi.fn().mockResolvedValue(jsonResponse(200, publicSummaryBody));
    vi.stubGlobal("fetch", fetchMock);

    const result = await getPublicSession("abc");

    expect(result?.latestSeq).toBe(1);
    const [url] = fetchMock.mock.calls[0] as [string];
    expect(url).toBe("/api/net-sessions/abc/live");
  });

  it("returns null on 404 (missing session), never redirecting to sign-in", async () => {
    const fetchMock = vi
      .fn()
      .mockResolvedValue(jsonResponse(404, { type: "/errors/net-session-not-found", status: 404 }));
    vi.stubGlobal("fetch", fetchMock);

    expect(await getPublicSession("abc")).toBeNull();
  });

  it("throws ProblemError on a non-404 failure (429 rate-limited)", async () => {
    const fetchMock = vi
      .fn()
      .mockResolvedValue(jsonResponse(429, { type: "/errors/rate-limited", status: 429 }));
    vi.stubGlobal("fetch", fetchMock);

    await expect(getPublicSession("abc")).rejects.toBeInstanceOf(ProblemError);
  });
});

describe("sessionApi.getPublicEventsSince", () => {
  it("GETs the public catch-up gap with a ?since= cursor", async () => {
    const fetchMock = vi.fn().mockResolvedValue(jsonResponse(200, []));
    vi.stubGlobal("fetch", fetchMock);

    const result = await getPublicEventsSince("abc", 1);

    expect(result).toEqual([]);
    const [url] = fetchMock.mock.calls[0] as [string];
    expect(url).toBe("/api/net-sessions/abc/live/events?since=1");
  });
});

describe("sessionApi.releaseLock", () => {
  it("DELETEs with keepalive so a pagehide-triggered release survives unload", async () => {
    const fetchMock = vi.fn().mockResolvedValue(new Response(null, { status: 204 }));
    vi.stubGlobal("fetch", fetchMock);

    await releaseLock("abc", "def");

    const [url, init] = fetchMock.mock.calls[0] as [string, RequestInit];
    expect(url).toBe("/api/net-sessions/abc/check-ins/def/lock");
    expect(init.method).toBe("DELETE");
    expect(init.keepalive).toBe(true);
  });
});

describe("sessionApi role management", () => {
  it("listRoles GETs the grants same-origin and parses them", async () => {
    const grants = [
      { accountId: "a1", callsign: "W1AW", role: "relay", grantedBy: "o1", grantedAt: "2026-07-17T00:00:00Z" },
    ];
    const fetchMock = vi.fn().mockResolvedValue(jsonResponse(200, grants));
    vi.stubGlobal("fetch", fetchMock);

    const result = await listRoles("abc");

    expect(result).toEqual(grants);
    const [url, init] = fetchMock.mock.calls[0] as [string, RequestInit];
    expect(url).toBe("/api/net-sessions/abc/roles");
    expect(init.credentials).toBe("same-origin");
  });

  it("grantRole POSTs the callsign and role and returns the grant", async () => {
    const fetchMock = vi
      .fn()
      .mockResolvedValue(jsonResponse(200, { accountId: "a1", role: "relay" }));
    vi.stubGlobal("fetch", fetchMock);

    const result = await grantRole("abc", { callsign: "w1aw", role: "relay" });

    expect(result).toEqual({ accountId: "a1", role: "relay" });
    const [url, init] = fetchMock.mock.calls[0] as [string, RequestInit];
    expect(url).toBe("/api/net-sessions/abc/roles");
    expect(init.method).toBe("POST");
    expect(JSON.parse(init.body as string)).toEqual({ callsign: "w1aw", role: "relay" });
  });

  it("grantRole throws ProblemError on an owner-not-found 404", async () => {
    const fetchMock = vi
      .fn()
      .mockResolvedValue(jsonResponse(404, { type: "/errors/owner-not-found", status: 404 }));
    vi.stubGlobal("fetch", fetchMock);

    await expect(grantRole("abc", { callsign: "zz9zzz", role: "relay" })).rejects.toBeInstanceOf(
      ProblemError,
    );
  });

  it("revokeRole DELETEs the grant by accountId", async () => {
    const fetchMock = vi.fn().mockResolvedValue(new Response(null, { status: 204 }));
    vi.stubGlobal("fetch", fetchMock);

    await revokeRole("abc", "a1");

    const [url, init] = fetchMock.mock.calls[0] as [string, RequestInit];
    expect(url).toBe("/api/net-sessions/abc/roles/a1");
    expect(init.method).toBe("DELETE");
  });

  it("revokeRole throws ProblemError on a role-grant-not-found 404", async () => {
    const fetchMock = vi
      .fn()
      .mockResolvedValue(jsonResponse(404, { type: "/errors/role-grant-not-found", status: 404 }));
    vi.stubGlobal("fetch", fetchMock);

    await expect(revokeRole("abc", "missing")).rejects.toBeInstanceOf(ProblemError);
  });
});

describe("exportUrl", () => {
  it("builds a same-origin export URL for each format", () => {
    // A plain `<a download href>` navigation carries the session cookie, so the
    // builder only needs to compose the same-origin path + `?format=`.
    expect(exportUrl("abc", "csv")).toBe(
      "/api/net-sessions/abc/export?format=csv",
    );
    expect(exportUrl("abc", "adif")).toBe(
      "/api/net-sessions/abc/export?format=adif",
    );
  });
});
