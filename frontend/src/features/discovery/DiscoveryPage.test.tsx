// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
import { readFileSync } from "node:fs";
import { dirname, resolve } from "node:path";
import {
  act,
  cleanup,
  fireEvent,
  render,
  screen,
  waitFor,
  within,
} from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";
import { RouterProvider, createMemoryRouter } from "react-router";

import { messageForProblemType } from "../../errors/problemMessages";
import {
  absoluteLocalTime,
  absoluteLocalTimeWithWeekday,
} from "../../ui/util/humanizeTime";
import { expectNoAxeViolations } from "../../test/axe";
import { ProblemError } from "../auth/authApi";
import { connectionSummary } from "../nets/connectionPresentation";
import { DiscoveryPage } from "./DiscoveryPage";
import type { NetConnection } from "../nets/netsApi";
import type {
  AppliedDiscoveryQuery,
  DiscoveryFilters,
  DiscoveryNet,
  DiscoveryResponse,
} from "./discoveryApi";
import { discoverySearchParams, getDiscovery } from "./discoveryApi";

// The real module, with ONLY the fetch replaced. A factory that returned just
// `getDiscovery` would make the page's `FILTER_KEYS` / serialiser imports
// `undefined`, and every URL assertion below would be testing a fake instead of
// the shipped function.
vi.mock("./discoveryApi", async () => ({
  ...(await vi.importActual<typeof import("./discoveryApi")>(
    "./discoveryApi",
  )),
  getDiscovery: vi.fn(),
}));
const mockGet = vi.mocked(getDiscovery);

/** One connection, defaulted to nothing set — a caller names only the fields its
 * kind carries, the way the wire does. */
function connection(overrides: Partial<NetConnection>): NetConnection {
  return {
    id: "conn-1",
    position: 0,
    kind: "hf",
    plannedFrequencyHz: null,
    band: null,
    mode: null,
    repeaterOffsetHz: null,
    toneMode: null,
    toneValue: null,
    node: null,
    reflector: null,
    network: null,
    talkgroup: null,
    label: null,
    detail: null,
    ...overrides,
  };
}

function net(overrides: Partial<DiscoveryNet>): DiscoveryNet {
  const base = {
    id: "def-1",
    definitionVersion: 1,
    occurrenceId: "occ-1",
    scheduledStartAt: "2099-01-01T20:00:00+00:00",
    title: "Sunday Traffic Net",
    description: null,
    country: null,
    state: null,
    grid: null,
    netCategory: "traffic",
    netType: "open",
    expectedDurationMinutes: null,
    linkToken: "tok-def-1",
    matchedConnectionId: null,
    ...overrides,
  };
  // The default connection set is ONE 20m SSB way. Defaulting to `[]` instead
  // would make every "no frequency is shown" assertion in this file pass for
  // every net in it, including the ones that must show one — the vacuous shape
  // this guards against. A fixture that means "internet-only" passes
  // `connections` explicitly. (This default once mirrored the fixture's flat
  // `band`/`mode`/`plannedFrequencyHz`; those fields left the wire with the
  // columns behind them.)
  return {
    ...base,
    connections:
      overrides.connections ??
      [
        connection({
          id: `${base.id}-c0`,
          kind: "hf",
          plannedFrequencyHz: 14_230_000,
          band: "20m",
          mode: "ssb",
        }),
      ],
  };
}

const resp = (r: Partial<DiscoveryResponse>): DiscoveryResponse => ({
  activeNow: [],
  upcoming: [],
  applied: { sort: "time" },
  ...r,
});

function jsonResponse(status: number, body: unknown): Response {
  return new Response(JSON.stringify(body), {
    status,
    headers: {
      "content-type":
        status >= 400 ? "application/problem+json" : "application/json",
    },
  });
}

function noContent(): Response {
  return new Response(null, { status: 204 });
}

const signedInAccount = {
  id: "acct-1",
  email: "op@example.com",
  emailVerifiedAt: "2026-07-16T00:00:00Z",
  consentRequired: false,
  requiredTermsVersion: "2026-07-15",
  callsign: "W1AW",
  displayName: null,
  location: null,
  grid: null,
  avatarUrl: null,
  gravatarUrl: "https://gravatar.example/x",
};

const MEMBERSHIP_PATH = "/api/favorites/membership";

/** The ids a membership request asked about, parsed off its URL. */
function membershipIds(url: string): string[] {
  const ids = new URL(url, "http://test").searchParams.get("ids");
  return ids === null ? [] : ids.split(",");
}

/** Stubs `/api/accounts/me` and the favorites endpoints (`GET
 * /api/favorites/membership?ids=…`, `PUT|DELETE /api/favorites/:id`) on global
 * `fetch`, following the same routing-by-method+path pattern PublicNetPage's
 * test suite uses. `getDiscovery` itself stays module-mocked (`mockGet`) — this
 * only covers the account/favorites surface the hero card and upcoming rows
 * reuse. The membership read answers the asked ids that are in `favoritedIds`
 * unless `membershipResponder` takes it over (to hold it in flight, say). Every
 * request is recorded in the returned list as `METHOD url`. */
function stubAccountFetch({
  account = null,
  favoritedIds = [],
  membershipResponder,
  onFavoriteWrite,
}: {
  account?: typeof signedInAccount | null;
  favoritedIds?: string[];
  membershipResponder?: (askedIds: string[]) => Promise<Response>;
  onFavoriteWrite?: (method: string, url: string) => void;
} = {}): string[] {
  const calls: string[] = [];
  const fetchMock = vi.fn(async (url: string, init?: RequestInit) => {
    const method = init?.method ?? "GET";
    calls.push(`${method} ${url}`);
    if (url === "/api/accounts/me") {
      return account === null
        ? jsonResponse(401, { type: "/errors/unauthenticated", status: 401 })
        : jsonResponse(200, account);
    }
    if (url.startsWith(`${MEMBERSHIP_PATH}?`)) {
      const asked = membershipIds(url);
      return membershipResponder
        ? membershipResponder(asked)
        : jsonResponse(200, { favorited: asked.filter((id) => favoritedIds.includes(id)) });
    }
    if (url.startsWith("/api/favorites/")) {
      onFavoriteWrite?.(method, url);
      return noContent();
    }
    throw new Error(`unexpected fetch: ${method} ${url}`);
  });
  vi.stubGlobal("fetch", fetchMock);
  return calls;
}

afterEach(() => {
  vi.clearAllMocks();
  vi.unstubAllGlobals();
});

/** Mounts the page on a memory router so `useSearchParams` has a location to
 * read. Flat rather than nested under the App shell on purpose: these are page
 * tests, `setSearchParams` navigates relatively, and both shapes put the page
 * at pathname `/` — the real nesting stays pinned by `router.test.tsx`.
 *
 * Returns `container` as well as the router because the axe pass needs it. */
function renderDiscovery(initialEntries: string[] = ["/"]): {
  router: ReturnType<typeof createMemoryRouter>;
  container: HTMLElement;
} {
  const router = createMemoryRouter(
    [{ path: "/", element: <DiscoveryPage /> }],
    { initialEntries },
  );
  const { container } = render(<RouterProvider router={router} />);
  return { router, container };
}

/** Scopes a query to the Upcoming panel. The on-air panel's next-up pointer
 * deliberately repeats the soonest net's title, so an unscoped title query is
 * ambiguous by design. */
const inUpcoming = (): ReturnType<typeof within> =>
  within(screen.getByRole("region", { name: /upcoming/i }));

/** Unmounts the current tree and mounts a fresh page — for a test that needs
 * both the live and the upcoming fixtures in one pass. */
function cleanupAndRerender(): void {
  cleanup();
  renderDiscovery();
}

/** Reads a stylesheet next to this test file, for the two tests that weld the
 * anchors' class name to the rules that style it.
 *
 * The mechanism is copied from `ui/tokens/tokens-css.test.ts`, which documents
 * why the obvious approaches do not work here: vitest stubs CSS imports to
 * empty modules even with a raw query, and a module URL is not a `file:` URL
 * under the jsdom environment. Anchoring on the test file's own path via
 * `expect.getState()` also survives vitest being launched from a CWD other
 * than `frontend/`, which a bare relative path would not. */
const stylesheet = (relativePath: string): string => {
  const testPath = expect.getState().testPath;
  if (!testPath) {
    throw new Error(`vitest testPath unavailable — cannot locate ${relativePath}`);
  }
  return readFileSync(resolve(dirname(testPath), relativePath), "utf8");
};

