// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";

import { WorkedSinkToggle } from "./WorkedSinkToggle";
import { messageForProblemType } from "../../errors/problemMessages";
import { expectNoAxeViolations } from "../../test/axe";

function jsonResponse(status: number, body: unknown): Response {
  return new Response(JSON.stringify(body), {
    status,
    headers: {
      "content-type": status >= 400 ? "application/problem+json" : "application/json",
    },
  });
}

afterEach(() => {
  vi.unstubAllGlobals();
});

describe("WorkedSinkToggle", () => {
  it("POSTs the worked-sink mode and reports the returned summary", async () => {
    const user = userEvent.setup();
    const onModeSet = vi.fn();
    const fetchMock = vi
      .fn()
      .mockResolvedValue(
        jsonResponse(200, { id: "abc", lifecycle: "live", rosterOrderMode: "worked-sink" }),
      );
    vi.stubGlobal("fetch", fetchMock);

    render(<WorkedSinkToggle sessionId="abc" mode="manual" onModeSet={onModeSet} />);
    await user.click(screen.getByRole("switch"));

    const [url, init] = fetchMock.mock.calls[0] as [string, RequestInit];
    expect(url).toBe("/api/net-sessions/abc/roster-order-mode");
    expect(init.method).toBe("POST");
    expect(JSON.parse(init.body as string)).toEqual({ mode: "worked-sink" });
    expect(onModeSet).toHaveBeenCalledTimes(1);
  });

  it("POSTs the legacy mode when the sink is already on", async () => {
    const user = userEvent.setup();
    const fetchMock = vi
      .fn()
      .mockResolvedValue(
        jsonResponse(200, { id: "abc", lifecycle: "live", rosterOrderMode: "manual" }),
      );
    vi.stubGlobal("fetch", fetchMock);

    render(<WorkedSinkToggle sessionId="abc" mode="worked-sink" onModeSet={vi.fn()} />);
    expect(screen.getByRole("switch")).toHaveAttribute("aria-checked", "true");
    await user.click(screen.getByRole("switch"));

    const [, init] = fetchMock.mock.calls[0] as [string, RequestInit];
    expect(JSON.parse(init.body as string)).toEqual({ mode: "manual" });
  });

  it("surfaces a mapped problem message when a non-NCS is refused (403)", async () => {
    const user = userEvent.setup();
    const fetchMock = vi
      .fn()
      .mockResolvedValue(jsonResponse(403, { type: "/errors/forbidden", status: 403 }));
    vi.stubGlobal("fetch", fetchMock);

    render(<WorkedSinkToggle sessionId="abc" mode="manual" onModeSet={vi.fn()} />);
    await user.click(screen.getByRole("switch"));

    expect(await screen.findByRole("alert")).toHaveTextContent(
      messageForProblemType("/errors/forbidden"),
    );
  });

  it("has no WCAG 2.1 AA violations", async () => {
    const { container } = render(
      <WorkedSinkToggle sessionId="abc" mode="manual" onModeSet={vi.fn()} />,
    );
    await expectNoAxeViolations(container);
  });
});
