// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
import { act, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { RouterProvider, createMemoryRouter } from "react-router";

import { LiveSessionPage } from "./LiveSessionPage";
import { connectionPresentation } from "./ConnectionStatus";
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


/** A no-op WebSocket so the stream never opens a real connection in jsdom. */
class NoopWebSocket {
  onopen: (() => void) | null = null;
  onmessage: (() => void) | null = null;
  onclose: (() => void) | null = null;
  onerror: (() => void) | null = null;
  readonly url: string;
  constructor(url: string) {
    this.url = url;
  }
  close(): void {}
}

/**
 * A WebSocket stand-in whose `onopen`/`onmessage` the test can invoke directly,
 * driving a live delta through the shipped stream wiring (the `defaultSocketFactory`
 * assigns these handlers and forwards `event.data`).
 */
class DrivableWebSocket {
  onopen: (() => void) | null = null;
  onmessage: ((event: MessageEvent) => void) | null = null;
  onclose: ((event: { code: number }) => void) | null = null;
  onerror: (() => void) | null = null;
  readonly url: string;
  constructor(url: string) {
    this.url = url;
  }
  close(): void {}
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

function summary(overrides: Record<string, unknown> = {}) {
  return {
    id: uuid(1),
    definitionId: uuid(7),
    definitionVersion: 3,
    lifecycle: "live",
    connections: [hfWay(14_250_000)],
    startedAt: "2026-07-16T00:00:00Z",
    closedAt: null,
    durationSeconds: null,
    latestSeq: 3,
    participantCount: 2,
    // Default to an Owner viewer so the existing owner-context assertions (close/
    // frequency/reorder/net-note/quick-add controls) keep holding;
    // the relay-gate tests below override this to "relay".
    viewerRole: "owner",
    roster: [
      { checkInId: uuid(42), callsign: "W1AW", addedAt: "t", addedBy: uuid(200) },
      { checkInId: uuid(43), callsign: "N1CCK", addedAt: "t", addedBy: uuid(200) },
    ],
    definition: {
      title: "Sunday Traffic Net",
      connections: [hfWay()],
      netCategory: "traffic",
      netType: "open",
    },
    ...overrides,
  };
}

function renderPage(
  fetchMock: ReturnType<typeof vi.fn>,
  options: {
    // `undefined` (the default) leaves `/api/accounts/me` to fall through to
    // `fetchMock` as before (harmless — most tests never inspect it); the
    // HandoffControl tests pass an explicit account to drive `isActiveNcs`.
    readonly account?: Record<string, unknown> | null;
    // The `GET …/roles` grants list — RoleManagementPanel AND
    // HandoffControl's target-fetch both read this. Empty by default.
    readonly roles?: readonly Record<string, unknown>[];
  } = {},
) {
  const { account, roles = [] } = options;
  // A ManageRoles viewer (owner/net-control) mounts the RoleManagementPanel,
  // which auto-loads its grants list from GET …/roles. Default that
  // read to an empty list so the blanket summary mocks don't feed the panel a
  // non-array; tests never drive grant/revoke through this page.
  const callMock = fetchMock as unknown as (
    input: RequestInfo | URL,
    init?: RequestInit,
  ) => Promise<Response>;
  const wrapped = vi.fn((input: RequestInfo | URL, init?: RequestInit) => {
    const url = typeof input === "string" ? input : input.toString();
    const method = (init?.method ?? "GET").toUpperCase();
    if (method === "GET" && url.endsWith("/roles")) {
      return Promise.resolve(jsonResponse(200, roles));
    }
    if (account !== undefined && url.endsWith("/api/accounts/me")) {
      return Promise.resolve(
        account === null
          ? jsonResponse(401, { type: "/errors/unauthenticated", status: 401 })
          : jsonResponse(200, account),
      );
    }
    return callMock(input, init);
  });
  vi.stubGlobal("fetch", wrapped);
  const router = createMemoryRouter(
    [
      { path: "/net-sessions/:id", element: <LiveSessionPage /> },
      { path: "/sign-in", element: <div data-testid="sign-in" /> },
    ],
    { initialEntries: [`/net-sessions/${uuid(1)}`] },
  );
  render(<RouterProvider router={router} />);
  return router;
}

beforeEach(() => {
  vi.stubGlobal("WebSocket", NoopWebSocket as unknown as typeof WebSocket);
});

afterEach(() => {
  vi.unstubAllGlobals();
});

describe("LiveSessionPage", () => {
  it("renders the folded roster in seq order once the snapshot loads", async () => {
    const fetchMock = vi.fn().mockResolvedValue(jsonResponse(200, summary()));
    renderPage(fetchMock);

    expect(await screen.findByText("W1AW")).toBeInTheDocument();
    // The callsign is the mono span; the row now also carries a source badge, so
    // assert on the callsign face rather than the row's full textContent. Scoped
    // to the Roster region — the page also carries a breadcrumb trail (its own
    // `<li>`s) that would otherwise pollute a page-wide listitem query.
    const calls = within(screen.getByRole("list", { name: "Roster" }))
      .getAllByRole("listitem")
      .map((li) => li.querySelector(".mono")?.textContent);
    expect(calls).toEqual(["W1AW", "N1CCK"]);
  });

  it("renders the connection status and the read-only operating frequency", async () => {
    const fetchMock = vi.fn().mockResolvedValue(jsonResponse(200, summary()));
    renderPage(fetchMock);

    await screen.findByText("W1AW");
    // Initial connection is catching-up until the live stream confirms.
    expect(
      screen.getByText(connectionPresentation("catching-up").label),
    ).toBeInTheDocument();
    // 14_250_000 Hz displayed as MHz. Anchored to the HEADER's ways-in line
    // (`sessionWaysIn`, "HF 14.250 MHz · …") rather than a bare /14\.25/:
    // A second truthful render of the same frequency sits on this page — the
    // stamp picker's "HF — 14.250 MHz" chip — and a match that cannot tell the
    // two apart fails for the right reason.
    expect(screen.getByText(/^HF 14\.250 MHz/)).toBeInTheDocument();
  });

  it("offers a Close Net control for a live session", async () => {
    const fetchMock = vi.fn().mockResolvedValue(jsonResponse(200, summary()));
    renderPage(fetchMock);

    await screen.findByText("W1AW");
    expect(screen.getByRole("button", { name: /close net/i })).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: /start net/i })).not.toBeInTheDocument();
  });

  it("offers a Start Net control for a closed session instead of Close", async () => {
    const fetchMock = vi
      .fn()
      .mockResolvedValue(jsonResponse(200, summary({ lifecycle: "closed" })));
    renderPage(fetchMock);

    await screen.findByText("W1AW");
    expect(screen.getByRole("button", { name: /start net/i })).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: /close net/i })).not.toBeInTheDocument();
  });

  it("offers a Set frequency control for a live session", async () => {
    const fetchMock = vi.fn().mockResolvedValue(jsonResponse(200, summary()));
    renderPage(fetchMock);

    await screen.findByText("W1AW");
    expect(screen.getByRole("button", { name: /set frequency/i })).toBeInTheDocument();
  });

  it("pins the quick-add row (Save & add new / Save & close) for a live session", async () => {
    const fetchMock = vi.fn().mockResolvedValue(jsonResponse(200, summary()));
    renderPage(fetchMock);

    await screen.findByText("W1AW");
    expect(screen.getByRole("button", { name: /save & add new/i })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: /save & close/i })).toBeInTheDocument();
    expect(screen.getByLabelText(/callsign/i)).toBeInTheDocument();
  });

  it("hides the quick-add row for a closed session", async () => {
    const fetchMock = vi
      .fn()
      .mockResolvedValue(jsonResponse(200, summary({ lifecycle: "closed" })));
    renderPage(fetchMock);

    await screen.findByText("W1AW");
    expect(screen.queryByRole("button", { name: /save & add new/i })).not.toBeInTheDocument();
    expect(screen.queryByLabelText(/callsign/i)).not.toBeInTheDocument();
  });

  it("appends a roster row when a checkin.added delta folds in over the WS", async () => {
    // The roster binds to the folded state, so an authoritative
    // checkin.added delta over the WS appends the row through the shipped
    // WS→reducer→roster path — no manual store poke from the control.
    const sockets: DrivableWebSocket[] = [];
    vi.stubGlobal(
      "WebSocket",
      class extends DrivableWebSocket {
        constructor(url: string) {
          super(url);
          sockets.push(this);
        }
      } as unknown as typeof WebSocket,
    );
    const fetchMock = vi.fn().mockResolvedValue(jsonResponse(200, summary()));
    renderPage(fetchMock);

    await screen.findByText("W1AW");
    expect(screen.queryByText("K2ABC")).not.toBeInTheDocument();

    await waitFor(() => expect(sockets.length).toBeGreaterThan(0));
    const socket = sockets[sockets.length - 1];
    act(() => {
      socket.onopen?.();
      socket.onmessage?.({
        data: JSON.stringify({
          type: "event",
          seq: 4,
          at: "2026-07-16T00:01:00Z",
          kind: "checkin.added",
          payload: { checkInId: uuid(99), callsign: "K2ABC" },
        }),
      } as MessageEvent);
    });

    expect(await screen.findByText("K2ABC")).toBeInTheDocument();
  });

  it("hides the Set frequency control for a closed session", async () => {
    const fetchMock = vi
      .fn()
      .mockResolvedValue(jsonResponse(200, summary({ lifecycle: "closed" })));
    renderPage(fetchMock);

    await screen.findByText("W1AW");
    expect(screen.queryByRole("button", { name: /set frequency/i })).not.toBeInTheDocument();
  });

  it("updates the frequency pill live when a frequency.changed delta folds in", async () => {
    // The pill binds to the folded CONNECTION
    // the delta names, so an authoritative frequency.changed over the WS
    // re-renders it without a manual store poke — proving the shipped
    // WS→reducer→pill path.
    const sockets: DrivableWebSocket[] = [];
    vi.stubGlobal(
      "WebSocket",
      class extends DrivableWebSocket {
        constructor(url: string) {
          super(url);
          sockets.push(this);
        }
      } as unknown as typeof WebSocket,
    );
    const fetchMock = vi.fn().mockResolvedValue(jsonResponse(200, summary()));
    renderPage(fetchMock);

    await screen.findByText("W1AW");
    // Anchored to the header's ways-in line — see the note on the same
    // assertion above (added a second render of this frequency).
    expect(screen.getByText(/^HF 14\.250 MHz/)).toBeInTheDocument();

    await waitFor(() => expect(sockets.length).toBeGreaterThan(0));
    const socket = sockets[sockets.length - 1];
    act(() => {
      socket.onopen?.();
      socket.onmessage?.({
        data: JSON.stringify({
          type: "event",
          seq: 4,
          at: "2026-07-16T00:01:00Z",
          kind: "frequency.changed",
          payload: {
            connectionId: "00000000-0000-0000-0000-000000001604",
            operatingFrequencyHz: 7_200_000,
          },
        }),
      } as MessageEvent);
    });

    expect(await screen.findByText(/^HF 7\.200 MHz/)).toBeInTheDocument();
    expect(screen.queryByText(/14\.250 MHz/)).not.toBeInTheDocument();
    expect(screen.queryByText(/14\.230 MHz/)).not.toBeInTheDocument();
  });

  it("redirects a signed-out visitor to /sign-in (self-gating on 401)", async () => {
    const fetchMock = vi.fn().mockResolvedValue(jsonResponse(401, {}));
    renderPage(fetchMock);

    expect(await screen.findByTestId("sign-in")).toBeInTheDocument();
  });

  it("shows the mapped problem message in an alert on a non-owner 403", async () => {
    const fetchMock = vi
      .fn()
      .mockResolvedValue(jsonResponse(403, { type: "/errors/forbidden", status: 403 }));
    renderPage(fetchMock);

    const alert = await screen.findByRole("alert");
    expect(alert).toHaveTextContent(messageForProblemType("/errors/forbidden"));
  });

  it("fully resets when navigating to a different session id on the same route", async () => {
    // Mirrors Start Net's re-run flow: the SAME `/net-sessions/:id` route
    // renders a new id after navigation, reusing the route's element — the
    // previous session's store/roster must not bleed through.
    const fetchMock = vi.fn((input: RequestInfo | URL) => {
      const url = typeof input === "string" ? input : input.toString();
      if (url.includes(uuid(2))) {
        return Promise.resolve(
          jsonResponse(
            200,
            summary({
              id: uuid(2),
              roster: [
                { checkInId: uuid(99), callsign: "K2ABC", addedAt: "t", addedBy: uuid(200) },
              ],
              participantCount: 1,
            }),
          ),
        );
      }
      return Promise.resolve(jsonResponse(200, summary()));
    });
    const router = renderPage(fetchMock);
    await screen.findByText("W1AW");

    await router.navigate(`/net-sessions/${uuid(2)}`);

    await screen.findByText("K2ABC");
    expect(screen.queryByText("W1AW")).not.toBeInTheDocument();
    expect(screen.queryByText("N1CCK")).not.toBeInTheDocument();
  });

  it("renders an optimistic pending row on submit and reconciles it on the matching echo (no duplicate)", async () => {
    // The quick-add is the first live caller of the optimistic map. A
    // submit renders a dimmed pending row immediately; the authoritative
    // checkin.added echoing the SAME clientEventId folds it into a single
    // authoritative row via the shipped WS→reducer→reconcilePending path.
    const user = userEvent.setup();
    const sockets: DrivableWebSocket[] = [];
    vi.stubGlobal(
      "WebSocket",
      class extends DrivableWebSocket {
        constructor(url: string) {
          super(url);
          sockets.push(this);
        }
      } as unknown as typeof WebSocket,
    );
    // A fresh Response per call: the snapshot GET and the check-ins POST each
    // read a body, and a Response body can only be consumed once.
    const fetchMock = vi
      .fn()
      .mockImplementation(() => Promise.resolve(jsonResponse(200, summary({ roster: [] }))));
    renderPage(fetchMock);

    await screen.findByLabelText(/callsign/i);
    await user.type(screen.getByLabelText(/callsign/i), "K2ABC");
    await user.click(screen.getByRole("button", { name: /save & add new/i }));

    // The dimmed optimistic row appears before any echo.
    const optimistic = await screen.findByText("K2ABC");
    expect(optimistic.closest("li")?.getAttribute("data-optimistic")).toBe("true");

    // The clientEventId the row was minted with is the one the POST carried.
    const postCall = fetchMock.mock.calls.find(([url]) =>
      String(url).endsWith("/check-ins"),
    ) as [string, RequestInit];
    const clientEventId = (JSON.parse(postCall[1].body as string) as { clientEventId: string })
      .clientEventId;
    expect(clientEventId).toBeTruthy();

    await waitFor(() => expect(sockets.length).toBeGreaterThan(0));
    const socket = sockets[sockets.length - 1];
    act(() => {
      socket.onopen?.();
      socket.onmessage?.({
        data: JSON.stringify({
          type: "event",
          seq: 4,
          at: "2026-07-16T00:01:00Z",
          kind: "checkin.added",
          payload: { checkInId: uuid(99), callsign: "K2ABC", clientEventId },
        }),
      } as MessageEvent);
    });

    // Reconciled to exactly one authoritative row: the account-less callsign
    // stands in the roster.
    await waitFor(() => {
      expect(
        screen.getByText("K2ABC").closest("li")?.getAttribute("data-optimistic"),
      ).toBe("false");
    });
    expect(screen.getAllByText("K2ABC")).toHaveLength(1);
  });

  it("shows a Staff-entered source badge on every operator roster row", async () => {
    // Each staff-entered check-in carries the amber
    // Staff badge on the operator console.
    const fetchMock = vi.fn().mockResolvedValue(jsonResponse(200, summary()));
    renderPage(fetchMock);

    await screen.findByText("W1AW");
    const badges = document.querySelectorAll('[data-source="staff"]');
    expect(badges.length).toBe(2); // W1AW + N1CCK
  });

  it("exposes the n/w keycap legend and an aria-live roster region (a11y floor)", async () => {
    // Both n and w discoverable; the roster region announces updates. The
    // `w` keycap now also appears on each roster row (the inline set-working
    // control), so scope the legend assertion to the legend group.
    const fetchMock = vi.fn().mockResolvedValue(jsonResponse(200, summary()));
    renderPage(fetchMock);

    await screen.findByText("W1AW");
    const legend = screen.getByRole("group", { name: /keyboard shortcuts/i });
    expect(within(legend).getByText("n")).toBeInTheDocument();
    expect(within(legend).getByText("w")).toBeInTheDocument();
    expect(screen.getByRole("list", { name: "Roster" })).toHaveAttribute("aria-live", "polite");
  });

  it("the n hotkey focuses the quick-add callsign from elsewhere on the console", async () => {
    // Pressing n from a non-input focus jumps to the callsign field.
    const user = userEvent.setup();
    const fetchMock = vi.fn().mockResolvedValue(jsonResponse(200, summary()));
    renderPage(fetchMock);

    await screen.findByText("W1AW");
    // Scoped to the keyboard-shortcuts legend — the quick-add's own eyebrow
    // also names the `n` hotkey (design handoff `2b`), but that keycap is a
    // plain, non-focusable hint, not the "elsewhere on the console" focus
    // target this test needs.
    const legend = screen.getByRole("group", { name: /keyboard shortcuts/i });
    const legendKey = within(legend).getByText("n"); // a focusable, non-input element
    legendKey.focus();
    await user.keyboard("n");

    expect(document.activeElement).toBe(screen.getByLabelText(/callsign/i));
  });

  it("the w hotkey sets the selected roster row as the working station", async () => {
    // Select a row (click), press `w`, and the console POSTs the cursor
    // move to the worked-station endpoint with that row's checkInId.
    const user = userEvent.setup();
    const fetchMock = vi
      .fn()
      .mockImplementation(() => Promise.resolve(jsonResponse(200, summary())));
    renderPage(fetchMock);

    await screen.findByText("W1AW");
    const row = screen.getByText("W1AW").closest("li") as HTMLElement;
    await user.click(row);
    await user.keyboard("w");

    await waitFor(() => {
      const call = fetchMock.mock.calls.find(([url]) =>
        String(url).endsWith("/worked-station"),
      ) as [string, RequestInit] | undefined;
      expect(call).toBeTruthy();
      expect(JSON.parse(call![1].body as string)).toEqual({ checkInId: uuid(42) });
    });
  });

  it("ignores OS auto-repeat keydown events on the w hotkey", async () => {
    // Holding `w` down fires a stream of keydown events with `repeat: true`
    // after the first. Unlike the `n` hotkey (which just moves focus and is
    // harmless to re-fire), `w` triggers a network mutation each time — a
    // held key must not flood `/worked-station` with overlapping toggles.
    const user = userEvent.setup();
    const fetchMock = vi
      .fn()
      .mockImplementation(() => Promise.resolve(jsonResponse(200, summary())));
    renderPage(fetchMock);

    await screen.findByText("W1AW");
    await user.click(screen.getByText("W1AW").closest("li") as HTMLElement);

    fireEvent.keyDown(document, { key: "w", repeat: false });
    fireEvent.keyDown(document, { key: "w", repeat: true });
    fireEvent.keyDown(document, { key: "w", repeat: true });

    await waitFor(() => {
      const calls = fetchMock.mock.calls.filter(([url]) => String(url).endsWith("/worked-station"));
      expect(calls).toHaveLength(1);
    });
  });

  it("ignores a second w-hotkey press while the first toggle is still in flight", async () => {
    // A fast double real (non-repeat) press before the first request settles
    // must not fire a second overlapping POST — the second press races the
    // first's still-stale `session.workingCheckInId` closure value and could
    // flip-flop the cursor.
    const user = userEvent.setup();
    let resolveFirst: ((value: Response) => void) | undefined;
    const fetchMock = vi.fn().mockImplementation((url: unknown) => {
      if (String(url).endsWith("/worked-station")) {
        return new Promise<Response>((resolve) => {
          resolveFirst = resolve;
        });
      }
      return Promise.resolve(jsonResponse(200, summary()));
    });
    renderPage(fetchMock);

    await screen.findByText("W1AW");
    await user.click(screen.getByText("W1AW").closest("li") as HTMLElement);

    fireEvent.keyDown(document, { key: "w", repeat: false });
    fireEvent.keyDown(document, { key: "w", repeat: false });

    const calls = fetchMock.mock.calls.filter(([url]) => String(url).endsWith("/worked-station"));
    expect(calls).toHaveLength(1);

    resolveFirst?.(jsonResponse(200, summary()));
    await waitFor(() => expect(resolveFirst).toBeDefined());
  });

  it("the w hotkey is inert while typing in a field (guarded like n)", async () => {
    const user = userEvent.setup();
    const fetchMock = vi
      .fn()
      .mockImplementation(() => Promise.resolve(jsonResponse(200, summary())));
    renderPage(fetchMock);

    await screen.findByText("W1AW");
    // Select a row, then focus the callsign field and type — the `w` must not fire.
    await user.click(screen.getByText("W1AW").closest("li") as HTMLElement);
    await user.click(screen.getByLabelText(/callsign/i));
    await user.keyboard("w");

    const workedCall = fetchMock.mock.calls.find(([url]) =>
      String(url).endsWith("/worked-station"),
    );
    expect(workedCall).toBeUndefined();
  });

  it("clears the w-hotkey selection when the selected row is removed from the roster (review fix)", async () => {
    // Review finding: selectedCheckInId was never invalidated when its row
    // disappeared (checkin.removed), so a later `w` press would silently
    // 404 against a check-in that no longer exists. Selecting the row, then
    // removing it via an authoritative delta, must clear the selection.
    const user = userEvent.setup();
    const sockets: DrivableWebSocket[] = [];
    vi.stubGlobal(
      "WebSocket",
      class extends DrivableWebSocket {
        constructor(url: string) {
          super(url);
          sockets.push(this);
        }
      } as unknown as typeof WebSocket,
    );
    const fetchMock = vi
      .fn()
      .mockImplementation(() => Promise.resolve(jsonResponse(200, summary())));
    renderPage(fetchMock);

    await screen.findByText("W1AW");
    await user.click(screen.getByText("W1AW").closest("li") as HTMLElement);

    await waitFor(() => expect(sockets.length).toBeGreaterThan(0));
    const socket = sockets[sockets.length - 1];
    act(() => {
      socket.onopen?.();
      socket.onmessage?.({
        data: JSON.stringify({
          type: "event",
          seq: 4,
          at: "2026-07-16T00:01:00Z",
          kind: "checkin.removed",
          payload: { checkInId: uuid(42) },
        }),
      } as MessageEvent);
    });
    await waitFor(() => expect(screen.queryByText("W1AW")).not.toBeInTheDocument());

    fetchMock.mockClear();
    await user.keyboard("w");

    const workedCall = fetchMock.mock.calls.find(([url]) =>
      String(url).endsWith("/worked-station"),
    );
    expect(workedCall).toBeUndefined();
  });

  it("surfaces a refused worked-station toggle instead of failing silently (review fix)", async () => {
    // Review finding: toggleWorking swallowed every fetch failure (403/404/
    // 409) with a bare `catch {}` — an operator clicking the inline `w`
    // control or pressing the hotkey saw nothing happen at all. It must
    // surface an alert, like every other write path added in this story.
    const user = userEvent.setup();
    const fetchMock = vi.fn().mockImplementation((url: unknown) => {
      if (String(url).endsWith("/worked-station")) {
        return Promise.resolve(
          jsonResponse(403, { type: "/errors/forbidden", status: 403 }),
        );
      }
      return Promise.resolve(jsonResponse(200, summary()));
    });
    renderPage(fetchMock);

    await screen.findByText("W1AW");
    await user.click(screen.getByText("W1AW").closest("li") as HTMLElement);
    await user.keyboard("w");

    expect(await screen.findByRole("alert")).toHaveTextContent(
      messageForProblemType("/errors/forbidden"),
    );
  });

  it("renders the net-level note panel on a live session", async () => {
    const fetchMock = vi
      .fn()
      .mockResolvedValue(jsonResponse(200, summary({ netNote: "Weekly traffic net" })));
    renderPage(fetchMock);

    await screen.findByText("W1AW");
    const notes = screen.getByLabelText(/net note/i) as HTMLTextAreaElement;
    expect(notes.value).toBe("Weekly traffic net");
  });

  it("has no WCAG 2.1 AA violations on the loaded page", async () => {
    const fetchMock = vi.fn().mockResolvedValue(jsonResponse(200, summary()));
    renderPage(fetchMock);

    await screen.findByText("W1AW");
    await expectNoAxeViolations(document.body);
  });

  it("wraps the console body in the shared Card chrome", async () => {
    const fetchMock = vi.fn().mockResolvedValue(jsonResponse(200, summary()));
    renderPage(fetchMock);

    await screen.findByText("W1AW");
    const card = document.querySelector('[style*="var(--rounded-xl)"]');
    expect(card).not.toBeNull();
    expect(card?.contains(screen.getByRole("list", { name: "Roster" }))).toBe(true);
  });

  it("contains the roster's fixed-width columns inside a scrollable ResponsiveList, not the page body", async () => {
    // Regression: the roster/quick-add's fixed-width columns were rendered
    // directly in the page body with no horizontal-scroll containment,
    // blowing the whole page out sideways on a normal desktop window
    // instead of scrolling inside their own container.
    const fetchMock = vi.fn().mockResolvedValue(jsonResponse(200, summary()));
    renderPage(fetchMock);

    await screen.findByText("W1AW");
    const lists = screen.getAllByTestId("responsive-list");
    expect(lists.length).toBeGreaterThan(0);
    expect(
      lists.some((el) => el.contains(screen.getByRole("list", { name: "Roster" }))),
    ).toBe(true);
  });

  it("renders a session-header card: eyebrow, freq/band/mode, Live badge, Elapsed + Checked-in stats", async () => {
    const fetchMock = vi.fn().mockResolvedValue(
      jsonResponse(200, summary({ startedAt: "2026-07-16T00:00:00Z" })),
    );
    renderPage(fetchMock);

    await screen.findByText("W1AW");
    const eyebrow = screen.getByText(/you are ncs/i);
    expect(eyebrow).toBeInTheDocument();
    // Scope the band/mode assertion to the header card — the quick-add's Report
    // field label ("Report · SSB") also matches /ssb/i elsewhere on the page.
    const header = eyebrow.closest("div")?.parentElement as HTMLElement;
    expect(within(header).getByText(/ssb/i).textContent).toContain("20m");
    expect(screen.getByText("Elapsed")).toBeInTheDocument();
    const checkedInStat = screen.getByText("Checked in");
    expect(checkedInStat.parentElement?.textContent).toContain("2");
  });

  it("heads the live roster with the operator column strip", async () => {
    const fetchMock = vi.fn().mockResolvedValue(jsonResponse(200, summary()));
    renderPage(fetchMock);

    await screen.findByText("W1AW");
    const heads = screen.getByTestId("roster-column-heads");

    // The live console is the dense operator surface (source + precedence).
    expect(heads).toHaveTextContent(/station/i);
    expect(heads).toHaveTextContent(/source/i);
    expect(heads).toHaveTextContent(/precedence/i);
    // The strip labels the roster, so it must precede it.
    const roster = screen.getByRole("list", { name: /roster/i });
    expect(
      heads.compareDocumentPosition(roster) & Node.DOCUMENT_POSITION_FOLLOWING,
    ).toBeTruthy();
  });

  it("omits the column strip when the roster has no rows to label", async () => {
    const fetchMock = vi
      .fn()
      .mockResolvedValue(jsonResponse(200, summary({ roster: [], participantCount: 0 })));
    renderPage(fetchMock);

    await waitFor(() => {
      expect(screen.getByRole("list", { name: /roster/i })).toBeInTheDocument();
    });
    expect(screen.queryByTestId("roster-column-heads")).not.toBeInTheDocument();
  });

  it("frames the console in an elevated panel on the shared content measure", async () => {
    const fetchMock = vi.fn().mockResolvedValue(jsonResponse(200, summary()));
    renderPage(fetchMock);

    await screen.findByText("W1AW");
    const page = screen.getByRole("main");
    expect(page).toHaveStyle({ maxWidth: "1200px" });

    const panel = screen.getByText(/you are ncs/i).closest("section");
    expect(panel).toHaveStyle({
      background: "var(--surface)",
      boxShadow: "var(--shadow)",
      overflow: "hidden",
    });
  });

  it("bleeds the session header flush to the panel edge on the tonal gradient", async () => {
    const fetchMock = vi.fn().mockResolvedValue(jsonResponse(200, summary()));
    renderPage(fetchMock);

    await screen.findByText("W1AW");
    const header = screen
      .getByText(/you are ncs/i)
      .closest("[data-session-header]") as HTMLElement;

    expect(header).toHaveStyle({ background: "var(--head-grad)" });
    // The old frame padded the whole body and then cancelled it with negative
    // margins on the header; the panel clips instead, so no compensation is
    // needed — and a stray negative margin would now pull the band outside it.
    expect(header.style.margin).toBe("");
  });

  it("merges Set-frequency and the keycap legend into one operator toolbar strip (no Round/Pause per locked decision)", async () => {
    const fetchMock = vi.fn().mockResolvedValue(jsonResponse(200, summary()));
    renderPage(fetchMock);

    await screen.findByText("W1AW");
    const freqButton = screen.getByRole("button", { name: /set frequency/i });
    const legend = screen.getByRole("group", { name: /keyboard shortcuts/i });
    const toolbar = freqButton.closest("[data-toolbar]");
    expect(toolbar).not.toBeNull();
    expect(legend.closest("[data-toolbar]")).toBe(toolbar);
    // Locked decision: neither a manual Pause-net button nor a Round-N/roll-call
    // chip exists anywhere — net-paused is derived from presence, and there is
    // no roll-call-round tracking in this codebase.
    expect(screen.queryByRole("button", { name: /pause net/i })).not.toBeInTheDocument();
    expect(screen.queryByText(/round\s*\d/i)).not.toBeInTheDocument();
  });

  it("carries the way-in stamp in that same strip, stating what is in force", async () => {
    // Visibility is what makes an in-memory stamp acceptable
    // at all: after a reload it says, truthfully, that nothing is stamped —
    // so the operator never answers "what is this recording?" from memory.
    const fetchMock = vi.fn().mockResolvedValue(jsonResponse(200, summary()));
    renderPage(fetchMock);

    await screen.findByText("W1AW");
    const legend = screen.getByRole("group", { name: /keyboard shortcuts/i });
    const status = screen.getByRole("status", { name: /taking check-ins on/i });
    expect(status.closest("[data-toolbar]")).toBe(legend.closest("[data-toolbar]"));
    expect(status).toHaveAttribute("data-via-stamp", "none");
  });

  it("renders a wayfinding breadcrumb trail ending at the session title", async () => {
    const fetchMock = vi.fn().mockResolvedValue(jsonResponse(200, summary()));
    renderPage(fetchMock);

    await screen.findByText("W1AW");
    const nav = screen.getByRole("navigation", { name: /breadcrumb/i });
    expect(within(nav).getByText("Nets")).toBeInTheDocument();
    expect(within(nav).getByText(/sunday traffic net/i)).toBeInTheDocument();
  });

  // --- Relay add-only console render gate -------------------

  it("shows a granted relay the quick-add but hides edit/reorder/worked/net-note/frequency/close/role-management", async () => {
    const fetchMock = vi
      .fn()
      .mockResolvedValue(
        jsonResponse(200, summary({ viewerRole: "relay", netNote: "Weekly traffic net" })),
      );
    renderPage(fetchMock);

    await screen.findByText("W1AW");
    // PRESENT for a relay: the quick-add (LogCheckIn).
    expect(screen.getByRole("button", { name: /save & add new/i })).toBeInTheDocument();
    expect(screen.getByLabelText(/callsign/i)).toBeInTheDocument();

    // ABSENT for a relay (capabilities it does not hold):
    expect(screen.queryByRole("button", { name: /order by precedence/i })).not.toBeInTheDocument();
    expect(screen.queryByLabelText(/net note/i)).not.toBeInTheDocument();
    expect(screen.queryByRole("button", { name: /set frequency/i })).not.toBeInTheDocument();
    expect(screen.queryByRole("button", { name: /close net/i })).not.toBeInTheDocument();
    expect(screen.queryByLabelText("Role management")).not.toBeInTheDocument();
    // The per-row Edit affordance (EditCheckIn = Logger+) is absent.
    expect(screen.queryByRole("button", { name: /edit/i })).not.toBeInTheDocument();
    // The per-row set-working control (SetWorkedStation = NCS) is absent.
    expect(
      screen.queryByRole("button", { name: /set working|mark working|working/i }),
    ).not.toBeInTheDocument();
  });

  it("hides the staff-only Report field in a relay's quick-add (EditStaffFields)", async () => {
    const fetchMock = vi
      .fn()
      .mockResolvedValue(jsonResponse(200, summary({ viewerRole: "relay" })));
    renderPage(fetchMock);

    await screen.findByLabelText(/callsign/i);
    // Callsign/Name/Location remain; the Report input (mode-shaped, staff-only)
    // is gone for a relay.
    expect(screen.getByLabelText(/name/i)).toBeInTheDocument();
    expect(screen.queryByLabelText(/signal report|rst|report/i)).not.toBeInTheDocument();
  });

  it("the w hotkey is inert for a relay (SetWorkedStation is NCS-only)", async () => {
    const user = userEvent.setup();
    const fetchMock = vi
      .fn()
      .mockImplementation(() => Promise.resolve(jsonResponse(200, summary({ viewerRole: "relay" }))));
    renderPage(fetchMock);

    await screen.findByText("W1AW");
    // The row offers no selection affordance for a relay; pressing w does nothing.
    await user.click(screen.getByText("W1AW").closest("li") as HTMLElement);
    await user.keyboard("w");

    const workedCall = fetchMock.mock.calls.find(([url]) =>
      String(url).endsWith("/worked-station"),
    );
    expect(workedCall).toBeUndefined();
  });

  it("shows the active NCS a Hand off control to an eligible target and posts the handoff", async () => {
    // HandoffControl existed but was never wired into
    // this page — voluntary handoff was unreachable from the UI. This proves
    // the affordance renders for the CURRENT active NCS and invokes the
    // endpoint with the picked target.
    const user = userEvent.setup();
    const account = {
      id: uuid(200),
      email: "nc@example.com",
      emailVerifiedAt: "2026-07-16T00:00:00Z",
      consentRequired: false,
      requiredTermsVersion: "1",
      callsign: "W1AW",
    };
    const fetchMock = vi.fn().mockImplementation((url: unknown) => {
      if (String(url).endsWith("/handoff")) {
        return Promise.resolve(
          jsonResponse(
            200,
            summary({
              viewerRole: "owner",
              controlState: "active",
              activeNcsAccountId: uuid(300),
            }),
          ),
        );
      }
      return Promise.resolve(
        jsonResponse(
          200,
          summary({
            viewerRole: "owner",
            controlState: "active",
            activeNcsAccountId: uuid(200),
          }),
        ),
      );
    });
    renderPage(fetchMock, {
      account,
      roles: [
        {
          accountId: uuid(300),
          callsign: "N1CCK",
          role: "net-control",
          grantedBy: uuid(200),
          grantedAt: "t",
        },
      ],
    });

    await screen.findByText("W1AW");
    await user.click(await screen.findByRole("button", { name: /hand off/i }));

    const handoffCall = fetchMock.mock.calls.find(([url]) =>
      String(url).endsWith("/handoff"),
    );
    expect(handoffCall).toBeDefined();
    const [, init] = handoffCall as [string, RequestInit];
    expect(JSON.parse(init.body as string)).toEqual({ targetAccountId: uuid(300) });
  });

  it("hides the Hand off control from a viewer who is NOT the active NCS", async () => {
    const account = {
      id: uuid(999),
      email: "other@example.com",
      emailVerifiedAt: "2026-07-16T00:00:00Z",
      consentRequired: false,
      requiredTermsVersion: "1",
      callsign: "N1ALE",
    };
    const fetchMock = vi.fn().mockResolvedValue(
      jsonResponse(
        200,
        summary({ viewerRole: "owner", controlState: "active", activeNcsAccountId: uuid(200) }),
      ),
    );
    renderPage(fetchMock, {
      account,
      roles: [
        {
          accountId: uuid(300),
          callsign: "N1CCK",
          role: "net-control",
          grantedBy: uuid(200),
          grantedAt: "t",
        },
      ],
    });

    await screen.findByText("W1AW");
    expect(screen.queryByRole("button", { name: /hand off/i })).not.toBeInTheDocument();
  });

  it("shows an NCS the role-management panel", async () => {
    // renderPage defaults GET …/roles to an empty grants list.
    const fetchMock = vi
      .fn()
      .mockResolvedValue(jsonResponse(200, summary({ viewerRole: "net-control" })));
    renderPage(fetchMock);

    await screen.findByText("W1AW");
    expect(await screen.findByLabelText("Role management")).toBeInTheDocument();
    //...and a relay-entered row still shows the Staff-entered badge, NOT a new
    // variant.
    expect(document.querySelectorAll('[data-source="staff"]').length).toBe(2);
  });

  // --- CSV/ADIF export affordance (Task 6) ----------------------

  it("offers CSV and ADIF download links to an owner on a CLOSED session", async () => {
    const fetchMock = vi.fn().mockResolvedValue(
      jsonResponse(
        200,
        summary({
          viewerRole: "owner",
          lifecycle: "closed",
          closedAt: "2026-07-16T01:00:00Z",
          durationSeconds: 3600,
        }),
      ),
    );
    renderPage(fetchMock);

    await screen.findByText("W1AW");
    const csv = await screen.findByRole("link", { name: /download csv/i });
    const adif = await screen.findByRole("link", { name: /download adif/i });
    // The links point at the same-origin export endpoint per format; a plain
    // `<a download href>` navigation carries the session cookie automatically.
    expect(csv).toHaveAttribute(
      "href",
      `/api/net-sessions/${uuid(1)}/export?format=csv`,
    );
    expect(adif).toHaveAttribute(
      "href",
      `/api/net-sessions/${uuid(1)}/export?format=adif`,
    );
  });

  it("hides the download links from a Logger even on a closed session", async () => {
    const fetchMock = vi.fn().mockResolvedValue(
      jsonResponse(
        200,
        summary({
          viewerRole: "logger",
          lifecycle: "closed",
          closedAt: "2026-07-16T01:00:00Z",
          durationSeconds: 3600,
        }),
      ),
    );
    renderPage(fetchMock);

    await screen.findByText("W1AW");
    expect(
      screen.queryByRole("link", { name: /download csv/i }),
    ).not.toBeInTheDocument();
    expect(
      screen.queryByRole("link", { name: /download adif/i }),
    ).not.toBeInTheDocument();
  });

  it("does not offer the download links on a LIVE session, even to an owner", async () => {
    const fetchMock = vi
      .fn()
      .mockResolvedValue(jsonResponse(200, summary({ viewerRole: "owner", lifecycle: "live" })));
    renderPage(fetchMock);

    await screen.findByText("W1AW");
    expect(
      screen.queryByRole("link", { name: /download csv/i }),
    ).not.toBeInTheDocument();
    expect(
      screen.queryByRole("link", { name: /download adif/i }),
    ).not.toBeInTheDocument();
  });

  // --- Post-net summary (closed lifecycle) distinct visual state ------------

  describe("post-net summary (closed lifecycle)", () => {
    function closedSummary(overrides: Record<string, unknown> = {}) {
      return summary({
        viewerRole: "owner",
        lifecycle: "closed",
        startedAt: "2026-07-16T00:00:00Z",
        closedAt: "2026-07-16T01:31:00Z",
        durationSeconds: 91 * 60,
        activeNcsAccountId: uuid(200),
        roster: [
          {
            checkInId: uuid(42),
            callsign: "W1AW",
            addedAt: "2026-07-16T00:00:00Z",
            addedBy: uuid(200),
            source: "staff",
            signalReport: "599",
            staying: "staying-for-comments",
            precedence: "priority",
            traffic: 2,
            publicNote: "relaying for the county EOC",
            worked: true,
            version: 1,
            corrections: [],
          },
          {
            checkInId: uuid(43),
            callsign: "N1CCK",
            addedAt: "2026-07-16T00:05:00Z",
            addedBy: uuid(201),
            source: "self",
            signalReport: "579",
            staying: "in-and-out",
            precedence: "routine",
            traffic: 1,
            worked: true,
            version: 1,
            corrections: [],
          },
        ],
        ...overrides,
      });
    }

    it("shows a distinct 'Net closed · logged' eyebrow with a closed-session meta line", async () => {
      const fetchMock = vi.fn().mockResolvedValue(jsonResponse(200, closedSummary()));
      renderPage(fetchMock);

      await screen.findAllByText("W1AW");
      expect(screen.getByText(/net closed · logged/i)).toBeInTheDocument();
      expect(screen.queryByText(/you are ncs/i)).not.toBeInTheDocument();
      // The meta line carries the NCS callsign (derived from the roster entry
      // whose addedBy matches activeNcsAccountId) and the freq/band/mode.
      const meta = document.querySelector("p")?.textContent ?? "";
      expect(meta).toContain("W1AW");
      expect(meta).toContain("20m");
    });

    it("renders a StatTile row: Check-ins, Traffic passed, Duration (mono), States/provinces", async () => {
      const fetchMock = vi.fn().mockResolvedValue(jsonResponse(200, closedSummary()));
      renderPage(fetchMock);

      await screen.findAllByText("W1AW");
      const checkIns = screen.getByText("Check-ins");
      expect(checkIns.previousElementSibling?.textContent).toBe("2");
      const traffic = screen.getByText("Traffic passed");
      expect(traffic.previousElementSibling?.textContent).toBe("3"); // 2 + 1
      const duration = screen.getByText("Duration");
      expect(duration.previousElementSibling?.textContent).toBe("01:31");
      expect(screen.getByText("States / provinces")).toBeInTheDocument();
    });

    it("promotes CSV/ADIF export to header buttons with icons", async () => {
      const fetchMock = vi.fn().mockResolvedValue(jsonResponse(200, closedSummary()));
      renderPage(fetchMock);

      await screen.findAllByText("W1AW");
      const csv = screen.getByRole("link", { name: /download csv/i });
      const adif = screen.getByRole("link", { name: /download adif/i });
      expect(csv.querySelector("svg")).not.toBeNull();
      expect(adif.querySelector("svg")).not.toBeNull();
    });

    it("shows staying on the CLOSED post-net roster, wherever report and precedence are shown", async () => {
      // "Uniform" has to include the surface nobody demoes. The closed summary
      // already opted into `showReport`/`showPrecedence` but not staying, which
      // would leave it the one roster in the product hiding a field every other
      // roster shows. Staying is passed wherever report and precedence are.
      const fetchMock = vi.fn().mockResolvedValue(jsonResponse(200, closedSummary()));
      renderPage(fetchMock);

      await screen.findAllByText("W1AW");
      expect(document.querySelector("[data-staying]")).not.toBeNull();
    });

    it("shows Precedence and an absolute Checked time instead of Heard on the post-net roster", async () => {
      const fetchMock = vi.fn().mockResolvedValue(jsonResponse(200, closedSummary()));
      renderPage(fetchMock);

      await screen.findAllByText("W1AW");
      expect(screen.getByText("Priority").closest("[data-precedence]")).not.toBeNull();
      // Absolute local time, not a relative "Xm/h/d ago" phrase.
      expect(screen.queryByText(/ago|in \d/i)).not.toBeInTheDocument();
      expect(
        screen.getByText(new Date("2026-07-16T00:00:00Z").toLocaleString()),
      ).toBeInTheDocument();
    });
  });

  it("prefers the server's problem detail over the slug-map fallback", async () => {
    // One of forty-two surfaces. This page already ordered
    // `detail` ahead of the map on its load-failure alert; the test pins the
    // ORDER now that a shared resolver owns it.
    const detail = "the field-naming answer the server sent";
    const fetchMock = vi.fn().mockResolvedValue(
      jsonResponse(403, { type: "/errors/forbidden", status: 403, detail }),
    );
    renderPage(fetchMock);

    const alert = await screen.findByRole("alert");
    expect(alert).toHaveTextContent(detail);
    expect(alert).not.toHaveTextContent(
      messageForProblemType("/errors/forbidden"),
    );
  });

});

