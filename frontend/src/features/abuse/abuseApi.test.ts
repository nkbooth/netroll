// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
import { afterEach, describe, expect, it, vi } from "vitest";

import { submitAbuseReport } from "./abuseApi";
import { ProblemError } from "../auth/authApi";

function problemResponse(status: number, type: string): Response {
  return new Response(JSON.stringify({ type, status }), {
    status,
    headers: { "content-type": "application/problem+json" },
  });
}

afterEach(() => {
  vi.unstubAllGlobals();
});

describe("submitAbuseReport", () => {
  it("POSTs the report body and bot-mitigation fields to the public endpoint", async () => {
    const fetchMock = vi.fn().mockResolvedValue(new Response(null, { status: 202 }));
    vi.stubGlobal("fetch", fetchMock);

    await submitAbuseReport(
      { body: "spam net titles", reporterContact: "W1RPT" },
      { hpField: "", formToken: "signed.token" },
    );

    const [url, init] = fetchMock.mock.calls[0] as [string, RequestInit];
    expect(url).toBe("/api/abuse-reports");
    expect(init.method).toBe("POST");
    expect(JSON.parse(init.body as string)).toEqual({
      body: "spam net titles",
      reporterContact: "W1RPT",
      hpField: "",
      formToken: "signed.token",
    });
  });

  it("throws a ProblemError carrying the slug when the server rejects", async () => {
    vi.stubGlobal(
      "fetch",
      vi.fn().mockResolvedValue(problemResponse(429, "/errors/rate-limited")),
    );

    await expect(submitAbuseReport({ body: "x" })).rejects.toMatchObject({
      problem: { type: "/errors/rate-limited" },
    });
    await expect(submitAbuseReport({ body: "x" })).rejects.toBeInstanceOf(ProblemError);
  });
});
