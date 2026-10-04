// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
import { afterEach, describe, expect, it, vi } from "vitest";

import { confirmEmailChange, requestEmailChange } from "./emailChangeApi";

function jsonResponse(status: number, body: unknown): Response {
  return new Response(status === 202 ? null : JSON.stringify(body), {
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

describe("requestEmailChange", () => {
  it("POSTs the new email with same-origin credentials and resolves on 202", async () => {
    const mock = stubFetch(jsonResponse(202, null));

    await expect(requestEmailChange("new@example.com")).resolves.toBeUndefined();

    const [url, init] = mock.mock.calls[0] as [string, RequestInit];
    expect(url).toBe("/api/accounts/me/email-change");
    expect(init.method).toBe("POST");
    expect(init.credentials).toBe("same-origin");
    expect(JSON.parse(init.body as string)).toEqual({ email: "new@example.com" });
  });

  it("surfaces a taken email as a ProblemError carrying the slug", async () => {
    stubFetch(jsonResponse(409, { type: "/errors/email-taken", status: 409 }));

    await expect(requestEmailChange("taken@example.com")).rejects.toMatchObject({
      problem: { type: "/errors/email-taken" },
    });
  });
});

describe("confirmEmailChange", () => {
  it("POSTs the token with same-origin credentials and returns the new email", async () => {
    const mock = stubFetch(jsonResponse(200, { email: "new@example.com" }));

    const result = await confirmEmailChange("raw-token-abc");

    expect(result).toEqual({ email: "new@example.com" });
    const [url, init] = mock.mock.calls[0] as [string, RequestInit];
    expect(url).toBe("/api/email-changes");
    expect(init.method).toBe("POST");
    expect(init.credentials).toBe("same-origin");
    expect(JSON.parse(init.body as string)).toEqual({ token: "raw-token-abc" });
  });

  it("surfaces an expired confirmation link as a ProblemError carrying the slug", async () => {
    stubFetch(
      jsonResponse(401, { type: "/errors/email-change-expired", status: 401 }),
    );

    await expect(confirmEmailChange("stale")).rejects.toMatchObject({
      problem: { type: "/errors/email-change-expired" },
    });
  });
});