describe("LiveSessionPage — the worked-station sink", () => {
  function sunkSummary(overrides: Record<string, unknown> = {}) {
    // The SERVER has already sunk W1AW and N1CCK below K1XYZ — the page renders
    // the shared order it was handed and groups the worked block for collapse.
    return summary({
      rosterOrderMode: "worked-sink",
      workingCheckInId: uuid(44),
      roster: [
        { checkInId: uuid(44), callsign: "K1XYZ", addedAt: "t", addedBy: uuid(200), worked: false },
        { checkInId: uuid(45), callsign: "W2ABC", addedAt: "t", addedBy: uuid(200), worked: false },
        { checkInId: uuid(42), callsign: "W1AW", addedAt: "t", addedBy: uuid(200), worked: true },
        { checkInId: uuid(43), callsign: "N1CCK", addedAt: "t", addedBy: uuid(200), worked: true },
      ],
      ...overrides,
    });
  }

  it("offers the NCS a worked-sink toggle that POSTs the mode and seeds the summary", async () => {
    const user = userEvent.setup();
    // The session starts in the legacy mode; only the mode POST answers with the
    // sink on, so the switch's state is driven by the SERVER's summary.
    const fetchMock = vi.fn().mockImplementation((url: unknown) =>
      Promise.resolve(
        String(url).endsWith("/roster-order-mode")
          ? jsonResponse(200, summary({ rosterOrderMode: "worked-sink" }))
          : jsonResponse(200, summary()),
      ),
    );
    renderPage(fetchMock);

    await screen.findByText("W1AW");
    await user.click(screen.getByRole("switch", { name: /worked/i }));

    const call = fetchMock.mock.calls.find(([url]) =>
      String(url).endsWith("/roster-order-mode"),
    );
    expect(call).toBeDefined();
    const [, init] = call as [string, RequestInit];
    expect(init.method).toBe("POST");
    expect(JSON.parse(init.body as string)).toEqual({ mode: "worked-sink" });
    await waitFor(() =>
      expect(screen.getByRole("switch", { name: /worked/i })).toHaveAttribute(
        "aria-checked",
        "true",
      ),
    );
  });

  it("collapses the worked stations behind a count and leaves the unworked rows in the shared order", async () => {
    const fetchMock = vi.fn().mockResolvedValue(jsonResponse(200, sunkSummary()));
    renderPage(fetchMock);

    await screen.findByText("K1XYZ");
    const list = screen.getByRole("list", { name: "Roster" });
    // `data-callsign` is the row-identity face; a plain `.mono` query would also
    // match inside the collapsed group's container once it is expanded.
    const visible = Array.from(list.querySelectorAll("[data-callsign]")).map(
      (el) => el.textContent,
    );
    expect(visible).toEqual(["K1XYZ", "W2ABC"]);

    const disclosure = within(list).getByRole("button", { expanded: false });
    expect(disclosure.textContent).toMatch(/\b2\b/);
  });

  it("expanding the worked group reveals the rows and issues NO request", async () => {
    const user = userEvent.setup();
    const fetchMock = vi.fn().mockResolvedValue(jsonResponse(200, sunkSummary()));
    renderPage(fetchMock);

    await screen.findByText("K1XYZ");
    const list = screen.getByRole("list", { name: "Roster" });
    const before = fetchMock.mock.calls.length;
    await user.click(within(list).getByRole("button", { expanded: false }));

    const visible = Array.from(list.querySelectorAll("[data-callsign]")).map(
      (el) => el.textContent,
    );
    expect(visible).toEqual(["K1XYZ", "W2ABC", "W1AW", "N1CCK"]);
    expect(fetchMock.mock.calls.length).toBe(before);
  });

  it("renders a flat roster with the mode off, worked rows in place", async () => {
    // The regression contract: with the mode off nothing groups and nothing
    // moves — the page must not partition an order the server did not sink.
    const fetchMock = vi.fn().mockResolvedValue(
      jsonResponse(
        200,
        summary({
          roster: [
            { checkInId: uuid(42), callsign: "W1AW", addedAt: "t", addedBy: uuid(200), worked: true },
            { checkInId: uuid(43), callsign: "N1CCK", addedAt: "t", addedBy: uuid(200), worked: false },
          ],
        }),
      ),
    );
    renderPage(fetchMock);

    await screen.findByText("W1AW");
    const list = screen.getByRole("list", { name: "Roster" });
    expect(
      within(list)
        .getAllByRole("listitem")
        .map((li) => li.querySelector(".mono")?.textContent),
    ).toEqual(["W1AW", "N1CCK"]);
    expect(within(list).queryByRole("button", { expanded: false })).not.toBeInTheDocument();
  });

  it("has no WCAG 2.1 AA violations with the worked group collapsed or expanded", async () => {
    // The fixture seeds the newest cells — a
    // labelled traffic count and a per-station public note, on rows in both the
    // unworked and the collapsible worked group — so the axe pass actually runs
    // over the new surface rather than over a roster that happens not to have
    // any. Without the seed this gate was green on rows that carried none of it.
    const user = userEvent.setup();
    const fetchMock = vi.fn().mockResolvedValue(
      jsonResponse(
        200,
        sunkSummary({
          roster: [
            {
              checkInId: uuid(44),
              callsign: "K1XYZ",
              addedAt: "t",
              addedBy: uuid(200),
              worked: false,
              staying: "staying-for-comments",
              precedence: "emergency",
              traffic: 3,
              publicNote: "relaying for the county EOC",
            },
            {
              checkInId: uuid(45),
              callsign: "W2ABC",
              addedAt: "t",
              addedBy: uuid(200),
              worked: false,
              precedence: "priority",
              traffic: 1,
            },
            {
              checkInId: uuid(42),
              callsign: "W1AW",
              addedAt: "t",
              addedBy: uuid(200),
              worked: true,
              precedence: "routine",
              traffic: 2,
              publicNote: "holding one piece for W1BBB",
            },
            {
              checkInId: uuid(43),
              callsign: "N1CCK",
              addedAt: "t",
              addedBy: uuid(200),
              worked: true,
            },
          ],
        }),
      ),
    );
    renderPage(fetchMock);

    await screen.findByText("K1XYZ");
    const list = screen.getByRole("list", { name: "Roster" });
    // The new cells are genuinely in the tree axe is about to walk — otherwise
    // this test would pass by rendering nothing new.
    expect(list.querySelector("[data-traffic]")).not.toBeNull();
    expect(list.querySelector("[data-public-note]")).not.toBeNull();
    await expectNoAxeViolations(list);
    await user.click(within(list).getByRole("button", { expanded: false }));
    // Expanding the worked group brings its own new cells into the tree.
    expect(within(list).getByText("W1AW")).toBeInTheDocument();
    await expectNoAxeViolations(list);
  });

  it("keeps the station being worked in the unworked group on a SECOND round", async () => {
    // The round-2 shape. `worked` is monotonic on the server, so once the
    // NCS calls a previously-worked station back, the entry holding the cursor
    // carries `worked: true` while the server keeps it OUT of the sunk block.
    // A client split that reads `worked` alone files the station being worked
    // into the collapsed disclosure — invisible on a default page load.
    const fetchMock = vi.fn().mockResolvedValue(
      jsonResponse(
        200,
        summary({
          rosterOrderMode: "worked-sink",
          workingCheckInId: uuid(42),
          roster: [
            { checkInId: uuid(44), callsign: "K1XYZ", addedAt: "t", addedBy: uuid(200), worked: false },
            { checkInId: uuid(42), callsign: "W1AW", addedAt: "t", addedBy: uuid(200), worked: true },
            { checkInId: uuid(43), callsign: "N1CCK", addedAt: "t", addedBy: uuid(200), worked: true },
          ],
        }),
      ),
    );
    renderPage(fetchMock);

    await screen.findByText("K1XYZ");
    const list = screen.getByRole("list", { name: "Roster" });
    const visible = Array.from(list.querySelectorAll("[data-callsign]")).map(
      (el) => el.textContent,
    );
    expect(visible).toEqual(["K1XYZ", "W1AW"]);
    // Only the one station actually left behind this round is collapsed.
    expect(within(list).getByRole("button", { expanded: false }).textContent).toMatch(/\b1\b/);
  });

  it("shows a CLOSED session's whole roster, ungrouped, even with the mode on", async () => {
    // The mode is recorded on the closed log (the switch that was in force is
    // part of the record), but the collapse is a LIVE operating affordance: the
    // toggle is gated `isLive`, so a closed session offers no way to expand the
    // group. Splitting here would hide the roster on the page that exists to
    // display it.
    const fetchMock = vi.fn().mockResolvedValue(
      jsonResponse(
        200,
        summary({
          lifecycle: "closed",
          closedAt: "2026-07-16T01:31:00Z",
          durationSeconds: 91 * 60,
          rosterOrderMode: "worked-sink",
          roster: [
            { checkInId: uuid(44), callsign: "K1XYZ", addedAt: "t", addedBy: uuid(200), worked: false },
            { checkInId: uuid(42), callsign: "W1AW", addedAt: "t", addedBy: uuid(200), worked: true },
            { checkInId: uuid(43), callsign: "N1CCK", addedAt: "t", addedBy: uuid(200), worked: true },
          ],
        }),
      ),
    );
    renderPage(fetchMock);

    await screen.findByText("K1XYZ");
    const list = screen.getByRole("list", { name: "Roster" });
    const visible = Array.from(list.querySelectorAll("[data-callsign]")).map(
      (el) => el.textContent,
    );
    expect(visible).toEqual(["K1XYZ", "W1AW", "N1CCK"]);
    expect(within(list).queryByRole("button", { expanded: false })).not.toBeInTheDocument();
  });

  it("the worked-sink toggle is absent for a relay (SetRosterOrderMode is NCS-only)", async () => {
    const fetchMock = vi
      .fn()
      .mockResolvedValue(jsonResponse(200, summary({ viewerRole: "relay" })));
    renderPage(fetchMock);

    await screen.findByText("W1AW");
    expect(screen.queryByRole("switch", { name: /worked/i })).not.toBeInTheDocument();
  });
});
