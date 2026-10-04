// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";

import { CloseNetControl } from "./CloseNetControl";
import { messageForProblemType } from "../../errors/problemMessages";
import { expectNoAxeViolations } from "../../test/axe";

function jsonResponse(status: number, body: unknown): Response {
  return new Response(JSON.stringify(body), {
    status,
    headers: {
      "content-type":
        status >= 400 ? "application/problem+json" : "application/json",
    },
  });
}

afterEach(() => {
  vi.unstubAllGlobals();
});

describe("CloseNetControl", () => {
  it("closes the session and reports the returned summary", async () => {
    const user = userEvent.setup();
    const onClosed = vi.fn();
    const fetchMock = vi
      .fn()
      .mockResolvedValue(jsonResponse(200, { id: "abc", lifecycle: "closed" }));
    vi.stubGlobal("fetch", fetchMock);

    render(<CloseNetControl sessionId="abc" onClosed={onClosed} />);
    await user.click(screen.getByRole("button", { name: /close net/i }));

    const [url, init] = fetchMock.mock.calls[0] as [string, RequestInit];
    expect(url).toBe("/api/net-sessions/abc/close");
    expect(init.method).toBe("POST");
    expect(onClosed).toHaveBeenCalledTimes(1);
  });

  it("surfaces a mapped problem message on failure", async () => {
    const user = userEvent.setup();
    const fetchMock = vi
      .fn()
      .mockResolvedValue(jsonResponse(409, { type: "/errors/forbidden", status: 409 }));
    vi.stubGlobal("fetch", fetchMock);

    render(<CloseNetControl sessionId="abc" onClosed={vi.fn()} />);
    await user.click(screen.getByRole("button", { name: /close net/i }));

    expect(await screen.findByRole("alert")).toHaveTextContent(
      messageForProblemType("/errors/forbidden"),
    );
  });

  it("has no WCAG 2.1 AA violations", async () => {
    const { container } = render(<CloseNetControl sessionId="abc" onClosed={vi.fn()} />);
    await expectNoAxeViolations(container);
  });
  it("prefers the server's problem detail over the slug-map fallback", async () => {
    // One of forty-two surfaces. The assertion is the preference
    // ORDER, not the copy.
    const detail = "the field-naming answer the server sent";
    const user = userEvent.setup();
    const fetchMock = vi.fn().mockResolvedValue(
      jsonResponse(409, { type: "/errors/forbidden", status: 409, detail }),
    );
    vi.stubGlobal("fetch", fetchMock);

    render(<CloseNetControl sessionId="abc" onClosed={vi.fn()} />);
    await user.click(screen.getByRole("button", { name: /close net/i }));

    const alert = await screen.findByRole("alert");
    expect(alert).toHaveTextContent(detail);
    expect(alert).not.toHaveTextContent(
      messageForProblemType("/errors/forbidden"),
    );
  });

});
