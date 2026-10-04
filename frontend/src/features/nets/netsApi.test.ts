// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
import { afterEach, describe, expect, it, vi } from "vitest";

import {
  addNetOwner,
  clearDeliveryConfig,
  clearNetSchedule,
  createNetDefinition,
  deleteNetDefinition,
  getDeliveryConfig,
  getNetByToken,
  getNetDefinition,
  getNetOccurrences,
  getOwnedNets,
  formatFrequencyMhz,
  hzToMhz,
  mhzToHz,
  parseMhz,
  removeNetOwner,
  setDeliveryConfig,
  setNetSchedule,
  updateNetDefinition,
  type NetDefinition,
  type OwnedNet,
  type PublicNetView,
  type Schedule,
} from "./netsApi";

function jsonResponse(status: number, body: unknown): Response {
  return new Response(status === 204 ? null : JSON.stringify(body), {
    status,
    headers: {
      "content-type":
        status >= 400 ? "application/problem+json" : "application/json",
    },
  });
}

function stubFetch(response: Response): ReturnType<typeof vi.fn> {
  const mock = vi.fn().mockResolvedValue(response);
  vi.stubGlobal("fetch", mock);
  return mock;
}

afterEach(() => {
  vi.unstubAllGlobals();
});

const definition: NetDefinition = {
  id: "0192-abc",
  definitionVersion: 1,
  title: "Sunday Traffic Net",
  description: null,
  country: null,
  state: null,
  grid: "FN31pr",
  connections: [
    {
      id: "0192-conn-0",
      position: 0,
      kind: "hf",
      plannedFrequencyHz: 14_230_000,
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
    },
  ],
  netCategory: "traffic",
  netType: "open",
  expectedDurationMinutes: null,
  visibility: "listed",
  linkToken: "tok-fixture",
  ownerAccountIds: ["me"],
  owners: [{ accountId: "me", callsign: "W1AW" }],
  archivedAt: null,
  createdAt: "2026-07-16T00:00:00Z",
  updatedAt: "2026-07-16T00:00:00Z",
};

