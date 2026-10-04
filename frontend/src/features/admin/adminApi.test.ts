// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
import { afterEach, describe, expect, it, vi } from "vitest";

import {
  disableAccount,
  fetchAbuseReports,
  fetchAuditLog,
  reenableAccount,
  resolveAbuseReport,
  searchObjects,
} from "./adminApi";

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

function calledInit(
  fetchMock: ReturnType<typeof vi.fn>,
  nth = 0,
): RequestInit | undefined {
  return (fetchMock.mock.calls[nth] as [unknown, RequestInit | undefined])[1];
}

afterEach(() => {
  vi.unstubAllGlobals();
});

describe("fetchAbuseReports", () => {
  it("requests the first page with no cursor param", () => {
    const fetchMock = stubFetch(
      jsonResponse(200, { items: [], nextCursor: null }),
    );

    void fetchAbuseReports();

    expect(calledUrl(fetchMock)).not.toContain("cursor=");
  });

  it("carries the server's cursor verbatim onto the next page request", async () => {
    // The cursor is opaque; mangling it (or rebuilding it client-side) would
    // desync paging from the server's keyset.
    const cursor = "1754000000123:00000000-0000-7000-8000-0000000000ab";
    const fetchMock = stubFetch(
      jsonResponse(200, { items: [], nextCursor: null }),
    );

    await fetchAbuseReports(cursor);

    const url = new URL(calledUrl(fetchMock), "https://example.test");
    expect(url.searchParams.get("cursor")).toBe(cursor);
  });

  it("returns the page envelope including the next cursor", async () => {
    const report = {
      id: "r1",
      createdAt: "2026-08-01T00:00:00Z",
      reporterContact: null,
      body: "spam",
      contextUrl: null,
    };
    stubFetch(jsonResponse(200, { items: [report], nextCursor: "c1" }));

    await expect(fetchAbuseReports()).resolves.toEqual({
      items: [report],
      nextCursor: "c1",
    });
  });

  it("throws a ProblemError carrying the slug when the server rejects", async () => {
    stubFetch(jsonResponse(403, { type: "/errors/forbidden", status: 403 }));

    await expect(fetchAbuseReports()).rejects.toMatchObject({
      problem: { type: "/errors/forbidden" },
    });
  });
});

describe("resolveAbuseReport", () => {
  it("POSTs to the report's resolve path", async () => {
    const fetchMock = stubFetch(new Response(null, { status: 204 }));

    await resolveAbuseReport("r1");

    expect(calledUrl(fetchMock)).toBe("/api/admin/abuse-reports/r1/resolve");
    expect(calledInit(fetchMock)?.method).toBe("POST");
  });

  it("surfaces a missing report as its problem slug", async () => {
    stubFetch(
      jsonResponse(404, { type: "/errors/abuse-report-not-found", status: 404 }),
    );

    await expect(resolveAbuseReport("gone")).rejects.toMatchObject({
      problem: { type: "/errors/abuse-report-not-found" },
    });
  });
});

describe("fetchAuditLog", () => {
  it("reads the audit-log page envelope", async () => {
    const entry = {
      id: "a1",
      occurredAt: "2026-08-01T00:00:00Z",
      actorAccountId: "acct-1",
      action: "disable-account",
      targetType: "account",
      targetId: "acct-2",
      metadata: { newlyDisabled: true },
    };
    const fetchMock = stubFetch(
      jsonResponse(200, { items: [entry], nextCursor: null }),
    );

    await expect(fetchAuditLog()).resolves.toEqual({
      items: [entry],
      nextCursor: null,
    });
    expect(calledUrl(fetchMock)).toContain("/api/admin/audit-log");
  });

  it("sends each investigation filter as its own param", async () => {
    const fetchMock = stubFetch(
      jsonResponse(200, { items: [], nextCursor: null }),
    );

    await fetchAuditLog({
      actor: "acct-1",
      object: "net-9",
      action: "disable-account",
    });

    const url = new URL(calledUrl(fetchMock), "https://example.test");
    expect(url.searchParams.get("actor")).toBe("acct-1");
    expect(url.searchParams.get("object")).toBe("net-9");
    expect(url.searchParams.get("action")).toBe("disable-account");
  });

  it("omits filters that are not set", async () => {
    const fetchMock = stubFetch(
      jsonResponse(200, { items: [], nextCursor: null }),
    );

    await fetchAuditLog({ actor: "acct-1" });

    const url = new URL(calledUrl(fetchMock), "https://example.test");
    expect(url.searchParams.has("object")).toBe(false);
    expect(url.searchParams.has("action")).toBe(false);
  });

  it("carries the filters alongside the cursor on a later page", async () => {
    // The server binds a cursor to the filter set that produced it and refuses
    // a mismatch — dropping the filters here would 400 every "load more".
    const fetchMock = stubFetch(
      jsonResponse(200, { items: [], nextCursor: null }),
    );

    await fetchAuditLog({ actor: "acct-1" }, "cursor-1");

    const url = new URL(calledUrl(fetchMock), "https://example.test");
    expect(url.searchParams.get("actor")).toBe("acct-1");
    expect(url.searchParams.get("cursor")).toBe("cursor-1");
  });
});

