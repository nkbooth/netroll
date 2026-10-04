// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
import { afterEach, describe, expect, it, vi } from "vitest";

import { fetchRecentCheckIns } from "./checkInHistoryApi";

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
  const fetchMock = vi.fn().mockResolvedValue(response);
  vi.stubGlobal("fetch", fetchMock);
  return fetchMock;
}

/** The URL of the nth (default first) fetch call. */
function calledUrl(fetchMock: ReturnType<typeof vi.fn>, nth = 0): string {
  return String((fetchMock.mock.calls[nth] as [unknown, RequestInit])[0]);
}

afterEach(() => {
  vi.unstubAllGlobals();
});

describe("fetchRecentCheckIns", () => {
  it("requests the widget page size and no cursor on the first page", async () => {
    // The widget's page size lives in the caller, not in a backend constant —
    // the endpoint's own default/maximum are unchanged by this surface.
    const fetchMock = stubFetch(
      jsonResponse(200, { items: [], nextCursor: null }),
    );

    await fetchRecentCheckIns();

    const url = new URL(calledUrl(fetchMock), "https://example.test");
    expect(url.pathname).toBe("/api/accounts/me/check-ins");
    expect(url.searchParams.get("limit")).toBe("10");
    expect(url.searchParams.has("cursor")).toBe(false);
  });

  it("passes the server's cursor back verbatim", async () => {
    // The cursor is opaque; mangling it (or rebuilding it client-side) would
    // desync paging from the server's keyset, and the server refuses anything
    // it did not issue.
    const cursor = "1754000000123:00000000-0000-7000-8000-0000000000ab";
    const fetchMock = stubFetch(
      jsonResponse(200, { items: [], nextCursor: null }),
    );

    await fetchRecentCheckIns(cursor);

    const url = new URL(calledUrl(fetchMock), "https://example.test");
    expect(url.searchParams.get("cursor")).toBe(cursor);
    expect(url.searchParams.get("limit")).toBe("10");
  });

  it("returns the page envelope including the next cursor", async () => {
    const entry = {
      netSessionId: "0192f4a1-0000-7000-8000-00000000000a",
      netTitle: "Tuesday Traffic",
      band: "40m",
      mode: "ssb",
      callsign: "W1AW",
      checkedInAt: "2026-07-21T01:02:03+00:00",
    };
    stubFetch(jsonResponse(200, { items: [entry], nextCursor: "c1" }));

    await expect(fetchRecentCheckIns()).resolves.toEqual({
      items: [entry],
      nextCursor: "c1",
    });
  });

  it("sends the session cookie", async () => {
    const fetchMock = stubFetch(
      jsonResponse(200, { items: [], nextCursor: null }),
    );

    await fetchRecentCheckIns();

    const init = (
      fetchMock.mock.calls[0] as [unknown, RequestInit | undefined]
    )[1];
    expect(init?.credentials).toBe("same-origin");
  });

  it("rejects with the problem when the server refuses", async () => {
    stubFetch(
      jsonResponse(401, { type: "/errors/unauthenticated", status: 401 }),
    );

    await expect(fetchRecentCheckIns()).rejects.toMatchObject({
      problem: { type: "/errors/unauthenticated" },
    });
  });

  it("rejects with the problem on a server error", async () => {
    stubFetch(jsonResponse(500, { type: "/errors/internal", status: 500 }));

    await expect(fetchRecentCheckIns()).rejects.toMatchObject({
      problem: { type: "/errors/internal" },
    });
  });
});
