// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
import { render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";
import { RouterProvider, createMemoryRouter } from "react-router";

import { messageForProblemType } from "../../errors/problemMessages";
import { expectNoAxeViolations } from "../../test/axe";
import { MyNetsPage } from "./MyNetsPage";

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

const account = {
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

const activeFavorite = {
  id: "def-1",
  title: "Sunday Traffic Net",
  description: null,
  connections: [
    {
      id: "conn-fav-0",
      position: 0,
      kind: "hf",
      plannedFrequencyHz: 14_230_000,
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
    },
  ],
  country: null,
  state: null,
  grid: null,
  netCategory: "traffic",
  netType: "open",
  expectedDurationMinutes: null,
  linkToken: "tok-active",
  archivedAt: null,
  favoritedAt: "2026-07-16T00:00:00+00:00",
};

const archivedFavorite = {
  ...activeFavorite,
  id: "def-2",
  title: "Retired Emergency Net",
  linkToken: "tok-archived",
  archivedAt: "2026-07-16T12:00:00+00:00",
};

/** One served connection, flat with `null`s exactly as the API serves it. */
function connection(overrides: Record<string, unknown>): Record<string, unknown> {
  return {
    id: "conn-0",
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

const liveOwnedNet = {
  id: "own-1",
  definitionVersion: 1,
  title: "Tuesday Night Traffic Net",
  description: null,
  country: null,
  state: null,
  grid: null,
  netCategory: "traffic",
  netType: "roll-call",
  expectedDurationMinutes: null,
  visibility: "listed",
  connections: [
    connection({
      kind: "hf",
      plannedFrequencyHz: 7_238_000,
      band: "40m",
      mode: "ssb",
    }),
  ],
  linkToken: "tok-own-1",
  ownerAccountIds: ["acct-1"],
  owners: [{ accountId: "acct-1", callsign: "W1AW" }],
  archivedAt: null,
  createdAt: "2026-07-16T00:00:00Z",
  updatedAt: "2026-07-16T00:00:00Z",
  liveSessionId: "sess-live-1",
  nextOccurrenceAt: null,
};

const scheduledOwnedNet = {
  ...liveOwnedNet,
  id: "own-2",
  title: "Weekend QRP Roundtable",
  plannedFrequencyHz: 14_060_000,
  band: "20m",
  mode: "cw",
  connections: [
    connection({
      kind: "hf",
      plannedFrequencyHz: 14_060_000,
      band: "20m",
      mode: "cw",
    }),
  ],
  netCategory: "social",
  netType: "open",
  linkToken: "tok-own-2",
  liveSessionId: null,
  nextOccurrenceAt: "2027-01-09T14:00:00Z",
};

interface MockOptions {
  signedIn?: boolean;
  favorites?: unknown[];
  /** Answers every `GET /api/favorites[?…]`, receiving the URL so a paging
   * test can branch on the cursor it carries. */
  favoritesResponder?: (url: string) => Promise<Response>;
  ownedNets?: unknown[];
  ownedResponder?: (url: string) => Promise<Response>;
  onDelete?: (url: string) => void;
  onStartSession?: (body: unknown) => void;
}

function renderPage(opts: MockOptions = {}): {
  router: ReturnType<typeof createMemoryRouter>;
  container: HTMLElement;
} {
  const {
    signedIn = true,
    favorites = [],
    favoritesResponder,
    ownedNets = [],
    ownedResponder,
    onDelete,
    onStartSession,
  } = opts;
  const fetchMock = vi.fn(async (url: string, init?: RequestInit) => {
    const method = init?.method ?? "GET";
    if (url === "/api/accounts/me") {
      return signedIn
        ? jsonResponse(200, account)
        : jsonResponse(401, { type: "/errors/unauthenticated", status: 401 });
    }
    if ((url === "/api/favorites" || url.startsWith("/api/favorites?")) && method === "GET") {
      return favoritesResponder
        ? favoritesResponder(url)
        : jsonResponse(200, { items: favorites, nextCursor: null });
    }
    if (url.startsWith("/api/favorites/") && method === "DELETE") {
      onDelete?.(url);
      return noContent();
    }
    if (
      (url === "/api/net-definitions" || url.startsWith("/api/net-definitions?")) &&
      method === "GET"
    ) {
      return ownedResponder
        ? ownedResponder(url)
        : jsonResponse(200, { items: ownedNets, nextCursor: null });
    }
    if (url === "/api/net-sessions" && method === "POST") {
      onStartSession?.(JSON.parse(init?.body as string));
      return jsonResponse(201, { id: "sess-new-1" });
    }
    return jsonResponse(404, { type: "/errors/not-found", status: 404 });
  });
  vi.stubGlobal("fetch", fetchMock);

  const router = createMemoryRouter(
    [
      { path: "/my-nets", element: <MyNetsPage /> },
      { path: "/sign-in", element: <div data-testid="sign-in" /> },
      { path: "/", element: <div data-testid="discovery" /> },
      { path: "/net-sessions/:id", element: <div data-testid="net-session-page" /> },
    ],
    { initialEntries: ["/my-nets"] },
  );
  const { container } = render(<RouterProvider router={router} />);
  return { router, container };
}

async function goToFavoritesTab(): Promise<void> {
  await userEvent.click(await screen.findByRole("tab", { name: /favorites/i }));
}

afterEach(() => {
  vi.unstubAllGlobals();
});

describe("MyNetsPage", () => {
  it("redirects a signed-out visitor to /sign-in (self-gate)", async () => {
    const { router } = renderPage({ signedIn: false });
    await waitFor(() =>
      expect(router.state.location.pathname).toBe("/sign-in"),
    );
  });

  it("renders an Owned/Favorites tablist with the Owned tab active by default", async () => {
    renderPage({ favorites: [activeFavorite], ownedNets: [liveOwnedNet] });

    const tablist = await screen.findByRole("tablist", { name: /my nets/i });
    expect(tablist).toBeInTheDocument();
    const ownedTab = await screen.findByRole("tab", { name: /owned/i });
    const favoritesTab = screen.getByRole("tab", { name: /favorites/i });
    expect(ownedTab).toHaveAttribute("aria-selected", "true");
    expect(favoritesTab).toHaveAttribute("aria-selected", "false");
  });

  it("shows a count badge on each tab once its data has loaded", async () => {
    renderPage({
      favorites: [activeFavorite, archivedFavorite],
      ownedNets: [liveOwnedNet],
    });

    const ownedTab = await screen.findByRole("tab", { name: /owned/i });
    await waitFor(() => expect(ownedTab).toHaveTextContent("1"));
    const favoritesTab = await screen.findByRole("tab", { name: /favorites/i });
    await waitFor(() => expect(favoritesTab).toHaveTextContent("2"));
  });

  it("switches the visible panel when the Favorites tab is activated", async () => {
    renderPage({ favorites: [activeFavorite], ownedNets: [liveOwnedNet] });

    await screen.findByText("Tuesday Night Traffic Net");
    expect(screen.queryByText("Sunday Traffic Net")).not.toBeInTheDocument();

    await goToFavoritesTab();

    expect(await screen.findByText("Sunday Traffic Net")).toBeInTheDocument();
    expect(screen.queryByText("Tuesday Night Traffic Net")).not.toBeInTheDocument();
  });

  it("has a New net call-to-action in the header linking to /nets/new", async () => {
    renderPage();
    const link = await screen.findByRole("link", { name: /new net/i });
    expect(link).toHaveAttribute("href", "/nets/new");
  });

  it("shows an empty-owned message with a link to create one when there are no owned nets", async () => {
    renderPage({ ownedNets: [] });
    const link = await screen.findByRole("link", { name: /create one/i });
    expect(link).toHaveAttribute("href", "/nets/new");
  });

  it("renders a live owned net with a Live badge and an Open console link to its session", async () => {
    renderPage({ ownedNets: [liveOwnedNet] });

    const card = (await screen.findByText("Tuesday Night Traffic Net")).closest(
      "[data-testid='owned-net-card']",
    ) as HTMLElement;
    expect(within(card).getByText(/live/i)).toBeInTheDocument();
    const openConsole = within(card).getByRole("link", { name: /open console/i });
    expect(openConsole).toHaveAttribute("href", "/net-sessions/sess-live-1");
    const edit = within(card).getByRole("link", { name: /^edit$/i });
    expect(edit).toHaveAttribute("href", "/nets/own-1/edit");
  });

  it("flattens the net list into hairline panel rows instead of cards inside a card", async () => {
    renderPage({ ownedNets: [scheduledOwnedNet] });

    const row = (await screen.findByText("Weekend QRP Roundtable")).closest(
      "[data-testid='owned-net-card']",
    ) as HTMLElement;

    // A surface-2 card per net, nested inside the page's own card, read as two
    // competing frames; rows now sit flush in one panel, divided by hairlines.
    expect(row.style.background).toBe("");
    expect(row.getAttribute("style")).toContain(
      "border-bottom: 1px solid var(--border)",
    );
    // Anti-bento (DESIGN.md Do/Don't): exactly one elevated frame per page.
    expect(document.querySelectorAll("section[style*='box-shadow']")).toHaveLength(1);
  });

  it("keeps the live net's accent left-bar through the flattening", async () => {
    renderPage({ ownedNets: [liveOwnedNet] });

    const row = (await screen.findByText("Tuesday Night Traffic Net")).closest(
      "[data-testid='owned-net-card']",
    ) as HTMLElement;

    // The 4px left bar is the design system's state language (DESIGN.md
    // § Elevation) — it carries "live" alongside the badge, so removing the
    // per-row card must not take it with it.
    expect(row.getAttribute("style")).toContain(
      "border-left: var(--space-status-bar) solid var(--accent)",
    );
  });

  it("bands the page header on the tonal gradient at the shared measure", async () => {
    renderPage();

    await screen.findByRole("heading", { name: /my nets/i });
    expect(screen.getByRole("main")).toHaveStyle({ maxWidth: "1200px" });
    expect(
      document.querySelector("[data-page-header]"),
    ).toHaveStyle({ background: "var(--head-grad)" });
  });

  it("renders a non-live owned net with a Start net action instead of a live badge", async () => {
    renderPage({ ownedNets: [scheduledOwnedNet] });

    const card = (await screen.findByText("Weekend QRP Roundtable")).closest(
      "[data-testid='owned-net-card']",
    ) as HTMLElement;
    expect(within(card).queryByText(/^live$/i)).not.toBeInTheDocument();
    expect(within(card).getByRole("button", { name: /start net/i })).toBeInTheDocument();
    expect(within(card).getByRole("link", { name: /^edit$/i })).toHaveAttribute(
      "href",
      "/nets/own-2/edit",
    );
    expect(within(card).getByRole("button", { name: /share link/i })).toBeInTheDocument();
  });

  it("starts a session and navigates to its console when Start net succeeds", async () => {
    const started: unknown[] = [];
    const { router } = renderPage({
      ownedNets: [scheduledOwnedNet],
      onStartSession: (body) => started.push(body),
    });

    const card = (await screen.findByText("Weekend QRP Roundtable")).closest(
      "[data-testid='owned-net-card']",
    ) as HTMLElement;
    await userEvent.click(within(card).getByRole("button", { name: /start net/i }));

    await waitFor(() =>
      expect(router.state.location.pathname).toBe("/net-sessions/sess-new-1"),
    );
    // The start body is the definition id and nothing else — the
    // session freezes the net's whole connection list rather than one number.
    expect(started).toEqual([{ definitionId: "own-2" }]);
  });

  it("copies the share link to the clipboard when Share link is clicked", async () => {
    const writeText = vi.fn();
    Object.defineProperty(navigator, "clipboard", {
      value: { writeText },
      configurable: true,
    });
    renderPage({ ownedNets: [scheduledOwnedNet] });

    const card = (await screen.findByText("Weekend QRP Roundtable")).closest(
      "[data-testid='owned-net-card']",
    ) as HTMLElement;
    await userEvent.click(within(card).getByRole("button", { name: /share link/i }));

    expect(writeText).toHaveBeenCalledWith(
      expect.stringContaining("/nets/t/tok-own-2"),
    );
  });

  it("shows the empty state with a link to Discovery when there are no favorites", async () => {
    renderPage({ favorites: [] });
    await goToFavoritesTab();
    // The Discovery link target is the load-bearing assertion (not the prose).
    const link = await screen.findByRole("link", { name: /discovery/i });
    expect(link).toHaveAttribute("href", "/");
  });

  it("distinguishes the loading placeholder from a confirmed-empty result", async () => {
    let resolve: (r: Response) => void = () => {};
    const pending = new Promise<Response>((r) => {
      resolve = r;
    });
    renderPage({ favoritesResponder: () => pending });
    await goToFavoritesTab();

    // While the favorites fetch is in flight, a status placeholder shows — NOT
    // the empty-state assertion, which the fetch has not yet established.
    expect(await screen.findByRole("status")).toBeInTheDocument();
    expect(screen.queryByRole("link", { name: /discovery/i })).not.toBeInTheDocument();

    resolve(jsonResponse(200, { items: [], nextCursor: null }));
    expect(await screen.findByRole("link", { name: /discovery/i })).toBeInTheDocument();
  });

  it("lists favorited nets, each linking back to its /nets/t/{token} view", async () => {
    renderPage({ favorites: [activeFavorite, archivedFavorite] });
    await goToFavoritesTab();

    expect(await screen.findByText("Sunday Traffic Net")).toBeInTheDocument();
    expect(screen.getByText("Retired Emergency Net")).toBeInTheDocument();

    const activeLink = screen.getByRole("link", { name: /sunday traffic net/i });
    expect(activeLink).toHaveAttribute("href", "/nets/t/tok-active");
  });

  it("marks an archived favorite with an archived indicator (never silently dropped)", async () => {
    renderPage({ favorites: [activeFavorite, archivedFavorite] });
    await goToFavoritesTab();

    const archivedRow = (await screen.findByText("Retired Emergency Net")).closest("li");
    expect(archivedRow).not.toBeNull();
    expect(within(archivedRow as HTMLElement).getByTestId("archived-badge")).toBeInTheDocument();

    // The active favorite carries no archived indicator.
    const activeRow = screen.getByText("Sunday Traffic Net").closest("li");
    expect(within(activeRow as HTMLElement).queryByTestId("archived-badge")).not.toBeInTheDocument();
  });

  it("renders each favorite's facet line with its connection summary in the freq-tinted mono style", async () => {
    // The segment used to be the definition's flat `band`; it is now the same
    // connection summary the Owned card and the discovery card render, read off
    // the favorite's own connection set.
    renderPage({ favorites: [activeFavorite] });
    await goToFavoritesTab();

    const row = (await screen.findByText("Sunday Traffic Net")).closest("li") as HTMLElement;
    const summary = within(row).getByText(/14\.230 · 20m · ssb/);
    expect(summary).toHaveStyle({ color: "var(--freq-text)" });
  });

  it("shows an error with a Try again control when the load fails", async () => {
    const responses = [
      jsonResponse(500, { type: "/errors/internal", status: 500 }),
      jsonResponse(200, { items: [activeFavorite], nextCursor: null }),
    ];
    renderPage({ favoritesResponder: () => Promise.resolve(responses.shift() as Response) });
    await goToFavoritesTab();

    expect(await screen.findByRole("alert")).toBeInTheDocument();
    const retry = screen.getByRole("button", { name: /try again/i });
    await userEvent.click(retry);
    expect(await screen.findByText("Sunday Traffic Net")).toBeInTheDocument();
  });

  it("removes a row when its favorite is toggled off", async () => {
    const deleted: string[] = [];
    renderPage({
      favorites: [activeFavorite, archivedFavorite],
      onDelete: (url) => deleted.push(url),
    });
    await goToFavoritesTab();

    await screen.findByText("Sunday Traffic Net");
    const activeRow = screen.getByText("Sunday Traffic Net").closest("li") as HTMLElement;
    await userEvent.click(within(activeRow).getByRole("button", { name: "Favorite this net" }));

    await waitFor(() => expect(deleted).toContain("/api/favorites/def-1"));
    await waitFor(() =>
      expect(screen.queryByText("Sunday Traffic Net")).not.toBeInTheDocument(),
    );
    expect(screen.getByText("Retired Emergency Net")).toBeInTheDocument();
  });

  it("shows a favorited net's description inside its row", async () => {
    renderPage({
      favorites: [
        {
          ...activeFavorite,
          description: "Weekly traffic handling for the section.",
        },
      ],
    });
    await goToFavoritesTab();

    const row = (await screen.findByText("Sunday Traffic Net")).closest(
      "li",
    ) as HTMLElement;

    expect(within(row).getAllByTestId("net-description-preview")).toHaveLength(1);
  });

  it("emits no description node at all for a favorited net without one", async () => {
    renderPage({ favorites: [activeFavorite] });
    await goToFavoritesTab();

    const row = (await screen.findByText("Sunday Traffic Net")).closest(
      "li",
    ) as HTMLElement;

    expect(within(row).queryByTestId("net-description-preview")).toBeNull();
  });

  it("shows an owned net's description inside its card", async () => {
    renderPage({
      ownedNets: [
        {
          ...liveOwnedNet,
          description: "Weekly traffic handling for the section.",
        },
      ],
    });

    const card = (await screen.findByText("Tuesday Night Traffic Net")).closest(
      "[data-testid='owned-net-card']",
    ) as HTMLElement;

    expect(within(card).getAllByTestId("net-description-preview")).toHaveLength(1);
  });

  it("emits no description node at all for an owned net without one", async () => {
    renderPage({ ownedNets: [liveOwnedNet] });

    const card = (await screen.findByText("Tuesday Night Traffic Net")).closest(
      "[data-testid='owned-net-card']",
    ) as HTMLElement;

    expect(within(card).queryByTestId("net-description-preview")).toBeNull();
  });

  it("has no accessibility violations on the default (Owned) view", async () => {
    const { container } = renderPage({
      ownedNets: [liveOwnedNet, scheduledOwnedNet],
      favorites: [activeFavorite],
    });
    await screen.findByText("Tuesday Night Traffic Net");
    await expectNoAxeViolations(container);
  });

  it("has no accessibility violations on the Favorites view", async () => {
    const { container } = renderPage({ favorites: [activeFavorite, archivedFavorite] });
    await goToFavoritesTab();
    await screen.findByText("Sunday Traffic Net");
    await expectNoAxeViolations(container);
  });

  it("prefers the server's problem detail over the slug-map fallback", async () => {
    // One of forty-two surfaces. The assertion is the preference
    // ORDER, not the copy. `/errors/internal` is unmapped, so the map half is
    // the generic fallback — still computed, never typed out.
    const detail = "the field-naming answer the server sent";
    renderPage({
      favoritesResponder: () =>
        Promise.resolve(
          jsonResponse(500, {
            type: "/errors/internal",
            status: 500,
            detail,
          }),
        ),
    });
    await goToFavoritesTab();

    const alert = await screen.findByRole("alert");
    expect(alert).toHaveTextContent(detail);
    expect(alert).not.toHaveTextContent(
      messageForProblemType("/errors/internal"),
    );
  });

});

// --- The owned card's meta line comes from the connections --

describe("MyNetsPage connection set", () => {
  it("names an internet-only net's connection instead of its stale flat frequency", async () => {
    renderPage({
      ownedNets: [
        {
          ...liveOwnedNet,
          plannedFrequencyHz: 7_238_000,
          band: "40m",
          connections: [
            connection({ id: "conn-el", kind: "echolink", node: "12345" }),
          ],
        },
      ],
    });

    const card = (await screen.findByText("Tuesday Night Traffic Net")).closest(
      "[data-testid='owned-net-card']",
    ) as HTMLElement;
    const meta = within(card).getByTestId("owned-net-meta");
    expect(meta).not.toHaveTextContent(/7\.238/);
    expect(meta).not.toHaveTextContent(/40m/);
    expect(meta).toHaveTextContent(/12345/);
  });
});

// --- Neither tab presents a page as the whole ---------------

/** A two-page responder for one list read: page 1 carries `cursor`, and the
 * request that echoes it is answered by `secondPage` — a Response, or a promise
 * a test settles itself to hold the page in flight. Counts the cursor requests
 * so a test can assert one press is one request. */
function twoPageResponder(
  firstItems: unknown[],
  cursor: string,
  secondPage: () => Promise<Response>,
): { respond: (url: string) => Promise<Response>; cursorRequests: () => number } {
  let cursorRequests = 0;
  return {
    respond: (url: string) => {
      if (url.includes("cursor=")) {
        cursorRequests += 1;
        expect(url).toContain(`cursor=${encodeURIComponent(cursor)}`);
        return secondPage();
      }
      return Promise.resolve(jsonResponse(200, { items: firstItems, nextCursor: cursor }));
    },
    cursorRequests: () => cursorRequests,
  };
}

const CURSOR = "1754000000123:00000000-0000-7000-8000-0000000000ab";

/** The favorite rows inside the visible tab panel — the breadcrumb is a list
 * too, so an unscoped `listitem` query would count its items. */
function favoriteRows(): HTMLElement[] {
  return within(screen.getByRole("tabpanel")).getAllByRole("listitem");
}

describe("MyNetsPage paging", () => {
  describe("Owned tab", () => {
    it("offers Load more and a `N+` count while a cursor remains, and neither once exhausted", async () => {
      const { respond } = twoPageResponder([liveOwnedNet], CURSOR, () =>
        Promise.resolve(jsonResponse(200, { items: [scheduledOwnedNet], nextCursor: null })),
      );
      renderPage({ ownedResponder: respond });

      const loadMore = await screen.findByRole("button", { name: /load more/i });
      // The badge says the count is a floor, not a total.
      expect(screen.getByRole("tab", { name: /owned/i })).toHaveTextContent(/1\+$/);

      await userEvent.click(loadMore);

      await waitFor(() => expect(screen.getAllByTestId("owned-net-card")).toHaveLength(2));
      expect(screen.queryByRole("button", { name: /load more/i })).not.toBeInTheDocument();
      expect(screen.getByRole("tab", { name: /owned/i })).toHaveTextContent(/2$/);
    });

    it("disables Load more while the page is in flight — the sole double-submit guard", async () => {
      let settle: (r: Response) => void = () => {};
      const pending = new Promise<Response>((r) => {
        settle = r;
      });
      const { respond, cursorRequests } = twoPageResponder([liveOwnedNet], CURSOR, () => pending);
      renderPage({ ownedResponder: respond });

      const loadMore = await screen.findByRole("button", { name: /load more/i });
      expect(loadMore).not.toBeDisabled();
      await userEvent.click(loadMore);
      expect(loadMore).toBeDisabled();
      expect(cursorRequests()).toBe(1);

      settle(jsonResponse(200, { items: [scheduledOwnedNet], nextCursor: null }));
      await waitFor(() => expect(screen.getAllByTestId("owned-net-card")).toHaveLength(2));
      expect(cursorRequests()).toBe(1);
    });

    it("keeps the loaded rows when a page fails, shows the problem beside the button, and re-enables it", async () => {
      const { respond } = twoPageResponder([liveOwnedNet], CURSOR, () =>
        Promise.resolve(jsonResponse(500, { type: "/errors/internal", status: 500 })),
      );
      renderPage({ ownedResponder: respond });

      await userEvent.click(await screen.findByRole("button", { name: /load more/i }));

      expect(await screen.findByRole("alert")).toBeInTheDocument();
      expect(screen.getAllByTestId("owned-net-card")).toHaveLength(1);
      await waitFor(() =>
        expect(screen.getByRole("button", { name: /load more/i })).not.toBeDisabled(),
      );
    });

    it("offers no Load more when the first page is the last", async () => {
      renderPage({ ownedNets: [liveOwnedNet] });
      await screen.findByTestId("owned-net-card");
      expect(screen.queryByRole("button", { name: /load more/i })).not.toBeInTheDocument();
      expect(screen.getByRole("tab", { name: /owned/i })).toHaveTextContent(/1$/);
    });

    it("keeps Load more, and shows no empty state, for an empty page that still carries a cursor", async () => {
      // Legitimate: the server splits the page on the raw rows and THEN skips
      // damaged ones, so a page can be `{ items: [], nextCursor: "…" }`. The rest
      // of the list is still there; "you own none" would be false and the button
      // is the only way to reach it.
      const { respond } = twoPageResponder([], CURSOR, () =>
        Promise.resolve(jsonResponse(200, { items: [liveOwnedNet], nextCursor: null })),
      );
      renderPage({ ownedResponder: respond });

      const loadMore = await screen.findByRole("button", { name: /load more/i });
      expect(screen.queryByTestId("owned-empty")).not.toBeInTheDocument();
      expect(screen.getByRole("tab", { name: /owned/i })).toHaveTextContent(/0\+$/);

      await userEvent.click(loadMore);

      await waitFor(() => expect(screen.getAllByTestId("owned-net-card")).toHaveLength(1));
      expect(screen.queryByRole("button", { name: /load more/i })).not.toBeInTheDocument();
      expect(screen.queryByTestId("owned-empty")).not.toBeInTheDocument();
    });

    it("shows a paging problem beside the button even when no row is loaded yet", async () => {
      const { respond } = twoPageResponder([], CURSOR, () =>
        Promise.resolve(jsonResponse(500, { type: "/errors/internal", status: 500 })),
      );
      renderPage({ ownedResponder: respond });

      await userEvent.click(await screen.findByRole("button", { name: /load more/i }));

      expect(await screen.findByRole("alert")).toBeInTheDocument();
      expect(screen.queryByTestId("owned-empty")).not.toBeInTheDocument();
      await waitFor(() =>
        expect(screen.getByRole("button", { name: /load more/i })).not.toBeDisabled(),
      );
    });

    it("shows the empty state only once the list is exhausted with nothing in it", async () => {
      renderPage({ ownedNets: [] });
      expect(await screen.findByTestId("owned-empty")).toBeInTheDocument();
      expect(screen.queryByRole("button", { name: /load more/i })).not.toBeInTheDocument();
    });
  });

  describe("Favorites tab", () => {
    it("offers Load more and a `N+` count while a cursor remains, and neither once exhausted", async () => {
      const { respond } = twoPageResponder([activeFavorite], CURSOR, () =>
        Promise.resolve(jsonResponse(200, { items: [archivedFavorite], nextCursor: null })),
      );
      renderPage({ favoritesResponder: respond });
      await goToFavoritesTab();

      const loadMore = await screen.findByRole("button", { name: /load more/i });
      expect(screen.getByRole("tab", { name: /favorites/i })).toHaveTextContent(/1\+$/);

      await userEvent.click(loadMore);

      await waitFor(() => expect(favoriteRows()).toHaveLength(2));
      expect(screen.queryByRole("button", { name: /load more/i })).not.toBeInTheDocument();
      expect(screen.getByRole("tab", { name: /favorites/i })).toHaveTextContent(/2$/);
    });

    it("disables Load more while the page is in flight — the sole double-submit guard", async () => {
      let settle: (r: Response) => void = () => {};
      const pending = new Promise<Response>((r) => {
        settle = r;
      });
      const { respond, cursorRequests } = twoPageResponder([activeFavorite], CURSOR, () => pending);
      renderPage({ favoritesResponder: respond });
      await goToFavoritesTab();

      const loadMore = await screen.findByRole("button", { name: /load more/i });
      expect(loadMore).not.toBeDisabled();
      await userEvent.click(loadMore);
      expect(loadMore).toBeDisabled();
      expect(cursorRequests()).toBe(1);

      settle(jsonResponse(200, { items: [archivedFavorite], nextCursor: null }));
      await waitFor(() => expect(favoriteRows()).toHaveLength(2));
      expect(cursorRequests()).toBe(1);
    });

    it("keeps the loaded rows when a page fails, shows the problem beside the button, and re-enables it", async () => {
      const { respond } = twoPageResponder([activeFavorite], CURSOR, () =>
        Promise.resolve(jsonResponse(500, { type: "/errors/internal", status: 500 })),
      );
      renderPage({ favoritesResponder: respond });
      await goToFavoritesTab();

      await userEvent.click(await screen.findByRole("button", { name: /load more/i }));

      expect(await screen.findByRole("alert")).toBeInTheDocument();
      expect(favoriteRows()).toHaveLength(1);
      await waitFor(() =>
        expect(screen.getByRole("button", { name: /load more/i })).not.toBeDisabled(),
      );
    });

    it("unfavoriting a loaded row keeps the cursor usable — the next page still appends", async () => {
      // Keyset paging makes the local filter CORRECT, not merely tolerable: the
      // cursor is the last loaded row's position, so removing a row shifts
      // nothing the next page depends on.
      const { respond } = twoPageResponder([activeFavorite, archivedFavorite], CURSOR, () =>
        Promise.resolve(
          jsonResponse(200, {
            items: [{ ...activeFavorite, id: "def-3", title: "Third Net", linkToken: "tok-3" }],
            nextCursor: null,
          }),
        ),
      );
      renderPage({ favoritesResponder: respond });
      await goToFavoritesTab();

      const row = (await screen.findByText("Sunday Traffic Net")).closest("li") as HTMLElement;
      await userEvent.click(within(row).getByRole("button", { name: /favorite/i }));
      await waitFor(() => expect(favoriteRows()).toHaveLength(1));

      await userEvent.click(screen.getByRole("button", { name: /load more/i }));
      await waitFor(() => expect(favoriteRows()).toHaveLength(2));
      expect(screen.getByText("Third Net")).toBeInTheDocument();
    });

    it("keeps Load more, and shows no empty state, after every loaded row is unfavorited while a cursor remains", async () => {
      // Zero rows with a cursor is not "you have no favorites": the next page
      // is still there, and the button is the only way to reach it.
      const { respond } = twoPageResponder([activeFavorite], CURSOR, () =>
        Promise.resolve(jsonResponse(200, { items: [archivedFavorite], nextCursor: null })),
      );
      renderPage({ favoritesResponder: respond });
      await goToFavoritesTab();

      const row = (await screen.findByText("Sunday Traffic Net")).closest("li") as HTMLElement;
      await userEvent.click(within(row).getByRole("button", { name: /favorite/i }));
      await waitFor(() =>
        expect(within(screen.getByRole("tabpanel")).queryAllByRole("listitem")).toHaveLength(0),
      );

      expect(screen.queryByTestId("favorites-empty")).not.toBeInTheDocument();
      expect(screen.getByRole("tab", { name: /favorites/i })).toHaveTextContent(/0\+$/);
      await userEvent.click(screen.getByRole("button", { name: /load more/i }));

      await waitFor(() => expect(favoriteRows()).toHaveLength(1));
      expect(screen.getByText("Retired Emergency Net")).toBeInTheDocument();
      expect(screen.queryByRole("button", { name: /load more/i })).not.toBeInTheDocument();
    });

    it("keeps an unfavorite that is confirmed while the next page is in flight — the landing page does not put the row back", async () => {
      // The append must compose with the removal, not overwrite it from a
      // snapshot of the rows taken before the round trip.
      let settle: (r: Response) => void = () => {};
      const pending = new Promise<Response>((r) => {
        settle = r;
      });
      const { respond } = twoPageResponder([activeFavorite, archivedFavorite], CURSOR, () => pending);
      renderPage({ favoritesResponder: respond });
      await goToFavoritesTab();

      await waitFor(() => expect(favoriteRows()).toHaveLength(2));
      await userEvent.click(screen.getByRole("button", { name: /load more/i }));

      const row = screen.getByText("Sunday Traffic Net").closest("li") as HTMLElement;
      await userEvent.click(within(row).getByRole("button", { name: /favorite/i }));
      await waitFor(() => expect(favoriteRows()).toHaveLength(1));

      settle(
        jsonResponse(200, {
          items: [{ ...activeFavorite, id: "def-3", title: "Third Net", linkToken: "tok-3" }],
          nextCursor: null,
        }),
      );

      await waitFor(() => expect(screen.getByText("Third Net")).toBeInTheDocument());
      expect(favoriteRows()).toHaveLength(2);
      expect(screen.queryByText("Sunday Traffic Net")).not.toBeInTheDocument();
    });
  });
});
