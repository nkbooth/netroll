// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
import { afterEach, describe, expect, it, vi } from "vitest";

import { clearQrzCredentials, setQrzCredentials } from "./qrzCredentialsApi";

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

describe("setQrzCredentials", () => {
  it("PUTs the callsign + password with same-origin credentials", async () => {
    const mock = stubFetch(jsonResponse(204, null));

    await setQrzCredentials("W1AW", "s3cret");

    const [url, init] = mock.mock.calls[0] as [string, RequestInit];
    expect(url).toBe("/api/accounts/me/qrz-credentials");
    expect(init.method).toBe("PUT");
    expect(init.credentials).toBe("same-origin");
    expect(JSON.parse(init.body as string)).toEqual({
      callsign: "W1AW",
      password: "s3cret",
    });
  });

  it("surfaces the 503 crypto-unavailable slug as a ProblemError", async () => {
    stubFetch(
      jsonResponse(503, { type: "/errors/crypto-unavailable", status: 503 }),
    );

    await expect(setQrzCredentials("W1AW", "s3cret")).rejects.toMatchObject({
      problem: { type: "/errors/crypto-unavailable" },
    });
  });

  it("surfaces an invalid-credentials 422 as a ProblemError", async () => {
    stubFetch(
      jsonResponse(422, {
        type: "/errors/qrz-credentials-invalid",
        status: 422,
      }),
    );

    await expect(setQrzCredentials("W1AW", "")).rejects.toMatchObject({
      problem: { type: "/errors/qrz-credentials-invalid" },
    });
  });
});

describe("clearQrzCredentials", () => {
  it("DELETEs with same-origin credentials", async () => {
    const mock = stubFetch(jsonResponse(204, null));

    await clearQrzCredentials();

    const [url, init] = mock.mock.calls[0] as [string, RequestInit];
    expect(url).toBe("/api/accounts/me/qrz-credentials");
    expect(init.method).toBe("DELETE");
    expect(init.credentials).toBe("same-origin");
  });
});