describe("DiscoveryPage", () => {
  it("shows a distinct hero loading placeholder before the fetch resolves, never the confirmed-empty text", () => {
    // The hero once defaulted `activeNow` to `[]` while
    // `status === "loading"`, so it rendered the SAME "No nets are live right
    // now." text before the fetch even resolved. A loading skeleton must stay
    // distinct from the confirmed-empty state.
    mockGet.mockResolvedValue(resp({}));
    renderDiscovery();

    expect(screen.queryByText(/no nets are live right now/i)).toBeNull();
    expect(screen.getByText(/checking for live nets/i)).toBeInTheDocument();
  });

  it("shows a loading state, then the live-empty hero and the upcoming rows", async () => {
    mockGet.mockResolvedValue(
      resp({ upcoming: [net({ title: "Sunday Traffic Net" })] }),
    );
    renderDiscovery();

    expect(screen.getByRole("status")).toBeInTheDocument();

    expect(
      await screen.findByText(/no nets are live right now/i),
    ).toBeInTheDocument();
    expect(inUpcoming().getByText("Sunday Traffic Net")).toBeInTheDocument();
    expect(inUpcoming().getByText(/^in /)).toBeInTheDocument();
  });

  it("keys the hero off activeNow.length, not a hardcoded flag", async () => {
    mockGet.mockResolvedValue(
      resp({ activeNow: [net({ id: "live-1", title: "Live Emergency Net" })] }),
    );
    renderDiscovery();

    expect(await screen.findByText("Live Emergency Net")).toBeInTheDocument();
    expect(screen.queryByText(/no nets are live right now/i)).toBeNull();
  });

  it("shows a live-pulse eyebrow counting all active nets, and features the first as a hero card", async () => {
    mockGet.mockResolvedValue(
      resp({
        activeNow: [
          net({
            id: "live-1",
            occurrenceId: "occ-live-1",
            title: "Live Emergency Net",
            connections: [
              connection({
                id: "live-1-c0",
                plannedFrequencyHz: 7_238_000,
                band: "40m",
                mode: "ssb",
              }),
            ],
          }),
          net({
            id: "live-2",
            occurrenceId: "occ-live-2",
            title: "Cascade Morning ARES Net",
            connections: [
              connection({
                id: "live-2-c0",
                plannedFrequencyHz: 145_230_000,
                band: "2m",
                mode: "fm",
              }),
            ],
          }),
        ],
      }),
    );
    renderDiscovery();

    expect(
      await screen.findByRole("region", { name: /on the air now/i }),
    ).toHaveTextContent(/2 live/i);
    expect(screen.getByText(/7\.238/)).toBeInTheDocument();
    expect(screen.getAllByText(/^live$/i).length).toBeGreaterThan(0);
    expect(screen.getByRole("link", { name: /watch live/i })).toHaveAttribute(
      "href",
      "/live/occ-live-1",
    );
    expect(screen.getByText("Cascade Morning ARES Net")).toBeInTheDocument();
    expect(screen.getByText(/145\.23/)).toBeInTheDocument();
  });

  it("hides the hero's favorite star for a signed-out visitor", async () => {
    stubAccountFetch({ account: null });
    mockGet.mockResolvedValue(
      resp({ activeNow: [net({ id: "def-1", title: "Live Emergency Net" })] }),
    );
    renderDiscovery();

    await screen.findByText("Live Emergency Net");
    expect(
      screen.queryByRole("button", { name: /favorite this net/i }),
    ).not.toBeInTheDocument();
  });

  it("shows the hero's favorite star pre-seeded true for a signed-in visitor whose favorites already include it", async () => {
    stubAccountFetch({ account: signedInAccount, favoritedIds: ["def-1"] });
    mockGet.mockResolvedValue(
      resp({ activeNow: [net({ id: "def-1", title: "Live Emergency Net" })] }),
    );
    renderDiscovery();

    await screen.findByText("Live Emergency Net");
    // Pre-seeded true from GET /api/favorites, not assumed unfavorited.
    //
    // Queried BY pressed state, so the query itself retries until the favorites
    // response has been folded in. The star renders unpressed as soon as the
    // account and the net resolve, so matching on name alone returns the button
    // before that third request settles and a synchronous read then samples the
    // wrong value — intermittently green locally, red on a loaded CI runner.
    await screen.findByRole("button", {
      name: /favorite this net/i,
      pressed: true,
    });
  });

  it("asks the membership read about the starred nets on THIS page, and never reads the favorites list", async () => {
    // The favorites list is paged; walking it to light the stars spent the read
    // budget in proportion to the account's favorite count and could never
    // finish for a large one. The page asks about its own ids instead: the
    // featured live net and each upcoming row, deduplicated.
    const calls = stubAccountFetch({
      account: signedInAccount,
      favoritedIds: ["def-2", "unrelated"],
    });
    mockGet.mockResolvedValue(
      resp({
        activeNow: [
          net({ id: "def-1", title: "Live Emergency Net" }),
          net({ id: "def-7", occurrenceId: "occ-7", title: "Second Live Net" }),
        ],
        upcoming: [
          net({ id: "def-2", occurrenceId: "occ-2", title: "Upcoming Net" }),
          net({ id: "def-1", occurrenceId: "occ-1b", title: "Live Emergency Net" }),
        ],
      }),
    );
    renderDiscovery();

    await screen.findByRole("button", { name: /favorite this net/i, pressed: true });
    const membershipReads = calls.filter((call) => call.startsWith(`GET ${MEMBERSHIP_PATH}?`));
    expect(membershipReads).toHaveLength(1);
    expect(membershipIds(membershipReads[0].slice("GET ".length)).sort()).toEqual([
      "def-1",
      "def-2",
    ]);
    expect(calls.some((call) => /^GET \/api\/favorites(\?|$)/.test(call))).toBe(false);
  });

  it("re-asks for the nets on a freshly loaded list, so a filter change lights the new rows' stars", async () => {
    const calls = stubAccountFetch({
      account: signedInAccount,
      favoritedIds: ["def-5"],
    });
    mockGet.mockResolvedValue(
      resp({ upcoming: [net({ id: "def-1", occurrenceId: "occ-1", title: "First List" })] }),
    );
    renderDiscovery();
    await inUpcoming().findByText("First List");
    await waitFor(() =>
      expect(calls.filter((call) => call.startsWith(`GET ${MEMBERSHIP_PATH}?`))).toHaveLength(1),
    );

    mockGet.mockResolvedValue(
      resp({ upcoming: [net({ id: "def-5", occurrenceId: "occ-5", title: "Second List" })] }),
    );
    await userEvent.selectOptions(screen.getByLabelText(/band/i), "40m");

    await inUpcoming().findByText("Second List");
    await screen.findByRole("button", { name: /favorite this net/i, pressed: true });
    const membershipReads = calls.filter((call) => call.startsWith(`GET ${MEMBERSHIP_PATH}?`));
    expect(membershipReads).toHaveLength(2);
    expect(membershipIds(membershipReads[1].slice("GET ".length))).toEqual(["def-5"]);
  });

  it("keeps a toggle confirmed while the membership read was in flight, and still applies the read's answer to the other stars", async () => {
    // The read describes the server as it stood when it began. A toggle
    // confirmed after that is the newer fact for ITS net; the answer is still
    // the only fact for every other net on the page.
    let settle: (r: Response) => void = () => {};
    const pending = new Promise<Response>((r) => {
      settle = r;
    });
    stubAccountFetch({
      account: signedInAccount,
      membershipResponder: () => pending,
    });
    mockGet.mockResolvedValue(
      resp({
        upcoming: [
          net({ id: "def-9", occurrenceId: "occ-9", title: "Toggled Net" }),
          net({ id: "def-1", occurrenceId: "occ-1", title: "Already Favorited Net" }),
        ],
      }),
    );
    renderDiscovery();

    const stars = await screen.findAllByRole("button", { name: /favorite this net/i });
    expect(stars).toHaveLength(2);
    const [toggledStar, otherStar] = stars;
    expect(toggledStar).toHaveAttribute("aria-pressed", "false");
    await userEvent.click(toggledStar);
    await waitFor(() => expect(toggledStar).toHaveAttribute("aria-pressed", "true"));

    // The snapshot the server took before the toggle: def-1 favorited, def-9 not.
    settle(jsonResponse(200, { favorited: ["def-1"] }));

    await waitFor(() => expect(otherStar).toHaveAttribute("aria-pressed", "true"));
    expect(toggledStar).toHaveAttribute("aria-pressed", "true");
  });

  it("favorites the featured live net by its definition id when the star is toggled", async () => {
    const writes: string[] = [];
    stubAccountFetch({
      account: signedInAccount,
      onFavoriteWrite: (method, url) => writes.push(`${method} ${url}`),
    });
    mockGet.mockResolvedValue(
      resp({ activeNow: [net({ id: "def-9", title: "Live Emergency Net" })] }),
    );
    renderDiscovery();

    await screen.findByText("Live Emergency Net");
    const star = await screen.findByRole("button", {
      name: /favorite this net/i,
    });
    expect(star).toHaveAttribute("aria-pressed", "false");

    await userEvent.click(star);

    await waitFor(() => {
      expect(writes).toContain("PUT /api/favorites/def-9");
    });
  });

  it("names the soonest upcoming net as next-up when nothing is live", async () => {
    mockGet.mockResolvedValue(
      resp({
        upcoming: [
          net({
            id: "u1",
            occurrenceId: "o1",
            title: "Sierra Emergency Net",
            connections: [
              connection({
                id: "u1-c0",
                plannedFrequencyHz: 3_985_000,
                band: "80m",
                mode: "ssb",
              }),
            ],
          }),
          net({ id: "u2", occurrenceId: "o2", title: "Later Net" }),
        ],
      }),
    );
    renderDiscovery();

    // An empty live panel should still point somewhere: the first upcoming
    // entry (the response is already time-ordered) with its frequency.
    const nextUp = await screen.findByTestId("next-up");
    expect(nextUp).toHaveTextContent(/sierra emergency net/i);
    expect(nextUp).toHaveTextContent(/3\.985/);
    expect(nextUp).not.toHaveTextContent(/later net/i);
  });

  it("pairs the relative phrase with an absolute weekday-bearing local time at both render sites", async () => {
    // "in 4d" alone is what let a rule stored on the wrong
    // WEEKDAY sit unnoticed on the live instance — the absolute local time,
    // weekday included, is what makes a wrong occurrence legible as wrong.
    // The expected string is computed with the same formatter, never hardcoded
    // to one platform's locale output.
    const startAt = "2099-01-01T20:00:00+00:00";
    mockGet.mockResolvedValue(
      resp({
        upcoming: [
          net({ id: "u1", occurrenceId: "o1", scheduledStartAt: startAt }),
        ],
      }),
    );
    renderDiscovery();

    const absolute = absoluteLocalTimeWithWeekday(startAt);
    // Comparing against the helper's own output alone would only prove the
    // component CALLS it — a helper that dropped the weekday, or returned "",
    // would keep that assertion green, and the weekday is the whole point of
    // this pairing. So also assert the rendered value is NOT the terse sibling's
    // output: that pins the weekday-bearing variant as the one in use without
    // hardcoding any platform locale string.
    const terse = absoluteLocalTime(startAt);
    expect(absolute).not.toBe(terse);

    // Render site 1: the upcoming row.
    const row = await waitFor(() =>
      inUpcoming().getByText(/^in /).closest("time"),
    );
    expect(row).not.toBeNull();
    // The machine-readable UTC instant stays in `dateTime`: a raw UTC string is
    // forbidden as VISIBLE text, not as the semantic attribute.
    expect(row).toHaveAttribute("dateTime", startAt);
    expect(row?.textContent).toContain(absolute);
    expect(row?.textContent).not.toContain(terse);

    // Render site 2: the next-up pointer in the on-air panel.
    const nextUpTime = within(screen.getByTestId("next-up")).getByText(/^in /)
      .closest("time");
    expect(nextUpTime).toHaveAttribute("dateTime", startAt);
    expect(nextUpTime?.textContent).toContain(absolute);
    expect(nextUpTime?.textContent).not.toContain(terse);
  });

  it("promises no next-up when nothing is live and nothing is scheduled", async () => {
    mockGet.mockResolvedValue(resp({}));
    renderDiscovery();
    await screen.findByText(/no nets are live right now/i);

    expect(screen.queryByTestId("next-up")).not.toBeInTheDocument();
  });

  it("drops the next-up line once a net is actually live", async () => {
    mockGet.mockResolvedValue(
      resp({
        activeNow: [net({ id: "l1", occurrenceId: "o1", title: "Live Net" })],
        upcoming: [net({ id: "u1", occurrenceId: "o2" })],
      }),
    );
    renderDiscovery();

    await screen.findByText("Live Net");
    // The live roster IS the answer now; a "next up" line would compete.
    expect(screen.queryByTestId("next-up")).not.toBeInTheDocument();
  });

  it("keeps secondary filters behind a disclosure, with the primary three always reachable", async () => {
    mockGet.mockResolvedValue(resp({}));
    renderDiscovery();
    await screen.findByText(/no upcoming nets/i);

    // Search/band/mode are the hot path and stay in the toolbar.
    expect(screen.getByLabelText(/search/i)).toBeInTheDocument();
    expect(screen.getByLabelText(/band/i)).toBeInTheDocument();
    expect(screen.getByLabelText(/mode/i)).toBeInTheDocument();
    // The long tail is collapsed — nine equal chips was the whole problem.
    // It stays mounted (values must survive collapsing) but hidden, so it is
    // out of reach for sighted users and AT alike.
    expect(screen.getByLabelText(/grid/i)).not.toBeVisible();

    const disclosure = screen.getByRole("button", { name: /more filters/i });
    expect(disclosure).toHaveAttribute("aria-expanded", "false");
    await userEvent.click(disclosure);

    expect(disclosure).toHaveAttribute("aria-expanded", "true");
    expect(screen.getByLabelText(/grid/i)).toBeVisible();
    expect(screen.getByLabelText(/net type/i)).toBeVisible();
  });

  it("counts set-but-collapsed filters on the disclosure so hidden state stays visible", async () => {
    mockGet.mockResolvedValue(resp({}));
    renderDiscovery();
    await screen.findByText(/no upcoming nets/i);

    const disclosure = screen.getByRole("button", { name: /more filters/i });
    await userEvent.click(disclosure);
    await userEvent.type(screen.getByLabelText(/grid/i), "FN31");
    await userEvent.click(disclosure);

    // Collapsed again, but the filter is still narrowing results — the count
    // is the only thing standing between the user and phantom filtering.
    expect(screen.getByLabelText(/grid/i, { selector: "input" })).not.toBeVisible();
    expect(disclosure).toHaveAccessibleName(/more filters/i);
    expect(disclosure).toHaveTextContent("1");
  });

  it("lays upcoming rows on the same column template as the head strip", async () => {
    mockGet.mockResolvedValue(
      resp({ upcoming: [net({ title: "Sierra Emergency Net" })] }),
    );
    renderDiscovery();

    await screen.findByRole("region", { name: /upcoming/i });
    const row = inUpcoming().getByText("Sierra Emergency Net").closest("li");
    const heads = screen.getByTestId("panel-column-heads");

    // Columns that don't share a template don't line up — that misalignment
    // is exactly what a head strip is supposed to prevent.
    expect(row?.style.gridTemplateColumns).not.toBe("");
    expect(row?.style.gridTemplateColumns).toBe(heads.style.gridTemplateColumns);
  });

  it("drops the column strip on a phone-width viewport, where rows stack", async () => {
    vi.stubGlobal("innerWidth", 380);
    mockGet.mockResolvedValue(
      resp({ upcoming: [net({ title: "Sierra Emergency Net" })] }),
    );
    renderDiscovery();
    await screen.findByRole("region", { name: /upcoming/i });
    inUpcoming().getByText("Sierra Emergency Net");

    // Column heads over stacked cards would label columns that aren't there.
    expect(screen.queryByTestId("panel-column-heads")).not.toBeInTheDocument();
  });

  it("shows a distinct no-upcoming empty state on success with no upcoming nets", async () => {
    mockGet.mockResolvedValue(resp({}));
    renderDiscovery();

    // Both empty states coexist and are independent.
    expect(
      await screen.findByText(/no nets are live right now/i),
    ).toBeInTheDocument();
    expect(screen.getByText(/no upcoming nets/i)).toBeInTheDocument();
  });

  it("maps an error to a message via the problem map and retries without auto-refresh", async () => {
    mockGet
      .mockRejectedValueOnce(
        new ProblemError({
          type: "/errors/discovery-query-invalid",
          status: 400,
        }),
      )
      .mockResolvedValueOnce(resp({ upcoming: [net({})] }));
    renderDiscovery();

    const alert = await screen.findByRole("alert");
    // The copy is resolved through the shared problem map, not a hardcoded
    // component string [memory: tdd-test-logic-not-text].
    expect(alert).toHaveTextContent(
      messageForProblemType("/errors/discovery-query-invalid"),
    );
    // Exactly one fetch — no auto-refresh loop.
    expect(mockGet).toHaveBeenCalledTimes(1);

    await userEvent.click(
      screen.getByRole("button", { name: /retry|try again/i }),
    );
    expect(
      await waitFor(() => inUpcoming().getByText("Sunday Traffic Net")),
    ).toBeInTheDocument();
    expect(mockGet).toHaveBeenCalledTimes(2);
  });

  it("surfaces a 429 rate-limited problem as its mapped message and does not auto-retry", async () => {
    // The public discovery read is IP-rate-limited server-side; a
    // 429 must render the shared /errors/rate-limited copy via the problem map
    // (not a hardcoded string) and, crucially, must NOT trigger a retry storm
    // against the throttled endpoint — the page never auto-refreshes.
    mockGet.mockRejectedValue(
      new ProblemError({ type: "/errors/rate-limited", status: 429 }),
    );
    renderDiscovery();

    const alert = await screen.findByRole("alert");
    expect(alert).toHaveTextContent(
      messageForProblemType("/errors/rate-limited"),
    );
    // Exactly one fetch — a 429 does not spawn further requests on its own.
    expect(mockGet).toHaveBeenCalledTimes(1);
  });

  it("re-fetches with the chosen filter when a filter control changes", async () => {
    mockGet.mockResolvedValue(resp({}));
    renderDiscovery();
    await screen.findByText(/no upcoming nets/i);

    await userEvent.selectOptions(screen.getByLabelText(/band/i), "20m");

    await waitFor(() => {
      const called = mockGet.mock.calls.some(
        ([filters]) =>
          (filters as { band?: string } | undefined)?.band === "20m",
      );
      expect(called).toBe(true);
    });
  });

  it("renders the filter row as pill-shaped chips, accenting only the one currently set", async () => {
    mockGet.mockResolvedValue(resp({}));
    renderDiscovery();
    await screen.findByText(/no upcoming nets/i);

    const bandChip = screen.getByLabelText(/band/i);
    const modeChip = screen.getByLabelText(/mode/i);
    expect(bandChip).toHaveStyle({ borderRadius: "var(--rounded-md)" });
    expect(bandChip).toHaveStyle({ background: "var(--surface)" });
    expect(modeChip).toHaveStyle({ background: "var(--surface)" });

    await userEvent.selectOptions(bandChip, "20m");

    // Only the chip whose filter is now set switches to the accent fill;
    // the rest stay neutral (task 77's "one active at a time per its value").
    expect(bandChip).toHaveStyle({ background: "var(--accent-deep)" });
    expect(modeChip).toHaveStyle({ background: "var(--surface)" });
  });

  it("states the filters and sort the SERVER applied, not the ones the client believes it sent", async () => {
    // The echo is only a compensating control if a surface
    // reads it; sourcing this from `filters` would restate the client's hope.
    // Asserted as the applied KEY SET (a structural property), never a
    // sentence.
    mockGet.mockResolvedValue(
      resp({ applied: { q: "sunday", sort: "name" }, upcoming: [net({})] }),
    );
    renderDiscovery();

    const summary = await screen.findByTestId("applied-summary");
    const keys = within(summary)
      .getAllByRole("listitem")
      .map((item) => item.dataset.appliedKey);
    expect(keys).toEqual(["q", "sort"]);
  });

  it("keeps the applied statement on the server's answer when the server honoured fewer filters than the client sent", async () => {
    vi.useFakeTimers();
    try {
      // The server's echo omits `band` even though the control holds "20m" —
      // the difference has to be visible rather than silent, and the control
      // itself must keep its own value (the 300 ms debounce trap).
      mockGet.mockResolvedValue(resp({ applied: { sort: "time" } }));
      renderDiscovery();
      await vi.advanceTimersByTimeAsync(0);

      const band = screen.getByLabelText(/band/i);
      fireEvent.change(band, { target: { value: "20m" } });
      await vi.advanceTimersByTimeAsync(300);

      expect(mockGet).toHaveBeenLastCalledWith(
        expect.objectContaining({ band: "20m" }),
      );
      expect((band as HTMLSelectElement).value).toBe("20m");

      const summary = screen.getByTestId("applied-summary");
      expect(
        within(summary).queryByTestId("applied-band"),
      ).not.toBeInTheDocument();
      const keys = within(summary)
        .getAllByRole("listitem")
        .map((item) => item.dataset.appliedKey);
      expect(keys).toEqual(["sort"]);
    } finally {
      vi.useRealTimers();
    }
  });

  it("debounces rapid filter changes into a single re-fetch, so an unindexed free-text scan on a public, unrate-limited endpoint is not hammered on every keystroke", async () => {
    vi.useFakeTimers();
    try {
      mockGet.mockResolvedValue(resp({}));
      renderDiscovery();

      // The initial mount load fires immediately — never debounced.
      expect(mockGet).toHaveBeenCalledTimes(1);

      const search = screen.getByLabelText(/search/i);
      fireEvent.change(search, { target: { value: "s" } });
      fireEvent.change(search, { target: { value: "su" } });
      fireEvent.change(search, { target: { value: "sun" } });

      // Still debouncing — no fetch yet for the burst of keystrokes.
      expect(mockGet).toHaveBeenCalledTimes(1);

      await vi.advanceTimersByTimeAsync(300);

      // Exactly ONE additional fetch for the whole burst, carrying the LAST
      // value (earlier, superseded changes never fired their own request).
      expect(mockGet).toHaveBeenCalledTimes(2);
      expect(mockGet).toHaveBeenLastCalledWith(
        expect.objectContaining({ q: "sun" }),
      );
    } finally {
      vi.useRealTimers();
    }
  });

  it("renders an upcoming net as a hairline-divided panel row carrying its freq", async () => {
    mockGet.mockResolvedValue(
      resp({
        upcoming: [
          net({
            title: "Sierra Emergency Net",
            connections: [
              connection({
                id: "def-1-c0",
                plannedFrequencyHz: 3_985_000,
                band: "80m",
                mode: "ssb",
              }),
            ],
          }),
        ],
      }),
    );
    renderDiscovery();

    await screen.findByRole("region", { name: /upcoming/i });
    const row = inUpcoming().getByText("Sierra Emergency Net").closest("li");
    // Rows sit flush inside the panel frame, divided by hairlines — not as
    // separately-bordered floating cards inside a card.
    expect(row?.getAttribute("style")).toContain(
      "border-bottom: 1px solid var(--border)",
    );
    expect(row).toHaveTextContent(/3\.985/);
    expect(row).toHaveTextContent(/80m/);
  });

  it("hides the upcoming row's favorite star for a signed-out visitor and shows it for a signed-in one", async () => {
    stubAccountFetch({ account: null });
    mockGet.mockResolvedValue(
      resp({ upcoming: [net({ title: "Sierra Emergency Net" })] }),
    );
    renderDiscovery();

    await screen.findByRole("region", { name: /upcoming/i });
    inUpcoming().getByText("Sierra Emergency Net");
    expect(
      screen.queryByRole("button", { name: /favorite this net/i }),
    ).not.toBeInTheDocument();
  });

  it("favorites an upcoming net by its definition id when the row's star is toggled", async () => {
    const writes: string[] = [];
    stubAccountFetch({
      account: signedInAccount,
      onFavoriteWrite: (method, url) => writes.push(`${method} ${url}`),
    });
    mockGet.mockResolvedValue(
      resp({
        upcoming: [net({ id: "def-42", title: "Sierra Emergency Net" })],
      }),
    );
    renderDiscovery();

    await screen.findByRole("region", { name: /upcoming/i });
    inUpcoming().getByText("Sierra Emergency Net");
    const star = await screen.findByRole("button", {
      name: /favorite this net/i,
    });
    expect(star).toHaveAttribute("aria-pressed", "false");

    await userEvent.click(star);

    await waitFor(() => {
      expect(writes).toContain("PUT /api/favorites/def-42");
    });
  });

  it("leads with an identity band carrying the mark and the product pitch", async () => {
    mockGet.mockResolvedValue(resp({}));
    renderDiscovery();
    await screen.findByText(/no nets are live right now/i);

    // An account-less first visitor needs to learn what NetRoll IS before the
    // roster data: the band sits above the live panel, not beside it.
    const band = screen.getByTestId("discovery-hero");
    expect(band).toContainElement(screen.getByTestId("logomark"));
    expect(band).toContainElement(
      screen.getByRole("heading", { level: 2, name: /net/i }),
    );
    expect(
      band.compareDocumentPosition(
        screen.getByRole("region", { name: /on the air now/i }),
      ) & Node.DOCUMENT_POSITION_FOLLOWING,
    ).toBeTruthy();
  });

  it("counts live and upcoming nets in the hero status strip from the response", async () => {
    mockGet.mockResolvedValue(
      resp({
        activeNow: [net({ id: "l1", occurrenceId: "o1" })],
        upcoming: [
          net({ id: "u1", occurrenceId: "o2" }),
          net({ id: "u2", occurrenceId: "o3" }),
        ],
      }),
    );
    renderDiscovery();

    const strip = await screen.findByTestId("discovery-status-strip");
    // Counts are derived, not decorative: one live, two upcoming.
    expect(strip).toHaveTextContent(/1 live/i);
    expect(strip).toHaveTextContent(/2 upcoming/i);
  });

  it("asserts no counts in the status strip until the fetch resolves", () => {
    mockGet.mockResolvedValue(resp({}));
    renderDiscovery();

    // "0 live now" before the data lands would be a claim the page cannot
    // yet make (the distinction the hero already respects).
    expect(screen.getByTestId("discovery-status-strip")).not.toHaveTextContent(
      /\d/,
    );
  });

  it("builds the page from framed panels on the shared content measure", async () => {
    mockGet.mockResolvedValue(resp({}));
    renderDiscovery();
    await screen.findByText(/no nets are live right now/i);

    // One card wrapping everything flattened the page's hierarchy; each
    // section is now its own elevated panel inside the 1200px measure the
    // shell chrome shares.
    const nav = screen.getByRole("navigation", { name: /breadcrumb/i });
    expect(nav.parentElement).toHaveStyle({ maxWidth: "1200px" });

    for (const name of [/on the air now/i, /upcoming/i]) {
      const panel = screen.getByRole("region", { name });
      // jsdom's CSS engine can't resolve `var()` inside the `border`
      // shorthand (it silently resets the whole property), so the border
      // itself is asserted via the raw style attribute.
      expect(panel).toHaveStyle({
        background: "var(--surface)",
        boxShadow: "var(--shadow)",
      });
      expect(panel.getAttribute("style")).toContain(
        "border: 1px solid var(--border)",
      );
    }
  });

  it("shows a net's description inside its upcoming row", async () => {
    mockGet.mockResolvedValue(
      resp({
        upcoming: [
          net({
            title: "Sierra Emergency Net",
            description: "Weekly traffic handling for the section.",
          }),
        ],
      }),
    );
    renderDiscovery();

    await screen.findByRole("region", { name: /upcoming/i });
    const row = inUpcoming()
      .getByText("Sierra Emergency Net")
      .closest("li") as HTMLElement;

    expect(
      within(row).getAllByTestId("net-description-preview"),
    ).toHaveLength(1);
  });

  it("emits no description node at all for an upcoming net without one", async () => {
    mockGet.mockResolvedValue(
      resp({ upcoming: [net({ title: "Sierra Emergency Net" })] }),
    );
    renderDiscovery();

    await screen.findByRole("region", { name: /upcoming/i });
    const row = inUpcoming()
      .getByText("Sierra Emergency Net")
      .closest("li") as HTMLElement;

    // Absence, not emptiness: an emitted-but-empty node still carries a gap.
    expect(within(row).queryByTestId("net-description-preview")).toBeNull();
  });

  it("has no accessibility violations in the loaded state", async () => {
    mockGet.mockResolvedValue(resp({ upcoming: [net({})] }));
    const { container } = renderDiscovery();
    await screen.findByText(/no nets are live right now/i);
    await expectNoAxeViolations(container);
  });

  it("has no accessibility violations while stating the URL keys it ignored", async () => {
    // The pass above mounts at "/", where the ignored-key statement never
    // renders — without this one the accessibility claim covers nothing of the
    // ignored-key statement at all.
    mockGet.mockResolvedValue(resp({ upcoming: [net({})] }));
    const { container } = renderDiscovery(["/?fbclid=abc"]);
    await screen.findByText(/no nets are live right now/i);
    await screen.findByTestId("ignored-params");
    await expectNoAxeViolations(container);
  });

  it("prefers the server's problem detail over the slug-map fallback", async () => {
    // One of forty-two surfaces. This file's only edit in that
    // story is the error alert; the assertion is the preference ORDER.
    const detail = "the field-naming answer the server sent";
    mockGet.mockRejectedValue(
      new ProblemError({
        type: "/errors/discovery-query-invalid",
        status: 400,
        detail,
      }),
    );
    renderDiscovery();

    const alert = await screen.findByRole("alert");
    expect(alert).toHaveTextContent(detail);
    expect(alert).not.toHaveTextContent(
      messageForProblemType("/errors/discovery-query-invalid"),
    );
  });

  /** Advances the debounce AND lets React commit. React Router lands a
   * navigation inside a transition, which only flushes on `act` exit — without
   * this the router's location can run several steps ahead of the render, and
   * a push followed by a back-navigation would never reach the component at
   * all. */
  const settle = async (ms: number): Promise<void> => {
    await act(async () => {
      await vi.advanceTimersByTimeAsync(ms);
    });
  };

  // ── The filters live in the URL ───────────────────────────────
  //
  // Assertions here are on the router's own location, the object handed to
  // `getDiscovery`, and call counts — never on rendered prose. `window.location`
  // is NOT the memory router's location and is deliberately never read.

  /** The filter object the Nth request actually carried. */
  const filtersOf = (call: number): DiscoveryFilters =>
    mockGet.mock.calls[call][0] ?? {};

  it("writes a chosen filter into the browser URL when the change settles", async () => {
    vi.useFakeTimers();
    try {
      mockGet.mockResolvedValue(resp({}));
      const { router } = renderDiscovery();
      await settle(0);

      fireEvent.change(screen.getByLabelText(/band/i), {
        target: { value: "20m" },
      });
      await settle(300);

      const params = new URLSearchParams(router.state.location.search);
      expect(params.get("band")).toBe("20m");
    } finally {
      vi.useRealTimers();
    }
  });

  it("walks back through filter states, re-REQUESTING the popped one rather than only re-painting", async () => {
    vi.useFakeTimers();
    try {
      mockGet.mockResolvedValue(resp({}));
      const { router } = renderDiscovery();
      await settle(0);

      fireEvent.change(screen.getByLabelText(/band/i), {
        target: { value: "20m" },
      });
      await settle(300);
      fireEvent.change(screen.getByLabelText(/^sort$/i), {
        target: { value: "name" },
      });
      await settle(300);
      expect(
        new URLSearchParams(router.state.location.search).get("sort"),
      ).toBe("name");

      // React Router commits a navigation inside a transition, which React
      // only flushes on `act` exit; the popped state then re-seeds `filters`
      // and the debounce it re-arms lands the request 300 ms later, not
      // synchronously with the navigation.
      await act(async () => {
        await router.navigate(-1);
      });
      await settle(300);

      const popped = new URLSearchParams(router.state.location.search);
      expect(popped.get("band")).toBe("20m");
      expect(popped.get("sort")).toBeNull();
      expect(mockGet).toHaveBeenLastCalledWith({ band: "20m" });
    } finally {
      vi.useRealTimers();
    }
  });

  it("leaves ONE history entry for a whole typed search term, not one per keystroke", async () => {
    vi.useFakeTimers();
    try {
      mockGet.mockResolvedValue(resp({}));
      const { router } = renderDiscovery();
      await settle(0);
      const before = router.state.location.search;
      expect(before).toBe("");
      expect(mockGet).toHaveBeenCalledTimes(1);

      const search = screen.getByLabelText(/search/i);
      fireEvent.change(search, { target: { value: "s" } });
      fireEvent.change(search, { target: { value: "su" } });
      fireEvent.change(search, { target: { value: "sun" } });
      await settle(300);

      expect(new URLSearchParams(router.state.location.search).get("q")).toBe(
        "sun",
      );
      expect(mockGet).toHaveBeenCalledTimes(2);

      // ...and the push the page just made must not itself re-arm the
      // debounce. `setSearchParams` is `useCallback`'d on `searchParams`, so
      // naming either in the effect's deps fires a SECOND request one debounce
      // later for a state already loaded.
      await settle(300);
      expect(mockGet).toHaveBeenCalledTimes(2);

      // One entry means ONE step back returns to the unfiltered state.
      await act(async () => {
        await router.navigate(-1);
      });
      await settle(300);
      expect(router.state.location.search).toBe(before);
    } finally {
      vi.useRealTimers();
    }
  });

  it("carries a cold URL's filters on the VERY FIRST request and shows them selected", async () => {
    mockGet.mockResolvedValue(resp({ upcoming: [net({})] }));
    renderDiscovery(["/?band=20m&sort=name"]);

    await screen.findByRole("region", { name: /upcoming/i });
    expect(filtersOf(0)).toEqual({ band: "20m", sort: "name" });
    expect((screen.getByLabelText(/band/i) as HTMLSelectElement).value).toBe(
      "20m",
    );
    expect((screen.getByLabelText(/^sort$/i) as HTMLSelectElement).value).toBe(
      "name",
    );
  });

  it("renders the list from a URL carrying unknown keys, requests only the known ones, and names the rest", async () => {
    mockGet.mockResolvedValue(
      resp({
        applied: { band: "20m", sort: "time" },
        upcoming: [net({ title: "Sunday Traffic Net" })],
      }),
    );
    const { router } = renderDiscovery(["/?band=20m&bnad=40m&fbclid=abc"]);

    await screen.findByRole("region", { name: /upcoming/i });
    expect(inUpcoming().getByText("Sunday Traffic Net")).toBeInTheDocument();

    // Only this page's own vocabulary reaches the request.
    expect(filtersOf(0)).toEqual({ band: "20m" });

    const ignored = screen.getByTestId("ignored-params");
    const keys = within(ignored)
      .getAllByRole("listitem")
      .map((item) => item.dataset.ignoredKey);
    expect(keys).toEqual(["bnad", "fbclid"]);
    // It is a statement about the link, not an error.
    expect(within(ignored).queryByRole("alert")).toBeNull();

    // The page did not author this link and must not rewrite it.
    expect(router.state.location.search).toBe("?band=20m&bnad=40m&fbclid=abc");
  });

  it("makes NO ignored-key statement about a KNOWN key sent blank", async () => {
    mockGet.mockResolvedValue(resp({}));
    renderDiscovery(["/?band=&q=%20%20"]);

    await screen.findByText(/no upcoming nets/i);
    // Blank known keys requested nothing: they reach neither the wire...
    expect(discoverySearchParams(filtersOf(0)).toString()).toBe("");
    //...nor the statement, which is computed against the page's key list and
    // never against the server's echo.
    expect(screen.queryByTestId("ignored-params")).toBeNull();
  });

  it("rebuilds the URL from known keys only once the viewer edits a filter", async () => {
    vi.useFakeTimers();
    try {
      mockGet.mockResolvedValue(resp({ applied: { sort: "time" } }));
      const { router } = renderDiscovery(["/?band=20m&bnad=40m&fbclid=abc"]);
      await settle(0);

      fireEvent.change(screen.getByLabelText(/mode/i), {
        target: { value: "fm" },
      });
      await settle(300);

      const params = new URLSearchParams(router.state.location.search);
      expect(params.get("band")).toBe("20m");
      expect(params.get("mode")).toBe("fm");
      expect(params.has("bnad")).toBe(false);
      expect(params.has("fbclid")).toBe(false);
      // The statement disappears by construction, not by a second code path.
      expect(screen.queryByTestId("ignored-params")).toBeNull();
    } finally {
      vi.useRealTimers();
    }
  });

  it("emits NO applied-summary element at all when the echo states nothing", async () => {
    // Cast because `sort` is required on the interface — that is the point: the
    // type proves nothing about what the network actually sent.
    mockGet.mockResolvedValue(
      resp({ applied: {} as AppliedDiscoveryQuery, upcoming: [net({})] }),
    );
    renderDiscovery();

    await screen.findByRole("region", { name: /upcoming/i });
    expect(screen.queryByTestId("applied-summary")).toBeNull();
  });

  it("keeps a non-string applied value away from React and still renders the list", async () => {
    mockGet.mockResolvedValue(
      resp({
        applied: {
          sort: "time",
          q: { nope: 1 } as unknown as string,
        },
        upcoming: [net({ title: "Sunday Traffic Net" })],
      }),
    );
    renderDiscovery();

    await screen.findByRole("region", { name: /upcoming/i });
    expect(inUpcoming().getByText("Sunday Traffic Net")).toBeInTheDocument();
    const summary = screen.getByTestId("applied-summary");
    const keys = within(summary)
      .getAllByRole("listitem")
      .map((item) => item.dataset.appliedKey);
    expect(keys).toEqual(["sort"]);
  });

  it("labels an echoed key by OWN property, so a `__proto__` key cannot resolve to Object.prototype", async () => {
    // The key has to arrive the way the network delivers it: an object literal
    // written `{ __proto__: "x" }` sets the prototype and creates no own
    // property, so it could never reach the render at all. `JSON.parse` makes
    // it an ordinary own enumerable key — which is exactly what `fetch` does.
    mockGet.mockResolvedValue(
      resp({
        applied: JSON.parse(
          '{"sort":"time","__proto__":"x"}',
        ) as AppliedDiscoveryQuery,
        upcoming: [net({ title: "Sunday Traffic Net" })],
      }),
    );
    renderDiscovery();

    // A prototype-chain lookup returns `Object.prototype` here and throws
    // "Objects are not valid as a React child", dropping the whole route — so
    // the region simply never appears.
    await screen.findByRole("region", { name: /upcoming/i });
    expect(inUpcoming().getByText("Sunday Traffic Net")).toBeInTheDocument();
    const keys = within(screen.getByTestId("applied-summary"))
      .getAllByRole("listitem")
      .map((item) => item.dataset.appliedKey);
    expect(keys).toEqual(["sort", "__proto__"]);
  });

  it("labels an echoed key by OWN property, so a function-valued inherited name cannot reach React", async () => {
    mockGet.mockResolvedValue(
      resp({
        applied: {
          sort: "time",
          toString: "x",
        } as unknown as AppliedDiscoveryQuery,
        upcoming: [net({ title: "Sunday Traffic Net" })],
      }),
    );
    renderDiscovery();

    // `"toString" in APPLIED_LABELS` is true through the prototype chain and
    // yields a FUNCTION, which React refuses as a child.
    await screen.findByRole("region", { name: /upcoming/i });
    expect(inUpcoming().getByText("Sunday Traffic Net")).toBeInTheDocument();
    const summary = screen.getByTestId("applied-summary");
    const keys = within(summary)
      .getAllByRole("listitem")
      .map((item) => item.dataset.appliedKey);
    expect(keys).toEqual(["sort", "toString"]);
    // Every stated dimension names itself. A prototype-chain lookup yields a
    // FUNCTION for this key; React drops a function child silently, leaving a
    // bare unlabelled ": x" pill rather than crashing. Asserting the SHAPE (the
    // pill is labelled at all) rather than the label's wording.
    for (const item of within(summary).getAllByRole("listitem")) {
      expect(item.textContent?.startsWith(":")).toBe(false);
    }
  });

  it("makes NO statement at all when the echo is not an object, rather than fabricating one from its indices", async () => {
    mockGet.mockResolvedValue(
      resp({
        applied: "time" as unknown as AppliedDiscoveryQuery,
        upcoming: [net({ title: "Sunday Traffic Net" })],
      }),
    );
    renderDiscovery();

    await screen.findByRole("region", { name: /upcoming/i });
    expect(inUpcoming().getByText("Sunday Traffic Net")).toBeInTheDocument();
    // Enumerating a string would emit one entry per character under an
    // accessible name promising the server's answer.
    expect(screen.queryByTestId("applied-summary")).toBeNull();
  });

  it("states a dimension the server applied even when the client's key list has never heard of it", async () => {
    mockGet.mockResolvedValue(
      resp({
        applied: {
          sort: "time",
          futureFilter: "x",
        } as AppliedDiscoveryQuery,
        upcoming: [net({})],
      }),
    );
    renderDiscovery();

    const summary = await screen.findByTestId("applied-summary");
    const keys = within(summary)
      .getAllByRole("listitem")
      .map((item) => item.dataset.appliedKey);
    expect(keys).toEqual(["sort", "futureFilter"]);
  });

  it("sends a known key's unrecognised VALUE unmodified — sanitising is not the client's call", async () => {
    mockGet.mockResolvedValue(resp({ applied: { sort: "band" } }));
    renderDiscovery(["/?sort=band"]);

    await screen.findByText(/no upcoming nets/i);
    expect(filtersOf(0)).toEqual({ sort: "band" });
  });

  it("surfaces the server's 400 for a bad filter value and does NOT rewrite the URL", async () => {
    mockGet.mockRejectedValue(
      new ProblemError({
        type: "/errors/discovery-query-invalid",
        status: 400,
        detail: "sort: is not a recognized sort",
      }),
    );
    const { router } = renderDiscovery(["/?sort=zzz"]);

    expect(await screen.findByRole("alert")).toBeInTheDocument();
    // Sent verbatim: `zzz` is not one of this page's own sort options, and
    // scrubbing it here would replace the server's 400 with a silent default.
    expect(filtersOf(0)).toEqual({ sort: "zzz" });
    expect(router.state.location.search).toBe("?sort=zzz");
  });

  it("serialises the same filter set to a byte-identical query string whatever order it was set in", async () => {
    vi.useFakeTimers();
    try {
      mockGet.mockResolvedValue(resp({}));
      const { router } = renderDiscovery();
      await settle(0);

      fireEvent.change(screen.getByLabelText(/^sort$/i), {
        target: { value: "name" },
      });
      await settle(300);
      fireEvent.change(screen.getByLabelText(/band/i), {
        target: { value: "20m" },
      });
      await settle(300);

      // The one place param ORDER is pinned, for both the URL and the request.
      expect(router.state.location.search).toBe("?band=20m&sort=name");
      expect(discoverySearchParams(filtersOf(2)).toString()).toBe(
        "band=20m&sort=name",
      );
    } finally {
      vi.useRealTimers();
    }
  });

  it("produces that same query string when the two filters are set in the opposite order", async () => {
    vi.useFakeTimers();
    try {
      mockGet.mockResolvedValue(resp({}));
      const { router } = renderDiscovery();
      await settle(0);

      fireEvent.change(screen.getByLabelText(/band/i), {
        target: { value: "20m" },
      });
      await settle(300);
      fireEvent.change(screen.getByLabelText(/^sort$/i), {
        target: { value: "name" },
      });
      await settle(300);

      expect(router.state.location.search).toBe("?band=20m&sort=name");
    } finally {
      vi.useRealTimers();
    }
  });

  it("keeps a second edit made while the page's OWN push is still in flight", async () => {
    vi.useFakeTimers();
    try {
      mockGet.mockResolvedValue(resp({}));
      const { router } = renderDiscovery();
      await settle(0);

      fireEvent.change(screen.getByLabelText(/band/i), {
        target: { value: "20m" },
      });
      // Deliberately NOT wrapped in `act`. React Router commits a navigation
      // inside a transition, so after this the page HAS pushed `?band=20m` but
      // the new `searchParams` has not reached the component yet. That late
      // arrival is indistinguishable from a back-navigation unless the page
      // tracks the query string it believes it already wrote.
      await vi.advanceTimersByTimeAsync(300);

      fireEvent.change(screen.getByLabelText(/^sort$/i), {
        target: { value: "name" },
      });
      await settle(300);

      // Without that tracking the arriving `?band=20m` resets `filters` to the
      // URL, discarding the sort the viewer just chose: the select goes back to
      // unset and the request narrows to `{ band: "20m" }`.
      expect(screen.getByLabelText(/^sort$/i)).toHaveValue("name");
      expect(new URLSearchParams(router.state.location.search).get("sort")).toBe(
        "name",
      );
      expect(mockGet).toHaveBeenLastCalledWith({ band: "20m", sort: "name" });
    } finally {
      vi.useRealTimers();
    }
  });

  it("leaves an arrival link alone when an edit settles back to the same filter set", async () => {
    vi.useFakeTimers();
    try {
      mockGet.mockResolvedValue(resp({}));
      const { router } = renderDiscovery(["/?band=20m&utm_source=newsletter"]);
      await settle(0);

      // The control moved; the filter set did not. Comparing the page's
      // canonical form against the URL's RAW string would read the untouched
      // `utm_source` as a change and push an entry that strips it.
      const search = screen.getByLabelText(/search/i);
      fireEvent.change(search, { target: { value: "a" } });
      fireEvent.change(search, { target: { value: "" } });
      await settle(300);

      expect(router.state.location.search).toBe(
        "?band=20m&utm_source=newsletter",
      );
    } finally {
      vi.useRealTimers();
    }
  });

  it("makes exactly ONE request from a cold filtered URL and pushes no history entry", async () => {
    vi.useFakeTimers();
    try {
      mockGet.mockResolvedValue(resp({}));
      const { router } = renderDiscovery(["/?band=20m"]);
      await settle(1000);

      expect(mockGet).toHaveBeenCalledTimes(1);
      expect(router.state.location.search).toBe("?band=20m");
    } finally {
      vi.useRealTimers();
    }
  });

  it("returns the URL to no query string when a filter is cleared, as a state back can walk to", async () => {
    vi.useFakeTimers();
    try {
      mockGet.mockResolvedValue(resp({}));
      const { router } = renderDiscovery();
      await settle(0);

      const band = screen.getByLabelText(/band/i);
      fireEvent.change(band, { target: { value: "20m" } });
      await settle(300);
      fireEvent.change(band, { target: { value: "" } });
      await settle(300);

      // A trailing "?" is a visible wart in a shared link.
      expect(router.state.location.search).toBe("");

      // Clearing was a PUSH, not a rewrite of the filtered entry.
      await act(async () => {
        await router.navigate(-1);
      });
      await settle(300);
      expect(router.state.location.search).toBe("?band=20m");
    } finally {
      vi.useRealTimers();
    }
  });

  // ── The card describes every connection ───────────────────────

  it("offers only the sort keys that still have a defensible answer", async () => {
    mockGet.mockResolvedValue(resp({ upcoming: [net({})] }));
    renderDiscovery();

    await screen.findByRole("region", { name: /upcoming/i });
    const sort = screen.getByLabelText(/^sort$/i) as HTMLSelectElement;
    // The placeholder option carries the empty value; the rest are the
    // vocabulary. `band` and `mode` are gone: a net now has a SET of bands and
    // ORDER BY over a set has no answer.
    expect(
      [...sort.options].map((o) => o.value).filter((v) => v !== ""),
    ).toEqual(["time", "name", "category", "type"]);
  });

  it("states the sort the server could not honour, outside the applied list", async () => {
    mockGet.mockResolvedValue(
      resp({
        applied: { sort: "time", sortUnavailable: "band" },
        upcoming: [net({})],
      }),
    );
    renderDiscovery(["/?sort=band"]);

    const statement = await screen.findByTestId("sort-unavailable");
    expect(statement.dataset.sortUnavailable).toBe("band");
    // Deliberately NOT folded into "Showing": that list's accessible name
    // promises the filters and sort the server APPLIED, and this one was not.
    const summary = screen.getByTestId("applied-summary");
    expect(summary.contains(statement)).toBe(false);
    expect(
      within(summary)
        .getAllByRole("listitem")
        .map((item) => item.dataset.appliedKey),
    ).toEqual(["sort"]);
  });

  it("makes no fallback statement when the server honoured the sort", async () => {
    // REGRESSION PIN. Guards the statement rendering unconditionally — a bar
    // that is always there says nothing.
    mockGet.mockResolvedValue(
      resp({ applied: { sort: "name" }, upcoming: [net({})] }),
    );
    renderDiscovery(["/?sort=name"]);

    await screen.findByTestId("applied-summary");
    expect(screen.queryByTestId("sort-unavailable")).toBeNull();
  });

  it("renders an internet-only net's actual ways on, and invents no frequency for it", async () => {
    // This fixture once also carried the stale flat `20m` /
    // 14.230 MHz the server's mirror kept when the last RF connection went
    // away, and the test pinned that the card ignored them. The mirror is gone
    // from the wire; what is left to pin is that a card with no RF way shows
    // no frequency at all.
    mockGet.mockResolvedValue(
      resp({
        upcoming: [
          net({
            title: "EchoLink Only Net",
            connections: [
              connection({ id: "c-echo", kind: "echolink", node: "12345" }),
            ],
          }),
        ],
      }),
    );
    renderDiscovery();

    await screen.findByRole("region", { name: /upcoming/i });
    const row = inUpcoming()
      .getByText("EchoLink Only Net")
      .closest("li") as HTMLElement;
    // Present: the way the net actually IS reached.
    expect(within(row).getByText(/12345/)).toBeInTheDocument();
    // Absent: the stale mirror, which the card published to the world as fact.
    expect(row.textContent).not.toMatch(/20m/);
    expect(row.textContent).not.toMatch(/14\.230/);
  });

  it("marks the connection the SERVER matched, not the first one", async () => {
    mockGet.mockResolvedValue(
      resp({
        upcoming: [
          net({
            title: "Two Ways Net",
            connections: [
              connection({
                id: "c-hf",
                kind: "hf",
                plannedFrequencyHz: 14_230_000,
                band: "20m",
                mode: "ssb",
              }),
              connection({
                id: "c-2m",
                position: 1,
                kind: "repeater",
                plannedFrequencyHz: 145_230_000,
                band: "2m",
                mode: "fm",
              }),
            ],
            matchedConnectionId: "c-2m",
          }),
        ],
      }),
    );
    renderDiscovery(["/?band=2m"]);

    await screen.findByRole("region", { name: /upcoming/i });
    const row = inUpcoming()
      .getByText("Two Ways Net")
      .closest("li") as HTMLElement;
    const marked = within(row).getByTestId("matched-connection");
    // The id crosses the wire as an IDENTITY and is never rendered as text;
    // the mark is keyed by it and shows that connection's own description.
    expect(marked.dataset.connectionId).toBe("c-2m");
    expect(marked.dataset.connectionId).not.toBe("c-hf");
    expect(row.textContent).not.toMatch(/c-2m/);
    // Axe has no rule for two elements sharing an accessible name, so the
    // distinction is asserted directly rather than inferred from a green scan.
    // STRUCTURALLY, not by fixture: the mark's accessible name always opens
    // with a prefix the plain summary never carries. Asserting only
    // `!== summary` would hold here because THIS fixture happens to have two
    // different RF connections, and would go quietly vacuous on a net whose
    // summary and matched description render the same string.
    expect(marked.getAttribute("aria-label")).toMatch(
      /^Matches your filter: \S/,
    );
    expect(marked.getAttribute("aria-label")).not.toBe(
      within(row).getByTestId("connection-summary").textContent,
    );
  });

  it("keeps the mark's accessible name distinct even when it describes the SAME text as the summary", async () => {
    // The case the sibling test above cannot reach. A single-RF net's summary
    // and its matched connection render byte-identical text — `connectionSummary`
    // shows the first RF connection, and a band or mode filter can only match an
    // RF connection — so the two elements would share an accessible name if the
    // prefix were ever dropped. Axe would still pass: it has no rule for
    // duplicate accessible names. This is the fixture that makes the prefix
    // load-bearing rather than incidental.
    mockGet.mockResolvedValue(
      resp({
        upcoming: [
          net({
            title: "One Way Net",
            connections: [
              connection({
                id: "c-only",
                kind: "hf",
                plannedFrequencyHz: 14_230_000,
                band: "20m",
                mode: "ssb",
              }),
            ],
            matchedConnectionId: "c-only",
          }),
        ],
      }),
    );
    renderDiscovery(["/?band=20m"]);

    await screen.findByRole("region", { name: /upcoming/i });
    const row = inUpcoming()
      .getByText("One Way Net")
      .closest("li") as HTMLElement;
    const marked = within(row).getByTestId("matched-connection");
    const summary = within(row).getByTestId("connection-summary").textContent;
    // The premise: this fixture really does make the two describe the same way.
    expect(marked.textContent).toContain(summary as string);
    // The invariant: they are still tellable apart by accessible name.
    expect(marked.getAttribute("aria-label")).toBe(
      `Matches your filter: ${summary}`,
    );
    expect(marked.getAttribute("aria-label")).not.toBe(summary);
  });

  it("marks nothing when no band or mode filter was applied", async () => {
    // REGRESSION PIN against guessing: the first connection is not "the match".
    mockGet.mockResolvedValue(
      resp({
        upcoming: [
          net({
            title: "Two Ways Net",
            connections: [
              connection({ id: "c-hf", plannedFrequencyHz: 14_230_000, band: "20m", mode: "ssb" }),
              connection({ id: "c-2m", position: 1, kind: "repeater", plannedFrequencyHz: 145_230_000, band: "2m", mode: "fm" }),
            ],
            matchedConnectionId: null,
          }),
        ],
      }),
    );
    renderDiscovery();

    await screen.findByRole("region", { name: /upcoming/i });
    expect(screen.queryByTestId("matched-connection")).toBeNull();
  });

  it("has no accessibility violations on a card carrying a multi-connection set and a fallback statement", async () => {
    mockGet.mockResolvedValue(
      resp({
        applied: { sort: "time", sortUnavailable: "band" },
        upcoming: [
          net({
            title: "Many Ways Net",
            connections: [
              connection({ id: "c-hf", plannedFrequencyHz: 14_230_000, band: "20m", mode: "ssb" }),
              connection({ id: "c-2m", position: 1, kind: "repeater", plannedFrequencyHz: 145_230_000, band: "2m", mode: "fm" }),
              connection({ id: "c-echo", position: 2, kind: "echolink", node: "12345" }),
            ],
            matchedConnectionId: "c-2m",
          }),
        ],
      }),
    );
    const { container } = renderDiscovery(["/?band=2m&sort=band"]);
    await screen.findByTestId("sort-unavailable");
    // A GREEN AXE RUN IS NOT EVIDENCE HERE, and the pin says so where it runs
    // rather than in a sibling test. Axe has no rule for two elements sharing
    // an accessible name, so the one thing this multi-connection card must get
    // right — that the marked connection is tellable from the summary beside it
    // — is asserted directly, and asserted on the PREFIX, which holds for every
    // connection set rather than only for one with two distinct RF ways.
    const row = inUpcoming()
      .getByText("Many Ways Net")
      .closest("li") as HTMLElement;
    const marked = within(row).getByTestId("matched-connection");
    expect(marked.getAttribute("aria-label")).toMatch(
      /^Matches your filter: \S/,
    );
    expect(marked.getAttribute("aria-label")).not.toBe(
      within(row).getByTestId("connection-summary").textContent,
    );
    await expectNoAxeViolations(container);
  });

  // ── Filter by connection kind ────────────────────────────────

  it("sends the chosen connection kind as its wire token and writes it into the URL", async () => {
    vi.useFakeTimers();
    try {
      mockGet.mockResolvedValue(resp({}));
      const { router } = renderDiscovery();
      await settle(0);

      fireEvent.change(screen.getByLabelText(/connection kind/i), {
        target: { value: "ysf" },
      });
      await settle(300);

      expect(mockGet).toHaveBeenLastCalledWith(
        expect.objectContaining({ kind: "ysf" }),
      );
      expect(router.state.location.search).toBe("?kind=ysf");
    } finally {
      vi.useRealTimers();
    }
  });

  it("offers the picker's own kinds, valued by wire token and named in words", async () => {
    mockGet.mockResolvedValue(resp({}));
    renderDiscovery();
    await screen.findByText(/no upcoming nets/i);

    const select = screen.getByLabelText(/connection kind/i) as HTMLSelectElement;
    const options = [...select.options].filter((o) => o.value !== "");
    // The value is what the server reads; the text is what a person reads. A
    // raw-token render puts `ysf` in both and fails here.
    expect(options.map((o) => o.value)).toEqual([
      "hf",
      "repeater",
      "echolink",
      "allstar",
      "dmr",
      "dstar",
      "ysf",
      "urf",
    ]);
    const ysf = options.find((o) => o.value === "ysf");
    expect(ysf?.textContent).toBe("System Fusion");
    expect(options.find((o) => o.value === "dstar")?.textContent).toBe("D-Star");
    expect(options.some((o) => o.value === "other")).toBe(false);
  });

  it("does not count a set connection kind on the More-filters badge, because the control is in the toolbar", async () => {
    mockGet.mockResolvedValue(resp({}));
    renderDiscovery();
    await screen.findByText(/no upcoming nets/i);

    const disclosure = screen.getByRole("button", { name: /more filters/i });
    expect(disclosure).toHaveAttribute("aria-expanded", "false");
    await userEvent.selectOptions(
      screen.getByLabelText(/connection kind/i),
      "echolink",
    );

    // A chip the visitor can see is not a hidden filter. The count badge only
    // renders when something behind the disclosure is set, so the button's
    // whole text is its label.
    expect(disclosure.textContent).toBe("More filters");
    expect(screen.getByLabelText(/connection kind/i)).toBeVisible();
  });

  it("states an applied connection kind in the echo's list order, after mode", async () => {
    // Structural, not prose: the applied list renders KNOWN keys in
    // `FILTER_KEYS` order and then anything the client has never heard of. A
    // `kind` the page does not know lands LAST, after `sort`; one it knows sits
    // between `mode` and `sort`.
    mockGet.mockResolvedValue(
      resp({
        applied: { mode: "fm", kind: "echolink", sort: "time" },
        upcoming: [net({})],
      }),
    );
    renderDiscovery();

    const summary = await screen.findByTestId("applied-summary");
    const keys = within(summary)
      .getAllByRole("listitem")
      .map((item) => item.dataset.appliedKey);
    expect(keys).toEqual(["mode", "kind", "sort"]);
    expect(within(summary).getByTestId("applied-kind").dataset.appliedKey).toBe(
      "kind",
    );
  });

  it("keeps Band and Mode each resolving to exactly one control beside the new chip", async () => {
    // REGRESSION PIN, green on write: `getByLabelText` throws on multiple
    // matches, and a double-digit number of tests in this file address the
    // band and mode chips by /band/i and /mode/i. A label containing either
    // word breaks them all at once with a failure that reads as unrelated.
    mockGet.mockResolvedValue(resp({}));
    renderDiscovery();
    await screen.findByText(/no upcoming nets/i);

    expect(screen.getAllByLabelText(/band/i)).toHaveLength(1);
    expect(screen.getAllByLabelText(/mode/i)).toHaveLength(1);
  });

  // ── The landing says what it left out ─────────────────────────
  //
  // `applied.truncated` names the collections the server cut, by wire name, and
  // is ABSENT when nothing was. Every assertion below is on an element's
  // presence, its scope, or a qualified count — never on a sentence.

  const inOnAir = (): ReturnType<typeof within> =>
    within(screen.getByRole("region", { name: /on the air now/i }));

  it("states beside EACH cut collection that it was cut, as two distinct elements", async () => {
    // Two statements, not one: `upcoming`'s can offer "narrow to reach the
    // rest" because the filters narrow it; `activeNow`'s cannot, because
    // nothing narrows it. A single shared statement would promise `activeNow`
    // an action it does not have.
    mockGet.mockResolvedValue(
      resp({
        applied: { sort: "time", truncated: ["activeNow", "upcoming"] },
        activeNow: [net({ id: "live-1", occurrenceId: "s-1", title: "Live One" })],
        upcoming: [net({ id: "up-1", occurrenceId: "o-1", title: "Up One" })],
      }),
    );
    renderDiscovery();

    const liveStatement = await screen.findByTestId("active-now-truncated");
    const upcomingStatement = screen.getByTestId("upcoming-truncated");
    expect(inOnAir().getByTestId("active-now-truncated")).toBe(liveStatement);
    expect(inUpcoming().getByTestId("upcoming-truncated")).toBe(upcomingStatement);
    // Alongside the rows, never instead of them: the rows shown are real.
    expect(inOnAir().getByText("Live One")).toBeInTheDocument();
    expect(inUpcoming().getByText("Up One")).toBeInTheDocument();
  });

  it("names only the collection that was cut, and nothing when neither was", async () => {
    mockGet.mockResolvedValueOnce(
      resp({
        applied: { sort: "time", truncated: ["upcoming"] },
        activeNow: [net({ id: "live-1", occurrenceId: "s-1" })],
        upcoming: [net({})],
      }),
    );
    renderDiscovery();
    await screen.findByTestId("upcoming-truncated");
    expect(screen.queryByTestId("active-now-truncated")).toBeNull();
  });

  it("makes no truncation statement when the key is absent — absence means nothing was cut", async () => {
    // REGRESSION PIN. The omit-when-empty ruling: an absent key is the positive
    // statement "complete", and a bar that is always there says nothing.
    mockGet.mockResolvedValue(
      resp({
        activeNow: [net({ id: "live-1", occurrenceId: "s-1" })],
        upcoming: [net({})],
      }),
    );
    renderDiscovery();
    await screen.findByTestId("applied-summary");
    expect(screen.queryByTestId("active-now-truncated")).toBeNull();
    expect(screen.queryByTestId("upcoming-truncated")).toBeNull();
  });

  it("qualifies every count of a cut collection with a trailing +, and leaves a complete one bare", async () => {
    // The counts are the lie: "2 live now" over a cut `activeNow` states a
    // total the response never gave. The `N+` idiom, asserted on the count
    // element and never on the copy around it.
    mockGet.mockResolvedValueOnce(
      resp({
        applied: { sort: "time", truncated: ["activeNow"] },
        activeNow: [
          net({ id: "live-1", occurrenceId: "s-1" }),
          net({ id: "live-2", occurrenceId: "s-2" }),
        ],
        upcoming: [net({})],
      }),
    );
    renderDiscovery();

    await screen.findByTestId("active-now-truncated");
    expect(screen.getByTestId("live-count")).toHaveTextContent(/^2\+$/);
    expect(screen.getByTestId("live-panel-count")).toHaveTextContent(/^2\+/);
    expect(screen.getByTestId("upcoming-count")).toHaveTextContent(/^1$/);
  });

  it("qualifies the upcoming count when `upcoming` is the cut collection", async () => {
    mockGet.mockResolvedValueOnce(
      resp({
        applied: { sort: "time", truncated: ["upcoming"] },
        activeNow: [net({ id: "live-1", occurrenceId: "s-1" })],
        upcoming: [net({ id: "u1", occurrenceId: "o1" }), net({ id: "u2", occurrenceId: "o2" })],
      }),
    );
    renderDiscovery();

    await screen.findByTestId("upcoming-truncated");
    expect(screen.getByTestId("upcoming-count")).toHaveTextContent(/^2\+$/);
    expect(screen.getByTestId("live-count")).toHaveTextContent(/^1$/);
    expect(screen.getByTestId("live-panel-count")).toHaveTextContent(/^1 /);
  });

  it("never lists the truncation key among the filters the server applied", async () => {
    // Excluded BY NAME, as `sortUnavailable` is — not by shape. The list's
    // accessible name promises the filters and sort the server APPLIED, and a
    // truncation fact is not one. A string-valued `truncated` is the shape that
    // would slip past a `typeof value === "string"` guard alone.
    mockGet.mockResolvedValue(
      resp({
        applied: {
          sort: "time",
          truncated: "upcoming",
        } as unknown as AppliedDiscoveryQuery,
        upcoming: [net({})],
      }),
    );
    renderDiscovery();

    const summary = await screen.findByTestId("applied-summary");
    expect(
      within(summary)
        .getAllByRole("listitem")
        .map((item) => item.dataset.appliedKey),
    ).toEqual(["sort"]);
    expect(within(summary).queryByTestId("applied-truncated")).toBeNull();
  });

  it("degrades a malformed `truncated` value to no statement, never to a render throw", async () => {
    // The same two guards `appliedEntries` takes on this exact object: the
    // value is unvalidated network JSON, and a non-array or an array holding a
    // non-string must reach neither React as a child nor a statement as a
    // fact. Two shapes, each rendered clean.
    mockGet.mockResolvedValueOnce(
      resp({
        applied: {
          sort: "time",
          truncated: "activeNow",
        } as unknown as AppliedDiscoveryQuery,
        activeNow: [net({ id: "live-1", occurrenceId: "s-1" })],
        upcoming: [net({})],
      }),
    );
    renderDiscovery();
    await screen.findByTestId("applied-summary");
    expect(screen.queryByTestId("active-now-truncated")).toBeNull();
    expect(screen.queryByTestId("upcoming-truncated")).toBeNull();
    expect(screen.getByTestId("live-count")).toHaveTextContent(/^1$/);

    mockGet.mockResolvedValueOnce(
      resp({
        applied: {
          sort: "time",
          truncated: ["upcoming", 7],
        } as unknown as AppliedDiscoveryQuery,
        upcoming: [net({})],
      }),
    );
    renderDiscovery();
    await waitFor(() =>
      expect(screen.getAllByTestId("applied-summary")).toHaveLength(2),
    );
    expect(screen.queryByTestId("upcoming-truncated")).toBeNull();
  });

  // ── The EMPTY-and-cut state ────────────────────────────────────────────────
  //
  // A cut collection can come back short of its bound and, when every row
  // inside the served window is one this version cannot render, empty. The
  // empty-state copy asserts the strongest totality claim on the page ("No
  // upcoming nets."), so leaving it up beside "more matched than are shown"
  // states both halves of a contradiction. So the empty branch and the Next-up
  // card are both suppressed when the collection is cut; the truncation
  // statement stands alone. No new copy — the with-cards placement rule,
  // extended to the zero-card case.

  it("keeps the empty state and Next-up when an EMPTY collection was not cut", async () => {
    // The other side of the fence: suppression is conditional on the cut, not
    // on emptiness, so the ordinary empty landing is untouched.
    mockGet.mockResolvedValue(
      resp({
        applied: { sort: "time" },
        activeNow: [],
        upcoming: [net({ id: "u1", occurrenceId: "o1", title: "Later Net" })],
      }),
    );
    renderDiscovery();

    expect(await screen.findByTestId("active-now-empty")).toBeInTheDocument();
    expect(screen.getByTestId("next-up")).toBeInTheDocument();
    expect(screen.queryByTestId("active-now-truncated")).toBeNull();
  });

  it("suppresses the empty state and Next-up when an empty `activeNow` WAS cut", async () => {
    // Reachable exactly as the adapter documents: every session inside the
    // served window carries a snapshot or log this version cannot replay, so
    // the raw count exceeded the bound and the served list is empty.
    mockGet.mockResolvedValue(
      resp({
        applied: { sort: "time", truncated: ["activeNow"] },
        activeNow: [],
        upcoming: [net({ id: "u1", occurrenceId: "o1", title: "Later Net" })],
      }),
    );
    renderDiscovery();

    expect(await screen.findByTestId("active-now-truncated")).toBeInTheDocument();
    expect(screen.queryByTestId("active-now-empty")).toBeNull();
    // `nextUp`'s whole premise is that nothing is on the air. It is not.
    expect(screen.queryByTestId("next-up")).toBeNull();
  });

  it("suppresses the empty state when an empty `upcoming` WAS cut", async () => {
    mockGet.mockResolvedValue(
      resp({
        applied: { sort: "time", truncated: ["upcoming"] },
        activeNow: [net({ id: "live-1", occurrenceId: "s-1" })],
        upcoming: [],
      }),
    );
    renderDiscovery();

    expect(await screen.findByTestId("upcoming-truncated")).toBeInTheDocument();
    expect(screen.queryByTestId("upcoming-empty")).toBeNull();
  });

  it("makes no statement for a collection name it does not recognise", async () => {
    // A well-formed array of UNRECOGNISED names. Deliberately silent, and the
    // reason is structural rather than an oversight: the applied-filters list
    // is FLAT, so an unknown key can always be appended to it, while a
    // truncation statement is POSITIONAL — it belongs beside the panel it
    // describes — and an unrecognised collection has no panel to sit beside.
    // Pinned so the silence stays a decision.
    mockGet.mockResolvedValue(
      resp({
        applied: { sort: "time", truncated: ["featured", "ActiveNow"] },
        activeNow: [net({ id: "live-1", occurrenceId: "s-1" })],
        upcoming: [net({})],
      }),
    );
    renderDiscovery();

    await screen.findByTestId("applied-summary");
    expect(screen.queryByTestId("active-now-truncated")).toBeNull();
    expect(screen.queryByTestId("upcoming-truncated")).toBeNull();
    // Case-sensitive on purpose: the wire names are literals, not a vocabulary
    // the client normalises.
    expect(screen.getByTestId("live-count")).toHaveTextContent(/^1$/);
  });

  describe("a net title opens the net", () => {
    // Four render sites, four DISTINCT tokens: a copy-pasted href from one site
    // cannot satisfy another's assertion. `getByRole("link", …)` is the whole
    // assertion — it fails on a `<div onClick>` and passes on `<a href>`.
    const liveFixture = (): DiscoveryResponse =>
      resp({
        activeNow: [
          net({
            id: "live-1",
            occurrenceId: "occ-live-1",
            title: "Live Emergency Net",
            linkToken: "tok-live-featured",
          }),
          net({
            id: "live-2",
            occurrenceId: "occ-live-2",
            title: "Cascade Morning ARES Net",
            linkToken: "tok-live-compact",
          }),
        ],
      });

    const upcomingFixture = (): DiscoveryResponse =>
      resp({
        upcoming: [
          net({
            id: "u1",
            occurrenceId: "o1",
            title: "Alpha Net",
            linkToken: "tok-upcoming-alpha",
          }),
          net({
            id: "u2",
            occurrenceId: "o2",
            title: "Bravo Net",
            linkToken: "tok-upcoming-bravo",
          }),
        ],
      });

    it("links the featured live card's title to /nets/t/{linkToken} and keeps its heading", async () => {
      mockGet.mockResolvedValue(liveFixture());
      renderDiscovery();

      const link = await screen.findByRole("link", { name: "Live Emergency Net" });
      expect(link).toHaveAttribute("href", "/nets/t/tok-live-featured");
      // `<h3><a>…</a></h3>`: the page outline keeps its heading.
      expect(
        screen.getByRole("heading", { level: 3, name: "Live Emergency Net" }),
      ).toBeInTheDocument();
    });

    it("links the compact live row's title to /nets/t/{linkToken}", async () => {
      mockGet.mockResolvedValue(liveFixture());
      renderDiscovery();

      const link = await screen.findByRole("link", {
        name: "Cascade Morning ARES Net",
      });
      expect(link).toHaveAttribute("href", "/nets/t/tok-live-compact");
    });

    it("links the upcoming row's title to /nets/t/{linkToken}", async () => {
      mockGet.mockResolvedValue(upcomingFixture());
      renderDiscovery();

      // Bravo is the SECOND upcoming net, so it is on no next-up strip: the
      // only element with this name is the Upcoming row's title.
      await screen.findByText("Bravo Net");
      const link = inUpcoming().getByRole("link", { name: "Bravo Net" });
      expect(link).toHaveAttribute("href", "/nets/t/tok-upcoming-bravo");
    });

    it("links the next-up strip's title to /nets/t/{linkToken}", async () => {
      mockGet.mockResolvedValue(upcomingFixture());
      renderDiscovery();

      // Alpha is upcoming[0], so it renders twice — the Upcoming row AND the
      // next-up strip. Scoping to the strip is what makes this the fourth site.
      const strip = await screen.findByTestId("next-up");
      const link = within(strip).getByRole("link", { name: "Alpha Net" });
      expect(link).toHaveAttribute("href", "/nets/t/tok-upcoming-alpha");
    });

    it("is a focusable real link at every site that leaves colour and underline to the stylesheet", async () => {
      // RE-CUT after the shipped version asserted the defect instead of
      // catching it.
      //
      // The bug: the resting look was an INLINE `color`/`textDecoration` on
      // every anchor, and an inline declaration outranks any author stylesheet
      // without `!important` — so `.net-title-link:hover` set exactly the two
      // properties that were already pinned inline, and no title ever changed
      // on hover. The old assertions re-stated that inline object verbatim and
      // were green throughout.
      //
      // jsdom loads no stylesheet, so the `:hover` itself cannot be asserted
      // here and this test does not pretend to. What it asserts is the half
      // the COMPONENT owns and the half that actually broke: the anchor
      // carries the class, and it leaves those two properties alone so the
      // rule can reach them. Re-introducing an inline colour reds this.
      mockGet.mockResolvedValue(liveFixture());
      renderDiscovery();
      const featured = await screen.findByRole("link", { name: "Live Emergency Net" });
      const compact = screen.getByRole("link", { name: "Cascade Morning ARES Net" });

      for (const link of [featured, compact]) {
        link.focus();
        expect(link).toHaveFocus();
        expect(link).toHaveClass("net-title-link");
        expect(link.style.color).toBe("");
        expect(link.style.textDecoration).toBe("");
      }

      mockGet.mockResolvedValue(upcomingFixture());
      cleanupAndRerender();
      const strip = await screen.findByTestId("next-up");
      const nextUp = within(strip).getByRole("link", { name: "Alpha Net" });
      const row = inUpcoming().getByRole("link", { name: "Bravo Net" });
      for (const link of [nextUp, row]) {
        link.focus();
        expect(link).toHaveFocus();
        expect(link).toHaveClass("net-title-link");
        expect(link.style.color).toBe("");
        expect(link.style.textDecoration).toBe("");
      }
    });

    it("leaves the next-up strip's title at the muted colour it inherited before it was a link", async () => {
      // The OTHER half — the resting look that must NOT change — and the
      // regression the first fix nearly shipped.
      // `nextUpStyle` sets `color: var(--text-muted)` on the strip, and this
      // title was a `<span>` carrying only `fontWeight`, so it INHERITED the
      // muted colour. An anchor that names its own colour — inline or via a
      // token in the class — silently un-mutes it. `color: inherit` in
      // `index.css` is what keeps it, and the guard here is that the element
      // contributes no colour of its own at this site either.
      mockGet.mockResolvedValue(upcomingFixture());
      renderDiscovery();

      const strip = await screen.findByTestId("next-up");
      const link = within(strip).getByRole("link", { name: "Alpha Net" });
      expect(link.style.color).toBe("");
      expect(strip).toHaveStyle({ color: "var(--text-muted)" });
    });

    it("has a stylesheet rule for the exact class its anchors carry, resting and hovered", async () => {
      // A gap no other gate reaches: the component names the class through
      // `NET_TITLE_LINK_CLASS` and `index.css` names it in an unrelated string
      // literal, with nothing tying the two together. Misspell the selector,
      // delete either rule, or rename `--accent-ink`, and `npm test`, `oxlint`
      // and `npm run build` all stay green while no title changes on hover —
      // which is the same failure mode the hover assertion closes, displaced
      // from the component out to the stylesheet.
      //
      // The class is read off the RENDERED anchor rather than hardcoded here,
      // so this reds from either direction: renaming the constant moves the
      // DOM class and the lookup misses, and renaming the selector misses too.
      // Asserting a CSS selector and a token name is structure, not prose, so
      // the house rule against pinning rendered text does not reach it.
      mockGet.mockResolvedValue(liveFixture());
      renderDiscovery();
      const link = await screen.findByRole("link", { name: "Live Emergency Net" });

      const className = link.className;
      expect(className).not.toBe("");

      const css = stylesheet("../../index.css");
      const restingRule = new RegExp(`\\.${className}\\s*\\{([^}]*)\\}`);
      const hoverRule = new RegExp(`\\.${className}:hover\\s*\\{([^}]*)\\}`);

      const resting = restingRule.exec(css);
      expect(resting).not.toBeNull();
      // `color: inherit` is what keeps the next-up strip muted and beats the
      // UA's link blue; no underline at rest is the other half.
      expect(resting?.[1]).toContain("color: inherit");
      expect(resting?.[1]).toContain("text-decoration: none");

      const hover = hoverRule.exec(css);
      expect(hover).not.toBeNull();
      expect(hover?.[1]).toContain("var(--accent-ink)");
      expect(hover?.[1]).toContain("text-decoration: underline");
      // Never pure `accent`: DESIGN.md reserves it for live/brand and it fails
      // AA as text on white.
      expect(hover?.[1]).not.toMatch(/var\(--accent\)/);

      // The token the hover resolves has to exist, or the rule is inert.
      // `tokens-css.test.ts` already welds tokens.css to `cssVariables()`; this
      // asserts the ONE token this rule resolves, so renaming it reds here too.
      expect(stylesheet("../../ui/tokens/tokens.css")).toContain("--accent-ink:");
    });

    it("underlines the title at rest where no pointer can hover", async () => {
      // `:hover` never fires on a touch device, so the hover WAS the whole
      // affordance for four links on the busiest public page. jsdom applies no
      // stylesheet and cannot evaluate a media query, so the assertion is that
      // the rule exists and targets the class the anchors actually carry —
      // the same coupling the test above establishes.
      mockGet.mockResolvedValue(liveFixture());
      renderDiscovery();
      const link = await screen.findByRole("link", { name: "Live Emergency Net" });

      const css = stylesheet("../../index.css");
      const noHoverBlock = /@media\s*\(hover:\s*none\)\s*\{([\s\S]*?)\n\}/.exec(css);
      expect(noHoverBlock).not.toBeNull();
      expect(noHoverBlock?.[1]).toContain(`.${link.className}`);
      expect(noHoverBlock?.[1]).toContain("text-decoration: underline");
    });
  });
});

