// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
import { afterEach, describe, expect, it, vi } from "vitest";

import { ProblemError } from "../auth/authApi";
import {
  filtersFromSearchParams,
  getDiscovery,
  type DiscoveryResponse,
} from "./discoveryApi";

function jsonResponse(status: number, body: unknown): Response {
  return new Response(JSON.stringify(body), {
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

const emptyResponse: DiscoveryResponse = {
  activeNow: [],
  upcoming: [],
  applied: { sort: "time" },
};

describe("getDiscovery", () => {
  it("GETs /api/discovery with no query when no filters are set", async () => {
    const mock = stubFetch(jsonResponse(200, emptyResponse));
    const result = await getDiscovery();

    const url = mock.mock.calls[0][0] as string;
    expect(url).toBe("/api/discovery");
    expect(result).toEqual(emptyResponse);
  });

  it("encodes only the set filters as camelCase query params", async () => {
    const mock = stubFetch(jsonResponse(200, emptyResponse));
    await getDiscovery({
      q: "sunday",
      band: "20m",
      type: "roll-call",
      sort: "name",
      mode: "", // blank filters are omitted, not sent as empty params
    });

    const url = new URL(
      mock.mock.calls[0][0] as string,
      "http://localhost",
    );
    expect(url.pathname).toBe("/api/discovery");
    expect(url.searchParams.get("q")).toBe("sunday");
    expect(url.searchParams.get("band")).toBe("20m");
    expect(url.searchParams.get("type")).toBe("roll-call");
    expect(url.searchParams.get("sort")).toBe("name");
    expect(url.searchParams.has("mode")).toBe(false);
    expect(url.searchParams.has("country")).toBe(false);
  });

  it("sends a set connection-kind filter and omits a blank one", async () => {
    // A fresh Response per call: a body reads once, and this test fetches twice.
    const mock = vi.fn((_url: string) => jsonResponse(200, emptyResponse));
    vi.stubGlobal("fetch", mock);
    await getDiscovery({ kind: "echolink" });
    const url = new URL(mock.mock.calls[0][0], "http://localhost");
    expect(url.searchParams.get("kind")).toBe("echolink");

    await getDiscovery({ kind: "   " });
    const blank = new URL(mock.mock.calls[1][0], "http://localhost");
    expect(blank.searchParams.has("kind")).toBe(false);
  });

  it("reads a kind back out of a URL verbatim, including one the page does not offer", async () => {
    // `other` is a value the server refuses as a filter (a 400); that answer is
    // the server's to give, so the arrival parser must carry it through rather
    // than scrub it. Pinned here so the round trip reaches the server.
    expect(filtersFromSearchParams(new URLSearchParams("?kind=echolink"))).toEqual({
      kind: "echolink",
    });
    expect(filtersFromSearchParams(new URLSearchParams("?kind=other"))).toEqual({
      kind: "other",
    });

    stubFetch(
      jsonResponse(400, {
        type: "/errors/discovery-query-invalid",
        status: 400,
        detail: "kind: is not a recognized connection kind",
      }),
    );
    await expect(
      getDiscovery(filtersFromSearchParams(new URLSearchParams("?kind=other"))),
    ).rejects.toMatchObject({
      problem: { type: "/errors/discovery-query-invalid" },
    });
  });

  it("throws ProblemError carrying the slug on a problem response", async () => {
    stubFetch(
      jsonResponse(400, {
        type: "/errors/discovery-query-invalid",
        status: 400,
        detail: "band: is not a recognized band",
      }),
    );

    await expect(getDiscovery({ band: "nonsense" })).rejects.toMatchObject({
      problem: { type: "/errors/discovery-query-invalid" },
    });
    await expect(getDiscovery({ band: "nonsense" })).rejects.toBeInstanceOf(
      ProblemError,
    );
  });

  it("returns the parsed activeNow/upcoming collections", async () => {
    const body: DiscoveryResponse = {
      activeNow: [],
      applied: { sort: "time" },
      upcoming: [
        {
          id: "def-1",
          definitionVersion: 1,
          occurrenceId: "occ-1",
          scheduledStartAt: "2027-01-01T20:00:00+00:00",
          title: "Sunday Traffic Net",
          description: null,
          country: null,
          state: null,
          grid: null,
          netCategory: "traffic",
          netType: "open",
          expectedDurationMinutes: null,
          linkToken: "tok-1",
          connections: [
            {
              id: "conn-1",
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
          matchedConnectionId: null,
        },
      ],
    };
    stubFetch(jsonResponse(200, body));
    const result = await getDiscovery();
    expect(result.activeNow).toHaveLength(0);
    expect(result.upcoming[0].occurrenceId).toBe("occ-1");
    // The connection set crosses the wire on the discovery card,
    // typed as the SAME `NetConnection` the connection presenters take — this
    // literal type-checks only against that shape.
    expect(result.upcoming[0].connections[0].kind).toBe("hf");
    expect(result.upcoming[0].matchedConnectionId).toBeNull();
  });

  it("carries `applied.truncated` through verbatim", async () => {
    // The wire names of the cut collections, in envelope order, untouched.
    stubFetch(
      jsonResponse(200, {
        ...emptyResponse,
        applied: { sort: "time", truncated: ["activeNow", "upcoming"] },
      }),
    );
    const result = await getDiscovery();
    expect(result.applied.truncated).toEqual(["activeNow", "upcoming"]);
  });

  it("leaves `truncated` undefined when the key is absent — absence is the statement that nothing was cut", async () => {
    // Deliberately NO `?? []` and no throw here, unlike `searchObjects`: on this
    // endpoint the key is omitted when empty, so a missing key is meaningful.
    // The fence against a backend that stops sending it is backend-side.
    stubFetch(jsonResponse(200, emptyResponse));
    const result = await getDiscovery();
    expect(result.applied.truncated).toBeUndefined();
  });
});
