// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
import { render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { ReactElement } from "react";

import { resetAppConfigForTests, useAppConfig } from "./useAppConfig";

// Hoisted so the `vi.mock` factory below (which is lifted above the imports)
// can close over it.
const { init } = vi.hoisted(() => ({ init: vi.fn() }));

vi.mock("@plausible-analytics/tracker/plausible.js", () => ({ init }));

function jsonResponse(body: unknown): Response {
  return new Response(JSON.stringify(body), {
    status: 200,
    headers: { "content-type": "application/json" },
  });
}

function stubConfig(body: Record<string, unknown>): ReturnType<typeof vi.fn> {
  const mock = vi.fn().mockResolvedValue(jsonResponse(body));
  vi.stubGlobal("fetch", mock);
  return mock;
}

/** Surfaces the hook's value so assertions read off the DOM. */
function Probe(): ReactElement {
  const config = useAppConfig();
  return <span data-testid="kofi">{config.kofiUsername ?? ""}</span>;
}

beforeEach(() => {
  // The config load is cached process-wide (one request per page load), so a
  // promise cached by an earlier case would mask this case's stub.
  resetAppConfigForTests();
  init.mockClear();
});

afterEach(() => {
  vi.unstubAllGlobals();
});

describe("useAppConfig", () => {
  it("loads nothing into the page when no integration is configured", async () => {
    stubConfig({
      plausibleDomain: null,
      plausibleScriptHost: null,
      kofiUsername: null,
    });

    render(<Probe />);
    await waitFor(() => {
      expect(screen.getByTestId("kofi")).toBeInTheDocument();
    });

    // The default posture: the tracker never starts, so an operator who opted
    // out sends nothing anywhere. That it is not even DOWNLOADED is a property
    // of the dynamic import, asserted against the built chunks in the e2e/build
    // checks rather than here.
    expect(init).not.toHaveBeenCalled();
  });

  it("initialises the tracker against Plausible's own host once a domain is configured", async () => {
    stubConfig({
      plausibleDomain: "netroll.radio",
      plausibleScriptHost: null,
      kofiUsername: null,
    });

    render(<Probe />);

    await waitFor(() => {
      expect(init).toHaveBeenCalledTimes(1);
    });
    expect(init).toHaveBeenCalledWith({
      domain: "netroll.radio",
      endpoint: "https://plausible.io/api/event",
    });
  });

  it("points the tracker at a self-hosted Plausible when one is configured", async () => {
    stubConfig({
      plausibleDomain: "netroll.radio",
      plausibleScriptHost: "https://analytics.example",
      kofiUsername: null,
    });

    render(<Probe />);

    await waitFor(() => {
      expect(init).toHaveBeenCalledWith({
        domain: "netroll.radio",
        endpoint: "https://analytics.example/api/event",
      });
    });
  });

  it("initialises the tracker only once across several mounted consumers", async () => {
    stubConfig({
      plausibleDomain: "netroll.radio",
      plausibleScriptHost: null,
      kofiUsername: null,
    });

    render(
      <>
        <Probe />
        <Probe />
        <Probe />
      </>,
    );

    // `init` is documented as callable once, and a second call would
    // double-count every pageview.
    await waitFor(() => {
      expect(init).toHaveBeenCalledTimes(1);
    });
  });

  it("keeps rendering when the tracker fails to load", async () => {
    stubConfig({
      plausibleDomain: "netroll.radio",
      plausibleScriptHost: null,
      kofiUsername: "n1cck",
    });
    init.mockImplementationOnce(() => {
      throw new Error("chunk unavailable");
    });

    render(<Probe />);

    // Analytics is decoration: a failed load must not take the page with it.
    await waitFor(() => {
      expect(screen.getByTestId("kofi")).toHaveTextContent("n1cck");
    });
  });

  it("reports the Ko-fi username so a caller can render the support link", async () => {
    stubConfig({
      plausibleDomain: null,
      plausibleScriptHost: null,
      kofiUsername: "n1cck",
    });

    render(<Probe />);

    await waitFor(() => {
      expect(screen.getByTestId("kofi")).toHaveTextContent("n1cck");
    });
    // A donation LINK needs no script — nothing third-party loads for it.
    expect(init).not.toHaveBeenCalled();
  });
});
