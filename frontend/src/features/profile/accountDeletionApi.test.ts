// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
import { afterEach, describe, expect, it, vi } from "vitest";

import { deleteAccount } from "./accountDeletionApi";

function response(status: number, body: unknown = null): Response {
  return new Response(status === 204 ? null : JSON.stringify(body), {
    status,
    headers: {
      "content-type":
        status >= 400 ? "application/problem+json" : "application/json",
    },
  });
}

function stubFetch(res: Response): ReturnType<typeof vi.fn> {
  const mock = vi.fn().mockResolvedValue(res);
  vi.stubGlobal("fetch", mock);
  return mock;
}

afterEach(() => {
  vi.unstubAllGlobals();
});

describe("deleteAccount", () => {
  it("DELETEs /api/accounts/me with same-origin credentials and resolves on 204", async () => {
    const mock = stubFetch(response(204));

    await expect(deleteAccount()).resolves.toBeUndefined();

    const [url, init] = mock.mock.calls[0] as [string, RequestInit];
    expect(url).toBe("/api/accounts/me");
    expect(init.method).toBe("DELETE");
    expect(init.credentials).toBe("same-origin");
  });

  it("treats a 401 as already-signed-out and resolves rather than throwing", async () => {
    stubFetch(response(401, { type: "/errors/unauthenticated", status: 401 }));

    await expect(deleteAccount()).resolves.toBeUndefined();
  });

  it("surfaces an unexpected failure as a ProblemError carrying the slug", async () => {
    stubFetch(response(500, { type: "/errors/internal", status: 500 }));

    await expect(deleteAccount()).rejects.toMatchObject({
      problem: { type: "/errors/internal" },
    });
  });
});