describe("searchObjects", () => {
  it("sends the search term url-encoded", async () => {
    // An email term contains '@' and a callsign can contain '/'; an unencoded
    // term would reach the server mangled or split the query string.
    const fetchMock = stubFetch(jsonResponse(200, { items: [], truncatedTypes: [] }));

    await searchObjects("op@example.com");

    const url = new URL(calledUrl(fetchMock), "https://example.test");
    expect(url.pathname).toBe("/api/admin/search");
    expect(url.searchParams.get("q")).toBe("op@example.com");
  });

  it("omits the type filter when searching every type", async () => {
    const fetchMock = stubFetch(jsonResponse(200, { items: [], truncatedTypes: [] }));

    await searchObjects("W1ABC");

    expect(calledUrl(fetchMock)).not.toContain("type=");
  });

  it("narrows to one object type when asked", async () => {
    const fetchMock = stubFetch(jsonResponse(200, { items: [], truncatedTypes: [] }));

    await searchObjects("Sunday", "net-definition");

    const url = new URL(calledUrl(fetchMock), "https://example.test");
    expect(url.searchParams.get("type")).toBe("net-definition");
  });

  it("returns the matched objects and which types were cut", async () => {
    // The envelope carries two facts and the caller needs both: a bare array
    // would drop the one that tells "no such account" from "not in the first
    // page".
    const match = {
      objectType: "account",
      id: "acct-target",
      label: "W1ABC",
      sublabel: "target@example.com",
      disabledAt: null,
      inactiveAt: null,
    };
    stubFetch(jsonResponse(200, { items: [match], truncatedTypes: [] }));

    await expect(searchObjects("W1ABC")).resolves.toEqual({
      hits: [match],
      truncatedTypes: [],
    });
  });

  it("carries a non-empty truncation signal through verbatim", async () => {
    stubFetch(
      jsonResponse(200, { items: [], truncatedTypes: ["account", "net-session"] }),
    );

    await expect(searchObjects("W1")).resolves.toMatchObject({
      truncatedTypes: ["account", "net-session"],
    });
  });

  it("rejects a 200 whose body carries no truncation signal", async () => {
    // Our own server always sends the field, so a body
    // without it is a contract break: it must surface as an error the tab can
    // show — not default to "complete" (ruled out), and not throw inside
    // render where it unmounts the whole page.
    stubFetch(jsonResponse(200, { items: [] }));

    await expect(searchObjects("W1")).rejects.toThrow();
  });

  it("resolves to an empty, complete result when nothing matched", async () => {
    // A no-match is a successful 200, not an error the caller must catch.
    stubFetch(jsonResponse(200, { items: [], truncatedTypes: [] }));

    await expect(searchObjects("W9ZZZ")).resolves.toEqual({
      hits: [],
      truncatedTypes: [],
    });
  });

  it("throws a ProblemError when the term is refused", async () => {
    stubFetch(jsonResponse(400, { type: "/errors/validation", status: 400 }));

    await expect(searchObjects("")).rejects.toMatchObject({
      problem: { type: "/errors/validation" },
    });
  });
});

describe("account actions", () => {
  it("POSTs to the disable path", async () => {
    const fetchMock = stubFetch(new Response(null, { status: 204 }));

    await disableAccount("acct-2");

    expect(calledUrl(fetchMock)).toBe("/api/admin/accounts/acct-2/disable");
    expect(calledInit(fetchMock)?.method).toBe("POST");
  });

  it("POSTs to the reenable path", async () => {
    const fetchMock = stubFetch(new Response(null, { status: 204 }));

    await reenableAccount("acct-2");

    expect(calledUrl(fetchMock)).toBe("/api/admin/accounts/acct-2/reenable");
    expect(calledInit(fetchMock)?.method).toBe("POST");
  });

  it("surfaces a refused self-disable as its problem slug", async () => {
    stubFetch(
      jsonResponse(409, { type: "/errors/cannot-disable-self", status: 409 }),
    );

    await expect(disableAccount("self")).rejects.toMatchObject({
      problem: { type: "/errors/cannot-disable-self" },
    });
  });
});