describe("the connection cell stops overrunning", () => {
  // No `hf`, no `repeater`, so `rfConnection` returns null and the summary
  // takes the unbounded branch — the ONE shape that reds on the unfixed code.
  // An RF fixture is green before and after the fix and proves nothing.
  const FOUR_WAYS: NetConnection[] = [
    connection({ id: "c-echo", position: 0, kind: "echolink", node: "12345" }),
    connection({
      id: "c-dmr",
      position: 1,
      kind: "dmr",
      talkgroup: "3100",
      network: "Brandmeister",
    }),
    connection({ id: "c-dstar", position: 2, kind: "dstar", reflector: "REF030C" }),
    connection({ id: "c-ysf", position: 3, kind: "ysf", reflector: "FCS001-99" }),
  ];

  const internetOnlyNet = (
    overrides: Partial<DiscoveryNet> = {},
  ): DiscoveryNet =>
    net({
      title: "Internet Only Net",
      linkToken: "tok-internet",
      connections: FOUR_WAYS,
      ...overrides,
    });

  // Scoped to the Upcoming panel, not `screen`: the on-air panel's next-up
  // pointer deliberately repeats the soonest net's title, so an unscoped query
  // is ambiguous by design (the `inUpcoming` docstring says so).
  const upcomingRow = async (title: string): Promise<HTMLElement> => {
    await screen.findByRole("region", { name: /upcoming/i });
    return inUpcoming().getByText(title).closest("li") as HTMLElement;
  };

  it("renders each way in as its own element rather than one joined run of text", async () => {
    mockGet.mockResolvedValue(resp({ upcoming: [internetOnlyNet()] }));
    renderDiscovery();
    const row = await upcomingRow("Internet Only Net");

    expect(within(row).getAllByTestId("connection-line")).toHaveLength(3);
  });

  it("caps the cell below the number of ways the net actually has", async () => {
    mockGet.mockResolvedValue(resp({ upcoming: [internetOnlyNet()] }));
    renderDiscovery();
    const row = await upcomingRow("Internet Only Net");

    const lines = within(row).getAllByTestId("connection-line");
    expect(lines.length).toBeLessThan(FOUR_WAYS.length);
    // Each line is a DIFFERENT way in, in the owner's order — a cap that
    // repeats one connection three times satisfies a bare count and nothing
    // else.
    expect(new Set(lines.map((l) => l.textContent)).size).toBe(lines.length);
  });

  it("counts the remainder rather than saying only 'more'", async () => {
    mockGet.mockResolvedValue(resp({ upcoming: [internetOnlyNet()] }));
    renderDiscovery();
    const row = await upcomingRow("Internet Only Net");

    const overflow = within(row).getByTestId("connection-overflow");
    const shown = within(row).getAllByTestId("connection-line").length;
    // The NUMBER is the assertion, derived from the fixture rather than copied
    // from the copy — a sentence rewrite must not red this, and a hardcoded
    // "+1 more" would pin prose (project-context: tests assert behaviour).
    const remainder = FOUR_WAYS.length - shown;
    expect(overflow.textContent).toMatch(new RegExp(`\\b${remainder}\\b`));
  });

  it("makes the remainder a real link to the net, not a session", async () => {
    mockGet.mockResolvedValue(resp({ upcoming: [internetOnlyNet()] }));
    renderDiscovery();
    const row = await upcomingRow("Internet Only Net");

    const overflow = within(row).getByTestId("connection-overflow");
    expect(overflow.tagName).toBe("A");
    expect(overflow).toHaveAttribute("href", "/nets/t/tok-internet");
    // `/live/{occurrenceId}` is a SESSION; the remainder belongs to the NET.
    expect(overflow.getAttribute("href")).not.toContain("/live/");
  });

  it("shows no affordance when every way in already fits", async () => {
    mockGet.mockResolvedValue(
      resp({
        upcoming: [
          internetOnlyNet({
            title: "Three Ways Net",
            linkToken: "tok-three",
            connections: FOUR_WAYS.slice(0, 3),
          }),
        ],
      }),
    );
    renderDiscovery();
    const row = await upcomingRow("Three Ways Net");

    expect(within(row).getAllByTestId("connection-line")).toHaveLength(3);
    expect(within(row).queryByTestId("connection-overflow")).toBeNull();
  });

  it("stops forbidding the pill to wrap", async () => {
    mockGet.mockResolvedValue(resp({ upcoming: [internetOnlyNet()] }));
    renderDiscovery();
    const row = await upcomingRow("Internet Only Net");

    // The containment measured in real chromium: `nowrap` off is one of the two
    // moves that actually bound the cell. jsdom has no layout engine, so this
    // asserts the MECHANISM; the Playwright spec asserts the outcome.
    // Located through the SHIPPED element rather than a new test id, so this
    // reds on the property (`nowrap` is set today) instead of on a missing
    // hook — a red that only says "the id is not there yet" is not evidence
    // about wrapping.
    const pill = within(row).getByTestId("connection-summary")
      .parentElement as HTMLElement;
    expect(pill.style.whiteSpace).not.toBe("nowrap");
    expect(pill.style.overflowWrap).toBe("anywhere");
  });

  it("leaves the other pill sites showing every way in", async () => {
    // The cap is opt-in at the ONE site in a fixed track. The featured live
    // card has room and no fixed column; capping it would hide facts to solve
    // a problem it does not have.
    mockGet.mockResolvedValue(
      resp({
        activeNow: [
          internetOnlyNet({
            title: "Live Internet Net",
            occurrenceId: "occ-live",
            linkToken: "tok-live",
          }),
        ],
      }),
    );
    renderDiscovery();
    const summary = await screen.findByTestId("connection-summary");

    expect(within(summary).queryAllByTestId("connection-line")).toHaveLength(0);
    expect(
      screen.queryByTestId("connection-overflow"),
    ).toBeNull();
  });

  it("names each affordance by its own net rather than repeating '+1 more' down the page", async () => {
    mockGet.mockResolvedValue(
      resp({
        upcoming: [
          internetOnlyNet(),
          internetOnlyNet({
            id: "def-2",
            occurrenceId: "occ-2",
            title: "Second Internet Net",
            linkToken: "tok-second",
          }),
        ],
      }),
    );
    renderDiscovery();
    await screen.findByRole("region", { name: /upcoming/i });

    const names = inUpcoming()
      .getAllByTestId("connection-overflow")
      .map((a: HTMLElement) => a.getAttribute("aria-label"));
    expect(names).toHaveLength(2);
    expect(new Set(names).size).toBe(2);
    for (const [index, title] of [
      "Internet Only Net",
      "Second Internet Net",
    ].entries()) {
      expect(names[index]).toContain(title);
    }
  });

  it("still tells two affordances apart when the nets share a title", async () => {
    // Titles are not unique — nothing stops two owners from running a
    // "Sunday Rag Chew". A name built from the title alone collides, which is
    // exactly the repetition the naming exists to prevent.
    mockGet.mockResolvedValue(
      resp({
        upcoming: [
          internetOnlyNet({ title: "Sunday Rag Chew" }),
          internetOnlyNet({
            id: "def-2",
            occurrenceId: "occ-2",
            title: "Sunday Rag Chew",
            linkToken: "tok-second",
          }),
        ],
      }),
    );
    renderDiscovery();
    await screen.findByRole("region", { name: /upcoming/i });

    const names = inUpcoming()
      .getAllByTestId("connection-overflow")
      .map((a: HTMLElement) => a.getAttribute("aria-label"));
    expect(names).toHaveLength(2);
    expect(new Set(names).size).toBe(2);
    // Distinct, and each still says which net it leads into.
    for (const name of names) {
      expect(name).toContain("Sunday Rag Chew");
    }
  });

  it("keeps the filter mark tellable from the affordance on a capped, matched net", async () => {
    // The fixture FINDING 9 asks for: internet-only, more ways in than fit,
    // AND a server-matched connection — so the mark and the affordance are
    // tenants of the same 150px at the same time.
    mockGet.mockResolvedValue(
      resp({
        upcoming: [internetOnlyNet({ matchedConnectionId: "c-dmr" })],
        applied: { sort: "time" },
      }),
    );
    const { container } = renderDiscovery(["/?kind=dmr"]);
    const row = await upcomingRow("Internet Only Net");

    const marked = within(row).getByTestId("matched-connection");
    // The PREFIX, never a fixture-specific equality: it holds for every set,
    // where an equality goes quietly vacuous on a net whose summary and
    // matched description render the same string.
    expect(marked.getAttribute("aria-label")).toMatch(
      /^Matches your filter: \S/,
    );
    expect(marked.getAttribute("aria-label")).not.toBe(
      within(row).getByTestId("connection-overflow").getAttribute("aria-label"),
    );

    // A green axe run is NOT the evidence for either naming property above —
    // axe has no rule for elements sharing an accessible name. It is here to
    // catch what it does cover: a link with no name, a bad role nesting.
    await expectNoAxeViolations(container);
  });

  it("stacks the mark under the summary instead of beside it in the fixed track", async () => {
    mockGet.mockResolvedValue(
      resp({ upcoming: [internetOnlyNet({ matchedConnectionId: "c-dmr" })] }),
    );
    renderDiscovery(["/?kind=dmr"]);
    const row = await upcomingRow("Internet Only Net");

    // Two side-by-side blocks in a 150px track is the overrun in miniature.
    const pill = within(row).getByTestId("connection-cell");
    expect(pill.style.flexDirection).toBe("column");
    expect(pill.style.alignItems).toBe("flex-start");
  });

  it("leaves the unbounded sites on a row, not a column", async () => {
    mockGet.mockResolvedValue(
      resp({
        activeNow: [
          internetOnlyNet({
            title: "Live Internet Net",
            occurrenceId: "occ-live",
            linkToken: "tok-live",
          }),
        ],
      }),
    );
    renderDiscovery();
    const pill = await screen.findByTestId("connection-cell");

    expect(pill.style.flexDirection).toBe("");
  });

  // ── The three UNBOUNDED sites, one test each ──────────────────────────────
  //
  // `freqPillStyle` itself changed — the object all four call sites share
  // — to contain the ONE capped pill that is a grid item in a fixed track.
  // `overflowWrap: "anywhere"`, `minWidth` and `maxWidth` therefore reached the
  // featured card, the compact live row and the next-up strip too. That was a
  // deliberate consistency choice (the object's own comments say so), but only
  // the featured card had a test standing under it, so two of the three sites
  // were carrying an untested style change. These pin what each site must still
  // render, so a later edit to the shared object cannot quietly turn an
  // unbounded site into a capped one.

  /** The block a net's title anchor shares with its connection pill. One climb
   * reaches it at every unbounded site — the featured card's heading wrapper,
   * the compact row's title block, the next-up strip — so a site is named by
   * the TITLE it renders rather than by an index into every pill on the page,
   * which would silently follow the wrong pill if a fixture gained a net. */
  const pillSiteFor = (link: HTMLElement): HTMLElement =>
    link.closest("div") as HTMLElement;

  /** What an UNBOUNDED site must render: the whole set as ONE joined run of
   * text, no per-line spans (that shape belongs to the capped row alone), and a
   * pill wearing `freqPillStyle` rather than the column `boundedPillStyle`.
   *
   * The expected text comes from `connectionSummary`, the shipped one-line
   * presenter, which the pill does NOT call for its summary — it joins
   * `connectionSummaryLines` itself. So the equality is a cross-check between
   * the two shipped forms rather than a restatement of the pill's own
   * expression, and it reds if either drifts. */
  const expectUnboundedPill = (
    site: HTMLElement,
    connections: readonly NetConnection[],
  ): void => {
    const summary = within(site).getByTestId("connection-summary");
    expect(summary.textContent).toBe(connectionSummary(connections));
    // The premise: this fixture really does have enough ways in for the join
    // to be exercised, so the equality above is not vacuous on one line.
    expect(summary.textContent).toContain(" · ");
    expect(within(site).queryAllByTestId("connection-line")).toHaveLength(0);
    expect(within(site).queryByTestId("connection-overflow")).toBeNull();

    const pill = within(site).getByTestId("connection-cell");
    // `boundedPillStyle` is exactly `freqPillStyle` plus these two. Both are
    // asserted: `flexDirection` alone would also hold for an object that had
    // dropped `alignItems: "center"` on its way past.
    expect(pill.style.flexDirection).not.toBe("column");
    expect(pill.style.alignItems).toBe("center");
    // The shared property does reach here, and that is the intent —
    // stated as an assertion so removing it from the shared object is a
    // decision with a test in front of it, not a side effect.
    expect(pill.style.overflowWrap).toBe("anywhere");
  };

  it("shows the whole set as one joined line on the featured live card", async () => {
    mockGet.mockResolvedValue(
      resp({
        activeNow: [
          internetOnlyNet({
            title: "Featured Internet Net",
            occurrenceId: "occ-featured",
            linkToken: "tok-featured",
          }),
        ],
      }),
    );
    renderDiscovery();
    const link = await screen.findByRole("link", {
      name: "Featured Internet Net",
    });

    expectUnboundedPill(pillSiteFor(link), FOUR_WAYS);
  });

  it("shows the whole set as one joined line on a compact live row", async () => {
    // Two live nets, so the internet-only set lands on the SECOND — the only
    // shape that renders a `CompactLiveRow` at all, since `activeNow[0]` is
    // always the featured card.
    mockGet.mockResolvedValue(
      resp({
        activeNow: [
          net({
            id: "live-featured",
            occurrenceId: "occ-featured",
            title: "Live Emergency Net",
            linkToken: "tok-featured",
          }),
          internetOnlyNet({
            id: "live-compact",
            occurrenceId: "occ-compact",
            title: "Compact Internet Net",
            linkToken: "tok-compact",
          }),
        ],
      }),
    );
    renderDiscovery();
    const link = await screen.findByRole("link", {
      name: "Compact Internet Net",
    });
    // The premise: this really is the compact row and not the featured card,
    // which is a heading at its own site.
    expect(
      screen.getByRole("heading", { level: 3, name: "Live Emergency Net" }),
    ).toBeInTheDocument();

    expectUnboundedPill(pillSiteFor(link), FOUR_WAYS);
  });

  it("shows the whole set as one joined line on the next-up strip while the same net's upcoming row stays capped", async () => {
    // One net at TWO sites in one render: `upcoming[0]` is the next-up strip's
    // net as well as the first Upcoming row. The strip shows every way in; the
    // row, in its fixed track, caps. Asserting both here is what makes this a
    // test of the site rather than of the fixture — a shared object that
    // bounded everything would still pass a strip-only assertion about lines
    // being absent.
    mockGet.mockResolvedValue(resp({ upcoming: [internetOnlyNet()] }));
    renderDiscovery();
    const strip = await screen.findByTestId("next-up");

    expectUnboundedPill(strip, FOUR_WAYS);

    const row = await upcomingRow("Internet Only Net");
    expect(
      within(row).getAllByTestId("connection-line").length,
    ).toBeLessThan(FOUR_WAYS.length);
    expect(within(row).getByTestId("connection-cell").style.flexDirection).toBe(
      "column",
    );
  });
});
