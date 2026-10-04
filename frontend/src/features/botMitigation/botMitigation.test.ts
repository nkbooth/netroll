// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
import { afterEach, describe, expect, it, vi } from "vitest";

import { fetchFormToken, withBotMitigation } from "./botMitigation";

function jsonResponse(status: number, body: unknown): Response {
  return new Response(JSON.stringify(body), {
    status,
    headers: { "content-type": "application/json" },
  });
}

afterEach(() => {
  vi.unstubAllGlobals();
});

describe("withBotMitigation", () => {
  it("attaches the token and the honeypot value when a token was issued", () => {
    const merged = withBotMitigation({ email: "op@example.com" }, "sig.123", "");
    expect(merged).toEqual({
      email: "op@example.com",
      hpField: "",
      formToken: "sig.123",
    });
  });

  it("omits the token field entirely when mitigation is disabled (null token)", () => {
    const merged = withBotMitigation({ email: "op@example.com" }, null, "");
    expect(merged).toEqual({ email: "op@example.com", hpField: "" });
    expect("formToken" in merged).toBe(false);
  });

  it("passes a filled honeypot through verbatim (the bot case)", () => {
    const merged = withBotMitigation({ a: 1 }, "sig.9", "http://spam.example");
    expect(merged.hpField).toBe("http://spam.example");
  });
});

describe("fetchFormToken", () => {
  it("returns the token from the response body", async () => {
    vi.stubGlobal(
      "fetch",
      vi.fn().mockResolvedValue(jsonResponse(200, { formToken: "sig.abc" })),
    );
    expect(await fetchFormToken()).toBe("sig.abc");
  });

  it("returns null when mitigation is disabled (null token), with no retry", async () => {
    const fetchMock = vi
      .fn()
      .mockResolvedValue(jsonResponse(200, { formToken: null }));
    vi.stubGlobal("fetch", fetchMock);
    expect(await fetchFormToken()).toBeNull();
    // A well-formed "disabled" response is not a failure — must not retry.
    expect(fetchMock).toHaveBeenCalledTimes(1);
  });

  it("retries once and recovers from a transient non-ok response", async () => {
    const fetchMock = vi
      .fn()
      .mockResolvedValueOnce(new Response(null, { status: 500 }))
      .mockResolvedValueOnce(jsonResponse(200, { formToken: "sig.retry" }));
    vi.stubGlobal("fetch", fetchMock);
    expect(await fetchFormToken()).toBe("sig.retry");
    expect(fetchMock).toHaveBeenCalledTimes(2);
  });

  it("retries once and recovers from a thrown network error", async () => {
    const fetchMock = vi
      .fn()
      .mockRejectedValueOnce(new Error("network down"))
      .mockResolvedValueOnce(jsonResponse(200, { formToken: "sig.retry" }));
    vi.stubGlobal("fetch", fetchMock);
    expect(await fetchFormToken()).toBe("sig.retry");
    expect(fetchMock).toHaveBeenCalledTimes(2);
  });

  it("fails open to null on a non-ok response that persists across the retry", async () => {
    vi.stubGlobal(
      "fetch",
      vi.fn().mockResolvedValue(new Response(null, { status: 500 })),
    );
    expect(await fetchFormToken()).toBeNull();
  });

  it("fails open to null when the request throws on every attempt", async () => {
    vi.stubGlobal(
      "fetch",
      vi.fn().mockRejectedValue(new Error("network down")),
    );
    expect(await fetchFormToken()).toBeNull();
  });
});
