// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";
import { RouterProvider, createMemoryRouter } from "react-router";

import { StartNetControl } from "./StartNetControl";
import { messageForProblemType } from "../../errors/problemMessages";
import { expectNoAxeViolations } from "../../test/axe";

// A session carries its ways IN, not one frequency. The fixtures
// below reach the net on one HF connection unless a test says otherwise.
function hfWay(plannedFrequencyHz = 14_230_000) {
  return {
    id: "00000000-0000-0000-0000-000000001604",
    position: 0,
    kind: "hf",
    plannedFrequencyHz,
    band: "20m",
    mode: "ssb",
    repeaterOffsetHz: null,
    toneMode: null,
    toneValue: null,
    node: null,
    reflector: null,
    network: null,
    talkgroup: null,
    label: null,
    detail: null,
  };
}


function jsonResponse(status: number, body: unknown): Response {
  return new Response(JSON.stringify(body), {
    status,
    headers: {
      "content-type":
        status >= 400 ? "application/problem+json" : "application/json",
    },
  });
}

const uuid = (n: number): string =>
  `00000000-0000-0000-0000-${n.toString(16).padStart(12, "0")}`;

function summary() {
  return {
    id: uuid(55),
    definitionId: uuid(7),
    definitionVersion: 1,
    lifecycle: "live",
    connections: [hfWay(14_250_000)],
    startedAt: "2026-07-16T00:00:00Z",
    closedAt: null,
    durationSeconds: null,
    latestSeq: 1,
    participantCount: 0,
    roster: [],
    definition: {
      title: "N",
      connections: [hfWay()],
      netCategory: "traffic",
      netType: "open",
    },
  };
}

function renderControl(fetchMock: ReturnType<typeof vi.fn>) {
  vi.stubGlobal("fetch", fetchMock);
  const router = createMemoryRouter(
    [
      {
        path: "/",
        element: <StartNetControl definitionId={uuid(7)} />,
      },
      { path: "/net-sessions/:id", element: <div data-testid="live-page" /> },
    ],
    { initialEntries: ["/"] },
  );
  render(<RouterProvider router={router} />);
  return router;
}

afterEach(() => {
  vi.unstubAllGlobals();
});

describe("StartNetControl", () => {
  it("starts a session and navigates to the new live-session route", async () => {
    const user = userEvent.setup();
    const fetchMock = vi.fn().mockResolvedValue(jsonResponse(201, summary()));
    renderControl(fetchMock);

    await user.click(screen.getByRole("button", { name: /start net/i }));

    expect(await screen.findByTestId("live-page")).toBeInTheDocument();
    const [url, init] = fetchMock.mock.calls[0] as [string, RequestInit];
    expect(url).toBe("/api/net-sessions");
    expect(JSON.parse(init.body as string)).toEqual({
      definitionId: uuid(7),
    });
  });

  it("surfaces a mapped problem message on failure without navigating", async () => {
    const user = userEvent.setup();
    const fetchMock = vi
      .fn()
      .mockResolvedValue(jsonResponse(409, { type: "/errors/forbidden", status: 409 }));
    renderControl(fetchMock);

    await user.click(screen.getByRole("button", { name: /start net/i }));

    const alert = await screen.findByRole("alert");
    expect(alert).toHaveTextContent(messageForProblemType("/errors/forbidden"));
    expect(screen.queryByTestId("live-page")).not.toBeInTheDocument();
  });

  it("has no WCAG 2.1 AA violations", async () => {
    const { container } = render(
      <RouterProvider
        router={createMemoryRouter(
          [
            {
              path: "/",
              element: (
                <StartNetControl definitionId={uuid(7)} />
              ),
            },
          ],
          { initialEntries: ["/"] },
        )}
      />,
    );
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
    renderControl(fetchMock);

    await user.click(screen.getByRole("button", { name: /start net/i }));

    const alert = await screen.findByRole("alert");
    expect(alert).toHaveTextContent(detail);
    expect(alert).not.toHaveTextContent(
      messageForProblemType("/errors/forbidden"),
    );
  });

});
