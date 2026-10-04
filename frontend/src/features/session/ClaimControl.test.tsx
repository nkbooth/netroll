// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";

import { ClaimControl } from "./ClaimControl";
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

describe("ClaimControl", () => {
  it("POSTs claim-control (no body) and reports the returned summary", async () => {
    const user = userEvent.setup();
    const onClaimed = vi.fn();
    const fetchMock = vi
      .fn()
      .mockResolvedValue(jsonResponse(200, { id: "abc", controlState: "active" }));
    vi.stubGlobal("fetch", fetchMock);

    render(<ClaimControl sessionId="abc" onClaimed={onClaimed} />);
    await user.click(screen.getByRole("button", { name: /take control/i }));

    const [url, init] = fetchMock.mock.calls[0] as [string, RequestInit];
    expect(url).toBe("/api/net-sessions/abc/claim-control");
    expect(init.method).toBe("POST");
    // No body — the claimer IS the target.
    expect(init.body).toBeUndefined();
    expect(onClaimed).toHaveBeenCalledTimes(1);
  });

  it("surfaces the control-not-stalled message when the net isn't paused", async () => {
    const user = userEvent.setup();
    const fetchMock = vi
      .fn()
      .mockResolvedValue(jsonResponse(409, { type: "/errors/control-not-stalled", status: 409 }));
    vi.stubGlobal("fetch", fetchMock);

    render(<ClaimControl sessionId="abc" onClaimed={vi.fn()} />);
    await user.click(screen.getByRole("button", { name: /take control/i }));

    expect(await screen.findByRole("alert")).toHaveTextContent(
      messageForProblemType("/errors/control-not-stalled"),
    );
  });

  it("has no WCAG 2.1 AA violations", async () => {
    const { container } = render(<ClaimControl sessionId="abc" onClaimed={vi.fn()} />);
    await expectNoAxeViolations(container);
  });
  it("prefers the server's problem detail over the slug-map fallback", async () => {
    // One of forty-two surfaces. The assertion is the preference
    // ORDER, not the copy.
    const detail = "the field-naming answer the server sent";
    const user = userEvent.setup();
    const fetchMock = vi.fn().mockResolvedValue(
      jsonResponse(409, {
        type: "/errors/control-not-stalled",
        status: 409,
        detail,
      }),
    );
    vi.stubGlobal("fetch", fetchMock);

    render(<ClaimControl sessionId="abc" onClaimed={vi.fn()} />);
    await user.click(screen.getByRole("button", { name: /take control/i }));

    const alert = await screen.findByRole("alert");
    expect(alert).toHaveTextContent(detail);
    expect(alert).not.toHaveTextContent(
      messageForProblemType("/errors/control-not-stalled"),
    );
  });

});
