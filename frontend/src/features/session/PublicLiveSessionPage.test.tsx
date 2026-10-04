// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
import { act, render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { RouterProvider, createMemoryRouter, useLocation } from "react-router";
import type { ReactElement } from "react";

import { PublicLiveSessionPage } from "./PublicLiveSessionPage";
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

/** A WebSocket stand-in whose handlers the test can invoke to drive a delta. */
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

/** A route probe that surfaces the `returnTo` a gate navigation carried in state. */
function SignInProbe(): ReactElement {
  const location = useLocation();
  const returnTo = (location.state as { returnTo?: string } | null)?.returnTo ?? "";
  return <div data-testid="sign-in">{returnTo}</div>;
}

/** The REDACTED public view shape — no definitionId/definitionVersion, no addedBy. */
function publicView(overrides: Record<string, unknown> = {}) {
  return {
    id: uuid(1),
    lifecycle: "live",
    connections: [hfWay(14_250_000)],
    startedAt: "2026-07-16T00:00:00Z",
    closedAt: null,
    durationSeconds: null,
    latestSeq: 3,
    participantCount: 2,
    roster: [
      {
        checkInId: uuid(42),
        callsign: "W1AW",
        addedAt: "t",
        source: "staff",
        // The four fields the 2026-08-27 ruling moved onto the
        // account-less wire, seeded NON-DEFAULT so the assertions below can tell
        // a real value from the reducer's absent-key fallback.
        staying: "staying-for-comments",
        precedence: "emergency",
        traffic: 3,
        publicNote: "relaying for the county EOC",
      },
      {
        checkInId: uuid(43),
        callsign: "N1CCK",
        addedAt: "t",
        source: "self",
        staying: "in-and-out",
        precedence: "routine",
      },
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

/**
 * Renders the page with the account-less endpoints wired. `GET /api/accounts/me`
 * is intercepted here: `account === null` (the default) returns 401 so the viewer
 * resolves account-less; passing an `account` returns it 200 so a SIGNED-IN
 * participant's affordances (self-check-in control, YourTurnIndicator) light up.
 * Every OTHER request falls through to the caller's `fetchMock`, so its
 * `mock.calls` see only the `/live` traffic (the /me call never reaches it).
 */
function renderPage(
  fetchMock: ReturnType<typeof vi.fn>,
  account: Record<string, unknown> | null = null,
) {
  const wrapped = vi.fn((input: RequestInfo | URL, init?: RequestInit) => {
    const url = typeof input === "string" ? input : input.toString();
    if (url.endsWith("/api/accounts/me")) {
      return Promise.resolve(
        account === null
          ? jsonResponse(401, { type: "/errors/unauthenticated", status: 401 })
          : jsonResponse(200, account),
      );
    }
    const callThrough = fetchMock as unknown as (
      input: RequestInfo | URL,
      init?: RequestInit,
    ) => Promise<Response>;
    return callThrough(input, init);
  });
  vi.stubGlobal("fetch", wrapped);
  const router = createMemoryRouter(
    [
      { path: "/live/:id", element: <PublicLiveSessionPage /> },
      { path: "/sign-in", element: <SignInProbe /> },
      { path: "/profile", element: <div data-testid="profile" /> },
    ],
    { initialEntries: [`/live/${uuid(1)}`] },
  );
  render(<RouterProvider router={router} />);
  return router;
}

/** A signed-in participant account whose callsign matches a roster row. */
function participantAccount(overrides: Record<string, unknown> = {}) {
  return {
    id: uuid(200),
    email: "maria@example.com",
    emailVerifiedAt: "2026-07-16T00:00:00Z",
    consentRequired: false,
    requiredTermsVersion: "1",
    callsign: "N1CCK",
    displayName: "Maria",
    location: "Hartford, CT",
    grid: null,
    avatarUrl: null,
    gravatarUrl: "https://example.com/g",
    ...overrides,
  };
}

beforeEach(() => {
  vi.stubGlobal("WebSocket", NoopWebSocket as unknown as typeof WebSocket);
});

afterEach(() => {
  vi.unstubAllGlobals();
});

describe("PublicLiveSessionPage", () => {
  it("renders the roster and the operating frequency from the redacted public view", async () => {
    const fetchMock = vi.fn().mockResolvedValue(jsonResponse(200, publicView()));
    renderPage(fetchMock);

    expect(await screen.findByText("W1AW")).toBeInTheDocument();
    // Each row now carries a list position + a source badge alongside the
    // callsign, so assert the callsign is present per row and the
    // rows stay in roster order — not an exact textContent match. Scoped to the
    // Roster region — the page also carries a breadcrumb trail (its own
    // `<li>`s) that would otherwise pollute a page-wide listitem query.
    const rows = within(screen.getByRole("list", { name: "Roster" })).getAllByRole("listitem");
    expect(rows[0].textContent).toContain("W1AW");
    expect(rows[1].textContent).toContain("N1CCK");
    expect(screen.getByText(/14\.25/)).toBeInTheDocument();
  });

  it("fetches the account-less /live endpoint (not the owner endpoint)", async () => {
    const fetchMock = vi.fn().mockResolvedValue(jsonResponse(200, publicView()));
    renderPage(fetchMock);

    await screen.findByText("W1AW");
    const url = (fetchMock.mock.calls[0] as [string])[0];
    expect(url).toBe(`/api/net-sessions/${uuid(1)}/live`);
  });

  it("renders READ-ONLY: no Start/Close/Frequency/Add-check-in controls", async () => {
    const fetchMock = vi.fn().mockResolvedValue(jsonResponse(200, publicView()));
    renderPage(fetchMock);

    await screen.findByText("W1AW");
    expect(screen.queryByRole("button", { name: /start net/i })).not.toBeInTheDocument();
    expect(screen.queryByRole("button", { name: /close net/i })).not.toBeInTheDocument();
    expect(screen.queryByRole("button", { name: /set frequency/i })).not.toBeInTheDocument();
    // The quick-add is operator-console only — never on the public view.
    expect(screen.queryByRole("button", { name: /save & add new/i })).not.toBeInTheDocument();
    expect(screen.queryByLabelText(/callsign/i)).not.toBeInTheDocument();
  });

  it("routes an account-less viewer's Check-in tap to /sign-in with returnTo, never a write", async () => {
    // The self-check-in control renders for EVERY viewer (its own gate handles
    // signed-out routing) — this was previously dead code because the page
    // gated the control's mount behind `account !== null`, making the
    // signed-out routing branch unreachable in production. Confirms: the
    // button IS present, the tap navigates to /sign-in carrying returnTo, and
    // no check-in write is ever attempted, because the gate intercepts the
    // click before any API call.
    const fetchMock = vi.fn().mockResolvedValue(jsonResponse(200, publicView()));
    renderPage(fetchMock);

    await screen.findByText("W1AW");
    const button = await screen.findByRole("button", { name: /check in/i });
    await userEvent.click(button);

    const probe = await screen.findByTestId("sign-in");
    expect(probe.textContent).toBe(`/live/${uuid(1)}`);
    // Only the /live snapshot fetch happened — no POST to /check-ins.
    expect(fetchMock).not.toHaveBeenCalledWith(
      expect.stringContaining("/check-ins"),
      expect.anything(),
    );
  });

  it("renders source badges on the public view", async () => {
    // `source` is now public provenance: the redacted roster advertises the
    // Self/Staff badge on each row. `addedBy` still never crosses, and the
    // operator keycap legend stays operator-only.
    const fetchMock = vi.fn().mockResolvedValue(jsonResponse(200, publicView()));
    renderPage(fetchMock);

    await screen.findByText("W1AW");
    const badges = document.querySelectorAll("[data-source]");
    expect(badges.length).toBe(2);
    // Both provenances render (W1AW staff, N1CCK self).
    const sources = [...badges].map((b) => b.getAttribute("data-source")).sort();
    expect(sources).toEqual(["self", "staff"]);
    // The operator keycap legend is still never on the public view.
    expect(screen.queryByRole("group", { name: /keyboard shortcuts/i })).not.toBeInTheDocument();
  });

  it("renders the worked-station highlight + list position on the public view", async () => {
    // The cursor points at W1AW; the public roster now renders the coral working
    // treatment (completing shown as its own state) and a 1-based list position per row.
    const fetchMock = vi
      .fn()
      .mockResolvedValue(jsonResponse(200, publicView({ workingCheckInId: uuid(42) })));
    renderPage(fetchMock);

    await screen.findByText("W1AW");
    const rows = within(screen.getByRole("list", { name: "Roster" })).getAllByRole("listitem");
    expect(rows[0].getAttribute("data-working")).toBe("true");
    expect(rows[0].querySelector("[data-position]")?.textContent).toBe("1");
    expect(rows[1].querySelector("[data-position]")?.textContent).toBe("2");
  });

  it("shows a signed-in participant the self-check-in control (account-less stays read-only)", async () => {
    const fetchMock = vi.fn().mockResolvedValue(jsonResponse(200, publicView()));
    // A different callsign (not on the roster) → not checked in → "Check in".
    renderPage(fetchMock, participantAccount({ callsign: "W9ABC" }));

    await screen.findByText("W1AW");
    expect(await screen.findByRole("button", { name: /check in/i })).toBeInTheDocument();
  });

  it("wraps the self-check-in control in a footer bar naming the viewer's own callsign once checked in", async () => {
    // N1CCK is already on the roster (publicView's fixture) — the viewer is
    // checked in, so the footer names them and offers the staying toggle.
    const fetchMock = vi.fn().mockResolvedValue(jsonResponse(200, publicView()));
    renderPage(fetchMock, participantAccount({ callsign: "N1CCK" }));

    await screen.findByText("W1AW");
    expect(await screen.findByText(/you.re checked in as/i)).toBeInTheDocument();
    expect(screen.getAllByText("N1CCK").length).toBeGreaterThan(0);
    expect(screen.getByRole("button", { name: /check out/i })).toBeInTheDocument();
  });

  it("seeds the participant's OWN staying toggle from the SERVER's value, not the reducer default", async () => {
    // `PublicLiveSessionPage` builds `ownCheckIn` from the folded
    // `ownEntry.staying`. The public snapshot once carried no `staying` key at
    // all, so `seedFromSnapshot`'s `?? "in-and-out"`
    // was ALWAYS the value that reached the toggle — a participant the operator
    // had already set to `staying-for-comments` was shown "Stay for comments"
    // and clicking it PUT the status they were already in: a no-op write that
    // burned their `expectedVersion`. Widening the public projection fixed that
    // as a consequence, so the corrected behaviour is asserted here rather than
    // left to be mistaken for an accident later.
    //
    // Behavioural, not cosmetic: the assertion is on the PUT body the click
    // sends, so it fails if the toggle ever reads a default again.
    const fetchMock = vi.fn().mockResolvedValue(
      jsonResponse(
        200,
        publicView({
          roster: [
            {
              checkInId: uuid(43),
              callsign: "N1CCK",
              addedAt: "t",
              source: "self",
              staying: "staying-for-comments",
              precedence: "routine",
            },
          ],
        }),
      ),
    );
    const user = userEvent.setup();
    renderPage(fetchMock, participantAccount({ callsign: "N1CCK" }));

    // The callsign appears on the row AND in the footer bar naming the viewer,
    // so scope the wait to the button whose LABEL is the thing under test.
    const toggle = await screen.findByRole("button", { name: /switch to in and out/i });
    await user.click(toggle);

    const put = fetchMock.mock.calls.find(
      ([, init]) => (init as RequestInit | undefined)?.method === "PUT",
    );
    expect(put).toBeDefined();
    const [, init] = put as [string, RequestInit];
    expect(JSON.parse(init.body as string)).toMatchObject({ staying: "in-and-out" });
  });

  it("hides the self-check-in control while the net is stalled", async () => {
    // The self-check-in write is a roster mutation and
    // must freeze while stalled exactly like every other write control — it
    // was previously rendered unconditionally regardless of controlState.
    const fetchMock = vi
      .fn()
      .mockResolvedValue(jsonResponse(200, publicView({ controlState: "stalled" })));
    renderPage(fetchMock, participantAccount({ callsign: "W9ABC" }));

    await screen.findByText("W1AW");
    expect(screen.queryByRole("button", { name: /check in/i })).not.toBeInTheDocument();
  });

  it("lights the YourTurnIndicator on the signed-in viewer's own next-up row", async () => {
    // Cursor works W1AW (row 0); the viewer is N1CCK (row 1) — the next non-worked
    // entry — so their row shows the "your turn" indicator.
    const fetchMock = vi
      .fn()
      .mockResolvedValue(jsonResponse(200, publicView({ workingCheckInId: uuid(42) })));
    renderPage(fetchMock, participantAccount({ callsign: "N1CCK" }));

    // Scoped to the Roster region — the self-check-in footer bar also names
    // the viewer's own callsign once they're checked in.
    const rosterList = await screen.findByRole("list", { name: "Roster" });
    await within(rosterList).findByText("N1CCK");
    const status = await screen.findByRole("status", { name: /next up/i });
    expect(status.getAttribute("data-your-turn")).toBe("true");
    // It is on the viewer's OWN row (N1CCK), not the worked row.
    const ownRow = within(rosterList).getByText("N1CCK").closest("li");
    expect(ownRow?.getAttribute("data-your-turn")).toBe("true");
  });

  it("under worked-sink, lights the YourTurnIndicator on a viewer ABOVE the cursor — the page hands the selector the public snapshot's mode", async () => {
    // Round 2 under the sink: the viewer N1CCK (row 0) still awaits their turn;
    // W1AW (row 1) was worked in round 1 and holds the cursor again. The NCS
    // will call N1CCK next. Only a selector that knows the mode is worked-sink
    // can say so — the forward scan from the cursor finds nothing.
    const fetchMock = vi.fn().mockResolvedValue(
      jsonResponse(
        200,
        publicView({
          rosterOrderMode: "worked-sink",
          workingCheckInId: uuid(42),
          roster: [
            {
              checkInId: uuid(43),
              callsign: "N1CCK",
              addedAt: "t",
              source: "self",
              staying: "in-and-out",
              precedence: "routine",
            },
            {
              checkInId: uuid(42),
              callsign: "W1AW",
              addedAt: "t",
              source: "staff",
              staying: "staying-for-comments",
              precedence: "routine",
              worked: true,
            },
          ],
        }),
      ),
    );
    renderPage(fetchMock, participantAccount({ callsign: "N1CCK" }));

    const rosterList = await screen.findByRole("list", { name: "Roster" });
    await within(rosterList).findByText("N1CCK");
    const status = await screen.findByRole("status", { name: /next up/i });
    expect(status.getAttribute("data-your-turn")).toBe("true");
    const ownRow = within(rosterList).getByText("N1CCK").closest("li");
    expect(ownRow?.getAttribute("data-your-turn")).toBe("true");
  });

  it("appends a roster row when a redacted checkin.added delta folds in", async () => {
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
    const fetchMock = vi.fn().mockResolvedValue(jsonResponse(200, publicView()));
    renderPage(fetchMock);

    await screen.findByText("W1AW");
    expect(screen.queryByText("K2ABC")).not.toBeInTheDocument();

    await waitFor(() => expect(sockets.length).toBeGreaterThan(0));
    const socket = sockets[sockets.length - 1];
    act(() => {
      socket.onopen?.();
      // A REDACTED delta: no actorId, no clientEventId.
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

  it("opens the account-less public WebSocket path (/live/ws)", async () => {
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
    const fetchMock = vi.fn().mockResolvedValue(jsonResponse(200, publicView()));
    renderPage(fetchMock);

    await screen.findByText("W1AW");
    await waitFor(() => expect(sockets.length).toBeGreaterThan(0));
    expect(sockets[0].url).toContain(`/api/net-sessions/${uuid(1)}/live/ws`);
  });

  it("renders a not-found state for a 404 and never redirects to /sign-in", async () => {
    const fetchMock = vi
      .fn()
      .mockResolvedValue(jsonResponse(404, { type: "/errors/net-session-not-found", status: 404 }));
    renderPage(fetchMock);

    expect(await screen.findByRole("status")).toBeInTheDocument();
    expect(screen.queryByTestId("sign-in")).not.toBeInTheDocument();
  });

  it("has no WCAG 2.1 AA violations on the loaded page", async () => {
    const fetchMock = vi.fn().mockResolvedValue(jsonResponse(200, publicView()));
    renderPage(fetchMock);

    await screen.findByText("W1AW");
    await expectNoAxeViolations(document.body);
  });

  it("has no WCAG 2.1 AA violations on the OBSERVER roster carrying the newly-visible cells", async () => {
    // Authored rather than inferred. The page-level axe
    // pass above walks the whole body of the default fixture; this one scopes to
    // the roster list and first PROVES the four newly-visible affordances are in
    // the tree (staying indicator, precedence chip, labelled traffic count,
    // public note), so a future change that quietly stops rendering them cannot
    // keep this gate green by having nothing left to audit.
    const fetchMock = vi.fn().mockResolvedValue(jsonResponse(200, publicView()));
    renderPage(fetchMock);

    await screen.findByText("W1AW");
    const roster = screen.getByRole("list", { name: "Roster" });
    expect(roster.querySelector("[data-staying]")).not.toBeNull();
    expect(roster.querySelector("[data-precedence]")).not.toBeNull();
    expect(roster.querySelector("[data-traffic]")).not.toBeNull();
    expect(roster.querySelector("[data-public-note]")).not.toBeNull();
    await expectNoAxeViolations(roster);
    // The head strip labels those columns, and it is `aria-hidden` — audit it
    // together with the rows it describes rather than in isolation.
    await expectNoAxeViolations(screen.getByTestId("roster-column-heads"));
  });

  it("wraps the roster body in the shared Card chrome", async () => {
    const fetchMock = vi.fn().mockResolvedValue(jsonResponse(200, publicView()));
    renderPage(fetchMock);

    await screen.findByText("W1AW");
    const card = document.querySelector('[style*="var(--rounded-xl)"]');
    expect(card).not.toBeNull();
    expect(card?.contains(screen.getByRole("list", { name: "Roster" }))).toBe(true);
  });

  it("frames the participant view in the same elevated panel as the console", async () => {
    const fetchMock = vi.fn().mockResolvedValue(jsonResponse(200, publicView()));
    renderPage(fetchMock);

    await screen.findByText("W1AW");
    expect(screen.getByRole("main")).toHaveStyle({ maxWidth: "1200px" });

    const header = screen
      .getByText(/you.re watching/i)
      .closest("[data-session-header]") as HTMLElement;
    expect(header).toHaveStyle({ background: "var(--head-grad)" });
    expect(header.style.margin).toBe("");
    expect(header.closest("section")).toHaveStyle({
      boxShadow: "var(--shadow)",
      overflow: "hidden",
    });
  });

  it("heads the participant roster with exactly the columns its rows render", async () => {
    // The strip used to be asserted "explicitly NO precedence column (that is
    // the operator's density)"; precedence now reaches the observer surface, so
    // the head must follow its rows there. `RosterColumnHeads`'s own contract — the strip never labels
    // a column the rows omit and never omits a label for one they show — is what
    // is under test, in the newly-uniform direction. The REPORT column did not
    // move and is still absent.
    const fetchMock = vi.fn().mockResolvedValue(jsonResponse(200, publicView()));
    renderPage(fetchMock);

    await screen.findByText("W1AW");
    const heads = screen.getByTestId("roster-column-heads");

    expect(heads).toHaveTextContent("#");
    expect(heads).toHaveTextContent(/station/i);
    expect(heads).toHaveTextContent(/source/i);
    expect(heads).toHaveTextContent(/precedence/i);
    // Still true: the signal report is not one of the four fields that moved.
    expect(heads).not.toHaveTextContent(/report/i);
  });

  it("shows an ACCOUNT-LESS viewer the staying, precedence, traffic and public note the operator set", async () => {
    // The account-less path is the intended reach, not the edge case. `renderPage`'s default returns 401 from
    // `/api/accounts/me`, so this render resolves `account === null`.
    const fetchMock = vi.fn().mockResolvedValue(jsonResponse(200, publicView()));
    renderPage(fetchMock);

    await screen.findByText("W1AW");
    const rows = within(screen.getByRole("list", { name: "Roster" })).getAllByRole("listitem");
    const w1aw = rows[0];

    expect(w1aw.querySelector("[data-staying]")?.textContent).toContain("Staying");
    expect(w1aw.querySelector("[data-precedence]")?.getAttribute("data-precedence")).toBe(
      "emergency",
    );
    expect(w1aw.querySelector("[data-traffic]")?.textContent).toContain("3");
    expect(w1aw.querySelector("[data-public-note]")?.textContent).toContain(
      "relaying for the county EOC",
    );
    // The second station is genuinely different, so the first row's values are
    // the ones the server sent and not a constant the component renders.
    expect(rows[1].querySelector("[data-precedence]")?.getAttribute("data-precedence")).toBe(
      "routine",
    );
  });

  it("shows a SIGNED-IN participant exactly the same field set as an account-less one", async () => {
    // Uniformity is PROVEN across surfaces, not asserted.
    // A review caught an `account !== null` hedge on this exact
    // file; this reds if one is reintroduced as a "safety improvement".
    const fetchMock = vi.fn().mockResolvedValue(jsonResponse(200, publicView()));
    renderPage(fetchMock, { id: uuid(9), callsign: "N1CCK" });

    await screen.findByText("W1AW");
    const rows = within(screen.getByRole("list", { name: "Roster" })).getAllByRole("listitem");
    expect(rows[0].querySelector("[data-staying]")).not.toBeNull();
    expect(rows[0].querySelector("[data-precedence]")?.getAttribute("data-precedence")).toBe(
      "emergency",
    );
    expect(rows[0].querySelector("[data-traffic]")?.textContent).toContain("3");
    expect(rows[0].querySelector("[data-public-note]")?.textContent).toContain(
      "relaying for the county EOC",
    );
  });

  it("shows the observer NO staff note and NO write affordance on a roster row", async () => {
    // Visibility widened for the observer; the WRITE clause did not.
    const fetchMock = vi.fn().mockResolvedValue(
      jsonResponse(
        200,
        publicView({
          roster: [
            {
              checkInId: uuid(42),
              callsign: "W1AW",
              addedAt: "t",
              source: "staff",
              staying: "in-and-out",
              precedence: "routine",
              publicNote: "relaying for W1BBB",
              // A STAFF note seeded onto the wire deliberately. The server never
              // sends this key on the public projection (pinned in
              // `api_worked_station.rs`), so the fixture is modelling the shape a
              // LEAK would take — an unexpected `notes` key arriving on the
              // account-less snapshot. The page must render nothing from it,
              // because `buildRoster` maps named keys rather than spreading the
              // wire entry. Without this seed the assertion below ran against a
              // fixture that had no staff note at all and could not fail.
              notes: "STAFF-ONLY: sounded rough, watch for a relay",
            },
          ],
        }),
      ),
    );
    renderPage(fetchMock);

    await screen.findByText("W1AW");
    const roster = screen.getByRole("list", { name: "Roster" });
    expect(within(roster).queryByRole("button", { name: /edit/i })).toBeNull();
    expect(roster.textContent).not.toContain("STAFF-ONLY");
    expect(roster.textContent).not.toContain("sounded rough");
    // The PUBLIC note on the same entry still renders — so the assertions above
    // cannot pass by the row simply dropping every note.
    expect(roster.textContent).toContain("relaying for W1BBB");
  });

  it("renders a session-header card: eyebrow, freq/band/mode, Live badge, Elapsed + Checked-in stats", async () => {
    const fetchMock = vi.fn().mockResolvedValue(
      jsonResponse(200, publicView({ startedAt: "2026-07-16T00:00:00Z" })),
    );
    renderPage(fetchMock);

    await screen.findByText("W1AW");
    expect(screen.getByText(/you.re watching/i)).toBeInTheDocument();
    // The band/mode text rides alongside "SSB" in the freq pill — scope off
    // that unique node since the breadcrumb's middle crumb also says "20m".
    const freqPill = screen.getByText(/ssb/i);
    expect(freqPill.textContent).toContain("20m");
    expect(screen.getByText("Elapsed")).toBeInTheDocument();
    const checkedInStat = screen.getByText("Checked in");
    expect(checkedInStat.parentElement?.textContent).toContain("2");
  });

  it("renders a wayfinding breadcrumb trail ending at the session title", async () => {
    const fetchMock = vi.fn().mockResolvedValue(jsonResponse(200, publicView()));
    renderPage(fetchMock);

    await screen.findByText("W1AW");
    const nav = screen.getByRole("navigation", { name: /breadcrumb/i });
    expect(within(nav).getByText("Nets")).toBeInTheDocument();
    expect(within(nav).getByText(/sunday traffic net/i)).toBeInTheDocument();
  });
});
