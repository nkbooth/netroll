// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
import { afterEach, describe, expect, it, vi } from "vitest";

import { ProblemError } from "../auth/authApi";
import {
  favoriteNet,
  fetchFavoriteMembership,
  getMyNets,
  unfavoriteNet,
  type FavoriteNet,
} from "./favoritesApi";

function jsonResponse(status: number, body: unknown): Response {
  return new Response(JSON.stringify(body), {
    status,
    headers: {
      "content-type":
        status >= 400 ? "application/problem+json" : "application/json",
    },
  });
}

function noContent(): Response {
  return new Response(null, { status: 204 });
}

function stubFetch(response: Response): ReturnType<typeof vi.fn> {
  const mock = vi.fn().mockResolvedValue(response);
  vi.stubGlobal("fetch", mock);
  return mock;
}

afterEach(() => {
  vi.unstubAllGlobals();
});

const sampleFavorite: FavoriteNet = {
  id: "def-1",
  title: "Sunday Traffic Net",
  description: null,
  connections: [
    {
      id: "conn-fav-0",
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
  country: null,
  state: null,
  grid: null,
  netCategory: "traffic",
  netType: "open",
  expectedDurationMinutes: null,
  linkToken: "tok-1",
  archivedAt: null,
  favoritedAt: "2026-07-16T00:00:00+00:00",
};

describe("favoriteNet", () => {
  it("PUTs /api/favorites/{id} same-origin and resolves on 204", async () => {
    const mock = stubFetch(noContent());
    await expect(favoriteNet("def-1")).resolves.toBeUndefined();

    const [url, init] = mock.mock.calls[0] as [string, RequestInit];
    expect(url).toBe("/api/favorites/def-1");
    expect(init.method).toBe("PUT");
    expect(init.credentials).toBe("same-origin");
  });

  it("throws ProblemError carrying the slug on a problem response", async () => {
    stubFetch(
      jsonResponse(404, { type: "/errors/net-definition-not-found", status: 404 }),
    );
    // The type assertion reads the response body first; the instanceof check
    // follows (a single stubbed Response body can only be consumed once).
    await expect(favoriteNet("ghost")).rejects.toMatchObject({
      problem: { type: "/errors/net-definition-not-found" },
    });
    await expect(favoriteNet("ghost")).rejects.toBeInstanceOf(ProblemError);
  });

  it("throws ProblemError carrying the rate-limit slug on 429", async () => {
    stubFetch(jsonResponse(429, { type: "/errors/rate-limited", status: 429 }));
    await expect(favoriteNet("def-1")).rejects.toMatchObject({
      problem: { type: "/errors/rate-limited" },
    });
  });
});

describe("unfavoriteNet", () => {
  it("DELETEs /api/favorites/{id} same-origin and resolves on 204", async () => {
    const mock = stubFetch(noContent());
    await expect(unfavoriteNet("def-1")).resolves.toBeUndefined();

    const [url, init] = mock.mock.calls[0] as [string, RequestInit];
    expect(url).toBe("/api/favorites/def-1");
    expect(init.method).toBe("DELETE");
    expect(init.credentials).toBe("same-origin");
  });

  it("throws ProblemError on a problem response", async () => {
    stubFetch(jsonResponse(401, { type: "/errors/unauthenticated", status: 401 }));
    await expect(unfavoriteNet("def-1")).rejects.toBeInstanceOf(ProblemError);
  });
});

describe("getMyNets", () => {
  it("GETs /api/favorites same-origin and returns the page — items AND nextCursor", async () => {
    const mock = stubFetch(
      jsonResponse(200, { items: [sampleFavorite], nextCursor: "1754000000123:abc" }),
    );
    const result = await getMyNets();

    const [url, init] = mock.mock.calls[0] as [string, RequestInit | undefined];
    expect(url).toBe("/api/favorites");
    expect(init?.credentials).toBe("same-origin");
    expect(result.items).toHaveLength(1);
    expect(result.items[0].id).toBe("def-1");
    expect(result.items[0].linkToken).toBe("tok-1");
    expect(result.nextCursor).toBe("1754000000123:abc");
  });

  it("sends the server's cursor back as ?cursor= when asked for the next page", async () => {
    const mock = stubFetch(jsonResponse(200, { items: [], nextCursor: null }));
    await getMyNets("1754000000123:abc");

    const [url] = mock.mock.calls[0] as [string, RequestInit | undefined];
    expect(url).toBe("/api/favorites?cursor=1754000000123%3Aabc");
  });

  it("returns an empty last page when the account has no favorites", async () => {
    stubFetch(jsonResponse(200, { items: [], nextCursor: null }));
    await expect(getMyNets()).resolves.toEqual({ items: [], nextCursor: null });
  });

  it("rejects a 200 that carries no nextCursor — a contract break is not smoothed to 'last page'", async () => {
    stubFetch(jsonResponse(200, { items: [sampleFavorite] }));
    await expect(getMyNets()).rejects.toThrow();
  });

  it("rejects a 200 whose items is missing or not an array — the break surfaces here, not inside render", async () => {
    // `rows: undefined` would reach `rows.map(...)` in the tab's render, outside
    // every catch, and unmount the page.
    stubFetch(jsonResponse(200, { nextCursor: null }));
    await expect(getMyNets()).rejects.toThrow();

    stubFetch(jsonResponse(200, { items: { id: "def-1" }, nextCursor: null }));
    await expect(getMyNets()).rejects.toThrow();
  });

  it("throws ProblemError on a problem response", async () => {
    stubFetch(jsonResponse(401, { type: "/errors/unauthenticated", status: 401 }));
    await expect(getMyNets()).rejects.toBeInstanceOf(ProblemError);
  });
});

/** The ids one membership request asked about, parsed off its URL. */
function askedIds(call: unknown[]): string[] {
  const [url] = call as [string, RequestInit | undefined];
  const ids = new URL(url, "http://test").searchParams.get("ids");
  expect(ids).not.toBeNull();
  return (ids as string).split(",");
}

describe("fetchFavoriteMembership", () => {
  it("GETs /api/favorites/membership?ids= same-origin with the asked ids and returns the favorited subset", async () => {
    const mock = stubFetch(jsonResponse(200, { favorited: ["def-2"] }));

    const result = await fetchFavoriteMembership(["def-1", "def-2", "def-3"]);

    expect(mock).toHaveBeenCalledTimes(1);
    const [url, init] = mock.mock.calls[0] as [string, RequestInit | undefined];
    expect(new URL(url, "http://test").pathname).toBe("/api/favorites/membership");
    expect(init?.credentials).toBe("same-origin");
    expect(askedIds(mock.mock.calls[0])).toEqual(["def-1", "def-2", "def-3"]);
    expect(result).toEqual(new Set(["def-2"]));
  });

  it("splits more than 200 ids into requests of at most 200 and unions the answers — over-cap is a 400, not a truncated answer", async () => {
    const ids = Array.from({ length: 401 }, (_, i) => `net-${i}`);
    const mock = vi.fn(async (url: string) => {
      const asked = (new URL(url, "http://test").searchParams.get("ids") as string).split(",");
      // Each batch answers with its own first id, so a dropped batch is a
      // missing id in the union, not merely a missing request.
      return jsonResponse(200, { favorited: [asked[0]] });
    });
    vi.stubGlobal("fetch", mock);

    const result = await fetchFavoriteMembership(ids);

    expect(mock).toHaveBeenCalledTimes(3);
    const batches = mock.mock.calls.map((call) => askedIds(call));
    expect(batches.map((batch) => batch.length)).toEqual([200, 200, 1]);
    expect(batches.flat()).toEqual(ids);
    expect(result).toEqual(new Set(["net-0", "net-200", "net-400"]));
  });

  it("makes no request for an empty list", async () => {
    const mock = stubFetch(jsonResponse(200, { favorited: [] }));
    await expect(fetchFavoriteMembership([])).resolves.toEqual(new Set());
    expect(mock).not.toHaveBeenCalled();
  });

  it("rejects a 200 that carries no favorited array — silence is not 'none favorited'", async () => {
    stubFetch(jsonResponse(200, {}));
    await expect(fetchFavoriteMembership(["def-1"])).rejects.toThrow();
  });

  it("throws ProblemError on a problem response", async () => {
    stubFetch(jsonResponse(429, { type: "/errors/rate-limited", status: 429 }));
    await expect(fetchFavoriteMembership(["def-1"])).rejects.toMatchObject({
      problem: { type: "/errors/rate-limited" },
    });
    stubFetch(jsonResponse(401, { type: "/errors/unauthenticated", status: 401 }));
    await expect(fetchFavoriteMembership(["def-1"])).rejects.toBeInstanceOf(ProblemError);
  });
});