describe("mhzToHz / hzToMhz", () => {
  it("parses decimal MHz to exact integer Hz without float drift", () => {
    expect(mhzToHz("14.230")).toBe(14_230_000);
    expect(mhzToHz("146.520")).toBe(146_520_000);
    expect(mhzToHz("14.074100")).toBe(14_074_100);
    expect(mhzToHz("50")).toBe(50_000_000);
  });

  it("returns null for a non-numeric or empty frequency", () => {
    expect(mhzToHz("")).toBeNull();
    expect(mhzToHz("abc")).toBeNull();
    expect(mhzToHz("14.2.3")).toBeNull();
    expect(mhzToHz("-")).toBeNull();
  });

  it("parses a signed decimal MHz offset to a negative integer Hz", () => {
    expect(mhzToHz("-0.6")).toBe(-600_000);
    expect(mhzToHz("-5")).toBe(-5_000_000);
  });

  it("returns null for a value that is a number but not a safe exact integer of Hz", () => {
    // Thirteen MHz digits is ~1e19 Hz: past 2^53 AND past i64::MAX, so it is
    // neither exactly representable here nor deserializable as an integer on
    // the server. A finite non-null here would pass the row gate and come back
    // as a page-scoped problem with no row index.
    expect(mhzToHz("10000000000000")).toBeNull();
    expect(mhzToHz("-10000000000000")).toBeNull();
    // Enough digits that `Number(whole)` is Infinity. Infinity is not null, and
    // `JSON.stringify` turns it into `null` on the wire.
    const overflowing = `1${"0".repeat(310)}`;
    expect(mhzToHz(overflowing)).toBeNull();
    expect(mhzToHz(`-${overflowing}`)).toBeNull();
    expect(mhzToHz(`${overflowing}.5`)).toBeNull();
  });

  it("names which of the three faults stopped a conversion", () => {
    // The server used to distinguish `NotNumeric` from `TooPrecise`; the
    // client inherited the faults and must keep them apart,
    // plus the one the wire itself adds.
    expect(parseMhz("abc").fault).toBe("not-a-number");
    expect(parseMhz("14,230").fault).toBe("not-a-number");
    expect(parseMhz("14.2.3").fault).toBe("not-a-number");
    expect(parseMhz("").fault).toBe("not-a-number");
    expect(parseMhz("14.2301234").fault).toBe("too-precise");
    expect(parseMhz("-0.6000001").fault).toBe("too-precise");
    expect(parseMhz("10000000000000").fault).toBe("too-large");
    expect(parseMhz(`1${"0".repeat(310)}`).fault).toBe("too-large");
    expect(parseMhz("14.230")).toEqual({ hz: 14_230_000, fault: null });
    // A value that is both too precise and too large is reported for the
    // digits the owner can see first.
    expect(parseMhz("10000000000000.1234567").fault).toBe("too-precise");
  });

  it("still converts the largest frequency the server can hold", () => {
    // 300 GHz is the domain ceiling; the safe-integer bound must not undercut
    // a value the server would have accepted.
    expect(mhzToHz("300000")).toBe(300_000_000_000);
    expect(mhzToHz("9007199254.740991")).toBe(Number.MAX_SAFE_INTEGER);
  });

  it("formats integer Hz back to a MHz string that round-trips", () => {
    expect(hzToMhz(14_230_000)).toBe("14.23");
    expect(hzToMhz(146_520_000)).toBe("146.52");
    expect(hzToMhz(50_000_000)).toBe("50");
    for (const hz of [14_230_000, 14_074_100, 135_700]) {
      expect(mhzToHz(hzToMhz(hz))).toBe(hz);
    }
  });

  it("preserves sign for a negative offset under 1 MHz in magnitude", () => {
    // -0.600 MHz is the standard 2m repeater offset — a whole-MHz part of
    // -0 must not stringify away the sign (JS `${-0}` === "0").
    expect(hzToMhz(-600_000)).toBe("-0.6");
    expect(hzToMhz(-100_000)).toBe("-0.1");
    expect(hzToMhz(-5_000_000)).toBe("-5");
    expect(hzToMhz(0)).toBe("0");
    for (const hz of [-600_000, -100_000, -5_000_000, -14_074_100]) {
      expect(mhzToHz(hzToMhz(hz))).toBe(hz);
    }
  });
});

describe("formatFrequencyMhz", () => {
  it("always pads to the three-digit kHz group operators read on the air", () => {
    // `hzToMhz` minimizes for round-tripping ("14.23"), which is not how a
    // frequency is ever written or spoken — display always shows X.XXX.
    expect(formatFrequencyMhz(7_200_000)).toBe("7.200");
    expect(formatFrequencyMhz(14_275_000)).toBe("14.275");
    expect(formatFrequencyMhz(146_520_000)).toBe("146.520");
    expect(formatFrequencyMhz(50_000_000)).toBe("50.000");
  });

  it("adds the four-digit sub-kHz group only when there is precision below 1 kHz", () => {
    // 448.670 MHz + 125 Hz, written as tenths of a Hz in the trailing group.
    expect(formatFrequencyMhz(448_670_125)).toBe("448.670.1250");
    expect(formatFrequencyMhz(14_074_100)).toBe("14.074.1000");
    // Exactly on a kHz boundary: no trailing group at all.
    expect(formatFrequencyMhz(14_074_000)).toBe("14.074");
  });

  it("keeps the sign on a sub-MHz repeater offset", () => {
    // The standard 2m offset. A whole-MHz part of -0 must not stringify the
    // sign away (JS `${-0}` === "0") — the offset's direction is the point.
    expect(formatFrequencyMhz(-600_000)).toBe("-0.600");
    expect(formatFrequencyMhz(-5_000_000)).toBe("-5.000");
    expect(formatFrequencyMhz(0)).toBe("0.000");
  });

  it("stays distinct from the transport format, which must still round-trip", () => {
    // The display format has two dots, which `mhzToHz` rejects by design —
    // so the API/form value keeps coming from `hzToMhz`, not from this.
    expect(mhzToHz(formatFrequencyMhz(448_670_125))).toBeNull();
    expect(mhzToHz(hzToMhz(448_670_125))).toBe(448_670_125);
  });
});

