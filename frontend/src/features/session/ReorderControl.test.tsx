// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";

import { ReorderControl } from "./ReorderControl";
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

describe("ReorderControl", () => {
  it("POSTs the precedence reorder and reports the returned summary", async () => {
    const user = userEvent.setup();
    const onReordered = vi.fn();
    const fetchMock = vi
      .fn()
      .mockResolvedValue(jsonResponse(200, { id: "abc", lifecycle: "live" }));
    vi.stubGlobal("fetch", fetchMock);

    render(<ReorderControl sessionId="abc" onReordered={onReordered} />);
    await user.click(screen.getByRole("button", { name: /order by precedence/i }));

    const [url, init] = fetchMock.mock.calls[0] as [string, RequestInit];
    expect(url).toBe("/api/net-sessions/abc/reorder");
    expect(init.method).toBe("POST");
    expect(JSON.parse(init.body as string)).toEqual({ by: "precedence" });
    expect(onReordered).toHaveBeenCalledTimes(1);
  });

  it("surfaces a mapped problem message when a non-NCS is refused (403)", async () => {
    const user = userEvent.setup();
    const fetchMock = vi
      .fn()
      .mockResolvedValue(jsonResponse(403, { type: "/errors/forbidden", status: 403 }));
    vi.stubGlobal("fetch", fetchMock);

    render(<ReorderControl sessionId="abc" onReordered={vi.fn()} />);
    await user.click(screen.getByRole("button", { name: /order by precedence/i }));

    expect(await screen.findByRole("alert")).toHaveTextContent(
      messageForProblemType("/errors/forbidden"),
    );
  });

  it("has no WCAG 2.1 AA violations", async () => {
    const { container } = render(<ReorderControl sessionId="abc" onReordered={vi.fn()} />);
    await expectNoAxeViolations(container);
  });
  it("prefers the server's problem detail over the slug-map fallback", async () => {
    // One of forty-two surfaces. The assertion is the preference
    // ORDER, not the copy.
    const detail = "the field-naming answer the server sent";
    const user = userEvent.setup();
    const fetchMock = vi.fn().mockResolvedValue(
      jsonResponse(403, { type: "/errors/forbidden", status: 403, detail }),
    );
    vi.stubGlobal("fetch", fetchMock);

    render(<ReorderControl sessionId="abc" onReordered={vi.fn()} />);
    await user.click(screen.getByRole("button", { name: /order by precedence/i }));

    const alert = await screen.findByRole("alert");
    expect(alert).toHaveTextContent(detail);
    expect(alert).not.toHaveTextContent(
      messageForProblemType("/errors/forbidden"),
    );
  });

});
