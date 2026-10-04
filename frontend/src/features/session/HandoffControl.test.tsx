// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";

import { HandoffControl } from "./HandoffControl";
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

const targets = [
  { accountId: "acct-1", callsign: "N1CCK" },
  { accountId: "acct-2", callsign: "K1XYZ" },
];

afterEach(() => {
  vi.unstubAllGlobals();
});

describe("HandoffControl", () => {
  it("POSTs the selected target account id and reports the summary", async () => {
    const user = userEvent.setup();
    const onHandedOff = vi.fn();
    const fetchMock = vi
      .fn()
      .mockResolvedValue(jsonResponse(200, { id: "abc", controlState: "active" }));
    vi.stubGlobal("fetch", fetchMock);

    render(<HandoffControl sessionId="abc" targets={targets} onHandedOff={onHandedOff} />);
    // Pick the second target, then hand off.
    await user.selectOptions(screen.getByLabelText(/hand off control/i), "acct-2");
    await user.click(screen.getByRole("button", { name: /hand off/i }));

    const [url, init] = fetchMock.mock.calls[0] as [string, RequestInit];
    expect(url).toBe("/api/net-sessions/abc/handoff");
    expect(init.method).toBe("POST");
    expect(JSON.parse(init.body as string)).toEqual({ targetAccountId: "acct-2" });
    expect(onHandedOff).toHaveBeenCalledTimes(1);
  });

  it("surfaces the unqualified-target message on a 422", async () => {
    const user = userEvent.setup();
    const fetchMock = vi.fn().mockResolvedValue(
      jsonResponse(422, { type: "/errors/handoff-target-unqualified", status: 422 }),
    );
    vi.stubGlobal("fetch", fetchMock);

    render(<HandoffControl sessionId="abc" targets={targets} onHandedOff={vi.fn()} />);
    await user.click(screen.getByRole("button", { name: /hand off/i }));

    expect(await screen.findByRole("alert")).toHaveTextContent(
      messageForProblemType("/errors/handoff-target-unqualified"),
    );
  });

  it("has no WCAG 2.1 AA violations", async () => {
    const { container } = render(
      <HandoffControl sessionId="abc" targets={targets} onHandedOff={vi.fn()} />,
    );
    await expectNoAxeViolations(container);
  });
  it("prefers the server's problem detail over the slug-map fallback", async () => {
    // One of forty-two surfaces. The assertion is the preference
    // ORDER, not the copy.
    const detail = "the field-naming answer the server sent";
    const user = userEvent.setup();
    const fetchMock = vi.fn().mockResolvedValue(
      jsonResponse(422, {
        type: "/errors/handoff-target-unqualified",
        status: 422,
        detail,
      }),
    );
    vi.stubGlobal("fetch", fetchMock);

    render(<HandoffControl sessionId="abc" targets={targets} onHandedOff={vi.fn()} />);
    await user.click(screen.getByRole("button", { name: /hand off/i }));

    const alert = await screen.findByRole("alert");
    expect(alert).toHaveTextContent(detail);
    expect(alert).not.toHaveTextContent(
      messageForProblemType("/errors/handoff-target-unqualified"),
    );
  });

});