describe("netsApi CRUD", () => {
  it("createNetDefinition POSTs same-origin and returns the parsed definition", async () => {
    const mock = stubFetch(jsonResponse(201, definition));

    const result = await createNetDefinition({
      title: "Sunday Traffic Net",
      netCategory: "traffic",
      netType: "open",
      connections: [
        { kind: "hf", plannedFrequencyHz: 14_230_000, band: "20m", mode: "ssb" },
      ],
    });

    expect(result).toEqual(definition);
    const [url, init] = mock.mock.calls[0] as [string, RequestInit];
    expect(url).toBe("/api/net-definitions");
    expect(init.method).toBe("POST");
    expect(init.credentials).toBe("same-origin");
    expect(JSON.parse(init.body as string)).toMatchObject({
      title: "Sunday Traffic Net",
      connections: [{ kind: "hf", plannedFrequencyHz: 14_230_000, band: "20m" }],
    });
  });

  it("getNetDefinition GETs by id and returns the parsed definition", async () => {
    const mock = stubFetch(jsonResponse(200, definition));

    const result = await getNetDefinition("0192-abc");

    expect(result).toEqual(definition);
    const [url, init] = mock.mock.calls[0] as [string, RequestInit | undefined];
    expect(url).toBe("/api/net-definitions/0192-abc");
    expect(init?.method ?? "GET").toBe("GET");
  });

  it("updateNetDefinition PUTs the fields to the id", async () => {
    const mock = stubFetch(
      jsonResponse(200, { ...definition, definitionVersion: 2 }),
    );

    const result = await updateNetDefinition("0192-abc", { title: "Edited" });

    expect(result.definitionVersion).toBe(2);
    const [url, init] = mock.mock.calls[0] as [string, RequestInit];
    expect(url).toBe("/api/net-definitions/0192-abc");
    expect(init.method).toBe("PUT");
    expect(JSON.parse(init.body as string)).toEqual({ title: "Edited" });
  });

  it("deleteNetDefinition DELETEs by id", async () => {
    const mock = stubFetch(jsonResponse(204, null));

    await deleteNetDefinition("0192-abc");

    const [url, init] = mock.mock.calls[0] as [string, RequestInit];
    expect(url).toBe("/api/net-definitions/0192-abc");
    expect(init.method).toBe("DELETE");
  });

  it("surfaces a field-level validation problem as a ProblemError carrying the slug", async () => {
    stubFetch(
      jsonResponse(400, {
        type: "/errors/net-definition-invalid",
        status: 400,
        detail: "band: is not a recognized band",
      }),
    );

    await expect(
      createNetDefinition({
        title: "x",
        connections: [{ kind: "hf", plannedFrequencyHz: 14_230_000, band: "21m" }],
      }),
    ).rejects.toMatchObject({
      problem: {
        type: "/errors/net-definition-invalid",
        detail: "band: is not a recognized band",
      },
    });
  });

  it("surfaces a 403 forbidden on edit as a ProblemError", async () => {
    stubFetch(jsonResponse(403, { type: "/errors/forbidden", status: 403 }));

    await expect(
      updateNetDefinition("0192-abc", { title: "hijack" }),
    ).rejects.toMatchObject({ problem: { type: "/errors/forbidden" } });
  });
});

