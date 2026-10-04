// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
import { afterEach, describe, expect, it, vi } from "vitest";

import { fetchAppConfig, plausibleEndpoint } from "./appConfigApi";

function jsonResponse(status: number, body: unknown): Response {
  return new Response(JSON.stringify(body), {
    status,
    headers: { "content-type": "application/json" },
  });
}

afterEach(() => {
  vi.unstubAllGlobals();
});

describe("fetchAppConfig", () => {
  it("reads the instance settings from the public endpoint without credentials", async () => {
    const mock = vi.fn().mockResolvedValue(
      jsonResponse(200, {
        plausibleDomain: "netroll.radio",
        plausibleScriptHost: null,
        kofiUsername: "n1cck",
      }),
    );
    vi.stubGlobal("fetch", mock);

    const config = await fetchAppConfig();

    expect(config).toEqual({
      plausibleDomain: "netroll.radio",
      plausibleScriptHost: null,
      kofiUsername: "n1cck",
    });
    expect(mock.mock.calls[0]?.[0]).toBe("/api/app-config");
  });

  it("treats an unreachable or failing endpoint as everything-off", async () => {
    // Analytics and a donation link are decoration: a failed config read must
    // never break the page that was about to render.
    vi.stubGlobal("fetch", vi.fn().mockRejectedValue(new TypeError("offline")));

    await expect(fetchAppConfig()).resolves.toEqual({
      plausibleDomain: null,
      plausibleScriptHost: null,
      kofiUsername: null,
    });
  });

  it("treats a non-OK response as everything-off too", async () => {
    vi.stubGlobal("fetch", vi.fn().mockResolvedValue(jsonResponse(500, {})));

    await expect(fetchAppConfig()).resolves.toEqual({
      plausibleDomain: null,
      plausibleScriptHost: null,
      kofiUsername: null,
    });
  });
});

describe("plausibleEndpoint", () => {
  it("defaults to Plausible's own host when no script host is configured", () => {
    expect(plausibleEndpoint(null)).toBe("https://plausible.io/api/event");
  });

  it("uses a self-hosted host, tolerating a trailing slash", () => {
    expect(plausibleEndpoint("https://analytics.example")).toBe(
      "https://analytics.example/api/event",
    );
    expect(plausibleEndpoint("https://analytics.example/")).toBe(
      "https://analytics.example/api/event",
    );
  });
});
