// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
import { afterEach, describe, expect, it, vi } from "vitest";

import {
  ProblemError,
  createSession,
  deleteCurrentSession,
  fetchCurrentAccount,
  requestMagicLink,
} from "./authApi";

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

describe("requestMagicLink", () => {
  it("POSTs the email with same-origin credentials and resolves on 202", async () => {
    const mock = stubFetch(new Response(null, { status: 202 }));

    await requestMagicLink("op@example.com");

    expect(mock).toHaveBeenCalledTimes(1);
    const [url, init] = mock.mock.calls[0] as [string, RequestInit];
    expect(url).toBe("/api/magic-links");
    expect(init.method).toBe("POST");
    expect(init.credentials).toBe("same-origin");
    expect(JSON.parse(init.body as string)).toEqual({
      email: "op@example.com",
    });
  });

  it("surfaces problem+json failures as ProblemError with the type slug", async () => {
    stubFetch(
      jsonResponse(429, { type: "/errors/rate-limited", status: 429 }),
    );

    const failure = requestMagicLink("op@example.com");

    await expect(failure).rejects.toBeInstanceOf(ProblemError);
    await expect(failure).rejects.toMatchObject({
      problem: { type: "/errors/rate-limited" },
    });
  });
});

describe("createSession", () => {
  it("POSTs the token and returns the signed-in account", async () => {
    const account = {
      id: "0198c5b6",
      email: "op@example.com",
      emailVerifiedAt: "2026-07-15T00:00:00Z",
    };
    const mock = stubFetch(jsonResponse(201, account));

    const result = await createSession("raw-token");

    expect(result).toEqual(account);
    const [url, init] = mock.mock.calls[0] as [string, RequestInit];
    expect(url).toBe("/api/sessions");
    expect(init.method).toBe("POST");
    expect(JSON.parse(init.body as string)).toEqual({ token: "raw-token" });
  });

  it("maps a consumed link to a ProblemError carrying the slug", async () => {
    stubFetch(
      jsonResponse(401, { type: "/errors/magic-link-consumed", status: 401 }),
    );

    await expect(createSession("used")).rejects.toMatchObject({
      problem: { type: "/errors/magic-link-consumed" },
    });
  });
});

describe("fetchCurrentAccount", () => {
  it("returns the account when the session is live", async () => {
    const account = { id: "1", email: "op@example.com", emailVerifiedAt: null };
    stubFetch(jsonResponse(200, account));

    expect(await fetchCurrentAccount()).toEqual(account);
  });

  it("returns null on 401 instead of throwing — signed-out is not an error", async () => {
    stubFetch(
      jsonResponse(401, { type: "/errors/unauthenticated", status: 401 }),
    );

    expect(await fetchCurrentAccount()).toBeNull();
  });
});

describe("deleteCurrentSession", () => {
  it("DELETEs the current session with same-origin credentials", async () => {
    const mock = stubFetch(new Response(null, { status: 204 }));

    await deleteCurrentSession();

    const [url, init] = mock.mock.calls[0] as [string, RequestInit];
    expect(url).toBe("/api/sessions/current");
    expect(init.method).toBe("DELETE");
    expect(init.credentials).toBe("same-origin");
  });
});