describe("owner management", () => {
  it("addNetOwner POSTs the callsign and returns the updated definition", async () => {
    const updated: NetDefinition = {
      ...definition,
      ownerAccountIds: ["me", "other"],
      owners: [
        { accountId: "me", callsign: "W1AW" },
        { accountId: "other", callsign: "K2XYZ" },
      ],
    };
    const mock = stubFetch(jsonResponse(200, updated));

    const result = await addNetOwner("0192-abc", "K2XYZ");

    expect(result).toEqual(updated);
    const [url, init] = mock.mock.calls[0] as [string, RequestInit];
    expect(url).toBe("/api/net-definitions/0192-abc/owners");
    expect(init.method).toBe("POST");
    expect(init.credentials).toBe("same-origin");
    expect(JSON.parse(init.body as string)).toEqual({ callsign: "K2XYZ" });
  });

  it("addNetOwner throws ProblemError on an unknown callsign", async () => {
    stubFetch(
      jsonResponse(404, { type: "/errors/owner-not-found", status: 404 }),
    );

    await expect(addNetOwner("0192-abc", "N0BODY")).rejects.toMatchObject({
      problem: { type: "/errors/owner-not-found" },
    });
  });

  it("removeNetOwner DELETEs by accountId and resolves on 204", async () => {
    const mock = stubFetch(jsonResponse(204, null));

    await removeNetOwner("0192-abc", "other");

    const [url, init] = mock.mock.calls[0] as [string, RequestInit];
    expect(url).toBe("/api/net-definitions/0192-abc/owners/other");
    expect(init.method).toBe("DELETE");
    expect(init.credentials).toBe("same-origin");
  });

  it("removeNetOwner throws ProblemError on a last-owner refusal", async () => {
    stubFetch(jsonResponse(409, { type: "/errors/last-owner", status: 409 }));

    await expect(removeNetOwner("0192-abc", "me")).rejects.toMatchObject({
      problem: { type: "/errors/last-owner" },
    });
  });
});

describe("getNetByToken (public read)", () => {
  const publicView: PublicNetView = {
    id: "0192-abc",
    title: "Sunday Traffic Net",
    description: null,
    country: null,
    state: null,
    grid: "FN31pr",
    connections: [
      {
        id: "0192-conn-0",
        position: 0,
        kind: "hf",
        plannedFrequencyHz: 14_230_000,
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
      },
    ],
    netCategory: "traffic",
    netType: "open",
    expectedDurationMinutes: null,
    visibility: "unlisted",
    createdAt: "2026-07-16T00:00:00Z",
    updatedAt: "2026-07-16T00:00:00Z",
    archivedAt: null,
  };

  it("GETs the by-token path and returns the parsed public view", async () => {
    const mock = stubFetch(jsonResponse(200, publicView));

    const result = await getNetByToken("the-token");

    expect(result).toEqual(publicView);
    const [url] = mock.mock.calls[0] as [string, RequestInit | undefined];
    expect(url).toBe("/api/net-definitions/by-token/the-token");
  });

  it("rejects a 404 as a ProblemError carrying the not-found slug", async () => {
    stubFetch(
      jsonResponse(404, {
        type: "/errors/net-definition-not-found",
        status: 404,
      }),
    );

    await expect(getNetByToken("wrong")).rejects.toMatchObject({
      problem: { type: "/errors/net-definition-not-found" },
    });
  });
});

