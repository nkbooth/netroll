// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
import { afterEach, describe, expect, it, vi } from "vitest";

import { setCallsign } from "./callsignApi";

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

describe("setCallsign", () => {
  it("PUTs the callsign with same-origin credentials and returns the parsed account", async () => {
    const account = {
      id: "1",
      email: "op@example.com",
      emailVerifiedAt: "2026-07-15T00:00:00Z",
      consentRequired: false,
      requiredTermsVersion: "2026-07-15",
      callsign: "W1AW",
    };
    const mock = stubFetch(jsonResponse(200, account));

    const result = await setCallsign("w1aw/p");

    expect(result).toEqual(account);
    const [url, init] = mock.mock.calls[0] as [string, RequestInit];
    expect(url).toBe("/api/accounts/me/callsign");
    expect(init.method).toBe("PUT");
    expect(init.credentials).toBe("same-origin");
    expect(JSON.parse(init.body as string)).toEqual({ callsign: "w1aw/p" });
  });

  it("surfaces a taken callsign as a ProblemError carrying the slug", async () => {
    stubFetch(jsonResponse(409, { type: "/errors/callsign-taken", status: 409 }));

    await expect(setCallsign("W1AW")).rejects.toMatchObject({
      problem: { type: "/errors/callsign-taken" },
    });
  });
});