describe("getOwnedNets (My Nets — Owned tab)", () => {
  const owned: OwnedNet = {
    ...definition,
    liveSessionId: null,
    nextOccurrenceAt: null,
  };

  it("GETs /api/net-definitions same-origin and returns the page — items AND nextCursor", async () => {
    const mock = stubFetch(
      jsonResponse(200, { items: [owned], nextCursor: "1754000000123:abc" }),
    );

    const result = await getOwnedNets();

    const [url, init] = mock.mock.calls[0] as [string, RequestInit | undefined];
    expect(url).toBe("/api/net-definitions");
    expect(init?.credentials).toBe("same-origin");
    expect(result.items).toHaveLength(1);
    expect(result.items[0].id).toBe("0192-abc");
    expect(result.items[0].liveSessionId).toBeNull();
    expect(result.nextCursor).toBe("1754000000123:abc");
  });

  it("sends the server's cursor back as ?cursor= when asked for the next page", async () => {
    const mock = stubFetch(jsonResponse(200, { items: [], nextCursor: null }));
    await getOwnedNets("1754000000123:abc");

    const [url] = mock.mock.calls[0] as [string, RequestInit | undefined];
    expect(url).toBe("/api/net-definitions?cursor=1754000000123%3Aabc");
  });

  it("passes through a live session id and next-occurrence pointer when present", async () => {
    stubFetch(
      jsonResponse(200, {
        items: [{ ...owned, liveSessionId: "sess-1", nextOccurrenceAt: "2027-01-08T20:00:00+00:00" }],
        nextCursor: null,
      }),
    );

    const { items: [result] } = await getOwnedNets();
    expect(result.liveSessionId).toBe("sess-1");
    expect(result.nextOccurrenceAt).toBe("2027-01-08T20:00:00+00:00");
  });

  it("returns an empty last page when the account owns no active nets", async () => {
    stubFetch(jsonResponse(200, { items: [], nextCursor: null }));
    await expect(getOwnedNets()).resolves.toEqual({ items: [], nextCursor: null });
  });

  it("rejects a 200 that carries no nextCursor — a contract break is not smoothed to 'last page'", async () => {
    stubFetch(jsonResponse(200, { items: [owned] }));
    await expect(getOwnedNets()).rejects.toThrow();
  });

  it("rejects a 200 whose items is missing or not an array — the break surfaces here, not inside render", async () => {
    // `rows: undefined` would reach `rows.map(...)` in the tab's render, outside
    // every catch, and unmount the page.
    stubFetch(jsonResponse(200, { nextCursor: null }));
    await expect(getOwnedNets()).rejects.toThrow();

    stubFetch(jsonResponse(200, { items: owned, nextCursor: null }));
    await expect(getOwnedNets()).rejects.toThrow();
  });

  it("throws ProblemError on a problem response", async () => {
    stubFetch(jsonResponse(401, { type: "/errors/unauthenticated", status: 401 }));
    await expect(getOwnedNets()).rejects.toMatchObject({
      problem: { type: "/errors/unauthenticated" },
    });
  });
});

describe("net schedule", () => {
  const occurrences = {
    occurrences: [
      { id: "occ-1", scheduledStartAt: "2027-01-01T20:00:00+00:00" },
      { id: "occ-2", scheduledStartAt: "2027-01-08T20:00:00+00:00" },
    ],
  };

  it("setNetSchedule PUTs the camelCase schedule body and returns the occurrences", async () => {
    const mock = stubFetch(jsonResponse(200, occurrences));
    const schedule: Schedule = {
      kind: "recurring",
      timezone: "America/New_York",
      frequency: "weekly",
      timeOfDay: "20:00",
      weekday: "tuesday",
    };

    const result = await setNetSchedule("0192-abc", schedule);

    expect(result).toEqual(occurrences.occurrences);
    const [url, init] = mock.mock.calls[0] as [string, RequestInit];
    expect(url).toBe("/api/net-definitions/0192-abc/schedule");
    expect(init.method).toBe("PUT");
    expect(init.credentials).toBe("same-origin");
    expect(JSON.parse(init.body as string)).toEqual(schedule);
  });

  it("setNetSchedule surfaces a schedule-invalid problem as a ProblemError", async () => {
    stubFetch(
      jsonResponse(400, {
        type: "/errors/schedule-invalid",
        status: 400,
        detail: "weekday: is required for a weekly schedule",
      }),
    );

    await expect(
      setNetSchedule("0192-abc", {
        kind: "recurring",
        timezone: "UTC",
        frequency: "weekly",
        timeOfDay: "20:00",
      }),
    ).rejects.toMatchObject({
      problem: {
        type: "/errors/schedule-invalid",
        detail: "weekday: is required for a weekly schedule",
      },
    });
  });

  it("clearNetSchedule DELETEs the schedule sub-resource and resolves on 204", async () => {
    const mock = stubFetch(jsonResponse(204, null));

    await clearNetSchedule("0192-abc");

    const [url, init] = mock.mock.calls[0] as [string, RequestInit];
    expect(url).toBe("/api/net-definitions/0192-abc/schedule");
    expect(init.method).toBe("DELETE");
    expect(init.credentials).toBe("same-origin");
  });

  it("getNetOccurrences GETs the occurrences sub-resource and returns the list", async () => {
    const mock = stubFetch(jsonResponse(200, occurrences));

    const result = await getNetOccurrences("0192-abc");

    expect(result).toEqual(occurrences.occurrences);
    const [url, init] = mock.mock.calls[0] as [string, RequestInit | undefined];
    expect(url).toBe("/api/net-definitions/0192-abc/occurrences");
    expect(init?.method ?? "GET").toBe("GET");
  });
});

describe("delivery config", () => {
  it("setDeliveryConfig PUTs the camelCase body and surfaces a minted secret once", async () => {
    const mock = stubFetch(
      jsonResponse(200, {
        emails: ["alerts@example.com"],
        webhookUrl: "https://hooks.example.com/net",
        webhookConfigured: true,
        discordWebhookUrl: "https://discord.com/api/webhooks/12/tok",
        webhookSecretSet: true,
        webhookSecret: "reveal-me-once",
      }),
    );

    const result = await setDeliveryConfig("0192-abc", {
      emails: ["alerts@example.com"],
      webhookUrl: "https://hooks.example.com/net",
      discordWebhookUrl: "https://discord.com/api/webhooks/12/tok",
    });

    expect(result.webhookSecret).toBe("reveal-me-once");
    expect(result.webhookSecretSet).toBe(true);
    expect(result.discordWebhookUrl).toBe(
      "https://discord.com/api/webhooks/12/tok",
    );
    const [url, init] = mock.mock.calls[0] as [string, RequestInit];
    expect(url).toBe("/api/net-definitions/0192-abc/delivery-config");
    expect(init.method).toBe("PUT");
    expect(init.credentials).toBe("same-origin");
    expect(JSON.parse(init.body as string)).toEqual({
      emails: ["alerts@example.com"],
      webhookUrl: "https://hooks.example.com/net",
      discordWebhookUrl: "https://discord.com/api/webhooks/12/tok",
    });
  });

  it("getDeliveryConfig GETs the config without any plaintext secret", async () => {
    const mock = stubFetch(
      jsonResponse(200, {
        emails: [],
        webhookUrl: "https://hooks.example.com/net",
        discordWebhookUrl: "https://discord.com/api/webhooks/12/tok",
        webhookConfigured: true,
        webhookSecretSet: true,
      }),
    );

    const result = await getDeliveryConfig("0192-abc");

    expect(result.webhookSecretSet).toBe(true);
    expect(result).not.toHaveProperty("webhookSecret");
    // The Discord URL mirrors `webhookUrl`: read back in the clear, unlike the
    // HMAC secret, which never appears in a read at all.
    expect(result.discordWebhookUrl).toBe(
      "https://discord.com/api/webhooks/12/tok",
    );
    const [url, init] = mock.mock.calls[0] as [string, RequestInit | undefined];
    expect(url).toBe("/api/net-definitions/0192-abc/delivery-config");
    expect(init?.method ?? "GET").toBe("GET");
  });

  it("setDeliveryConfig surfaces a delivery-config-invalid problem as a ProblemError", async () => {
    stubFetch(
      jsonResponse(422, {
        type: "/errors/delivery-config-invalid",
        status: 422,
        detail: "webhook url: must use https",
      }),
    );

    await expect(
      setDeliveryConfig("0192-abc", {
        emails: [],
        webhookUrl: "http://hooks.example.com/net",
        discordWebhookUrl: null,
      }),
    ).rejects.toMatchObject({
      problem: {
        type: "/errors/delivery-config-invalid",
        detail: "webhook url: must use https",
      },
    });
  });

  it("clearDeliveryConfig DELETEs the sub-resource and resolves on 204", async () => {
    const mock = stubFetch(jsonResponse(204, null));

    await clearDeliveryConfig("0192-abc");

    const [url, init] = mock.mock.calls[0] as [string, RequestInit];
    expect(url).toBe("/api/net-definitions/0192-abc/delivery-config");
    expect(init.method).toBe("DELETE");
    expect(init.credentials).toBe("same-origin");
  });
});
