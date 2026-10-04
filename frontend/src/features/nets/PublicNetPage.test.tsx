// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
import { cleanup, render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";
import { RouterProvider, createMemoryRouter } from "react-router";

import { messageForProblemType } from "../../errors/problemMessages";
import { PublicNetPage } from "./PublicNetPage";

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

const publicView = {
  id: "0192-abc",
  title: "Sunday Traffic Net",
  description: null,
  country: null,
  state: null,
  grid: null,
  netCategory: "traffic",
  netType: "open",
  expectedDurationMinutes: null,
  visibility: "unlisted",
  connections: [
    connection({
      kind: "hf",
      plannedFrequencyHz: 14_230_000,
      band: "20m",
      mode: "ssb",
    }),
  ],
  createdAt: "2026-07-16T00:00:00Z",
  updatedAt: "2026-07-16T00:00:00Z",
  archivedAt: null,
};

const MEMBERSHIP_PATH = "/api/favorites/membership";

/** The ids a membership request asked about, parsed off its URL. */
function membershipIds(url: string): string[] {
  const ids = new URL(url, "http://test").searchParams.get("ids");
  return ids === null ? [] : ids.split(",");
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

/** Routes by method+path so the account fetch and the favorite writes are
 * distinguishable from the public token read. `account` null → signed out (the
 * `/api/accounts/me` call answers 401). `favoritedIds` seeds the membership
 * read (`GET /api/favorites/membership?ids=…` answers the asked ids that are in
 * it) so the toggle's initial state can be asserted against the account's
 * actual server-side favorites, not assumed unfavorited. `membershipResponder`
 * replaces that default answer so a test can hold the read in flight. */
function renderAt(
  token: string,
  {
    account = null,
    favoritedIds = [],
    membershipResponder,
    onFavoriteWrite,
    view = publicView,
  }: {
    account?: typeof signedInAccount | null;
    favoritedIds?: string[];
    membershipResponder?: (askedIds: string[]) => Promise<Response>;
    onFavoriteWrite?: (method: string, url: string) => void;
    view?: Omit<typeof publicView, "description"> & {
      description: string | null;
    };
  } = {},
): { router: ReturnType<typeof createMemoryRouter>; calls: string[] } {
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
    return jsonResponse(200, view);
  });
  vi.stubGlobal("fetch", fetchMock);
  const router = createMemoryRouter(
    [
      { path: "/nets/t/:token", element: <PublicNetPage /> },
      { path: "/sign-in", element: <div data-testid="sign-in" /> },
    ],
    { initialEntries: [`/nets/t/${token}`] },
  );
  render(<RouterProvider router={router} />);
  return { router, calls };
}

afterEach(() => {
  vi.unstubAllGlobals();
});

describe("PublicNetPage", () => {
  it("frames the shared net in an elevated panel with a tonal header band", async () => {
    renderAt("the-token");

    const title = await screen.findByRole("heading", {
      name: /sunday traffic net/i,
    });
    const band = title.closest("[data-net-header]") as HTMLElement;
    expect(band).toHaveStyle({ background: "var(--head-grad)" });
    expect(band.closest("section")).toHaveStyle({
      background: "var(--surface)",
      boxShadow: "var(--shadow)",
      overflow: "hidden",
    });
  });

  it("sets the frequency in the mono freq pill rather than as prose", async () => {
    renderAt("the-token");

    await screen.findByRole("heading", { name: "Sunday Traffic Net" });
    const pill = screen.getByTestId("freq-pill");

    // Frequency is radio data: the one heritage nod is mono numerals, and the
    // pill carries its own cyan-ink treatment (DESIGN.md § components).
    expect(pill).toHaveTextContent(/14\.23/);
    expect(pill).toHaveStyle({ background: "var(--freq-fill)" });
    expect(pill.querySelector(".mono")).not.toBeNull();
  });

  it("gives the visitor a way back to discovery from a shared link", async () => {
    renderAt("the-token");

    await screen.findByRole("heading", { name: "Sunday Traffic Net" });
    // A link-token arrival has no history to go back through — wayfinding is
    // load-bearing here (EXPERIENCE.md: breadcrumbs self-explain the app).
    const crumbs = screen.getByRole("navigation", { name: /breadcrumb/i });
    expect(within(crumbs).getByRole("link", { name: /nets/i })).toHaveAttribute(
      "href",
      "/",
    );
  });

  it("lays the net's spec out as labelled rows, not a stack of paragraphs", async () => {
    renderAt("the-token");

    await screen.findByRole("heading", { name: "Sunday Traffic Net" });
    const spec = screen.getByTestId("net-spec");

    // Every fact is a label/value row on one grid, so the values line up
    // instead of trailing their labels mid-sentence.
    const rows = spec.querySelectorAll("[data-spec-row]");
    expect(rows.length).toBeGreaterThan(0);
    for (const row of rows) {
      expect(row).toHaveStyle({ display: "grid" });
    }
  });

  it("publishes the access details a visitor needs to actually tune in", async () => {
    const repeaterNet = {
      ...publicView,
      grid: "FN31",
      expectedDurationMinutes: 45,
      connections: [
        connection({
          id: "conn-rpt",
          kind: "repeater",
          plannedFrequencyHz: 146_940_000,
          band: "2m",
          mode: "fm",
          repeaterOffsetHz: -600_000,
          toneMode: "ctcss",
          toneValue: "146.2",
        }),
        connection({ id: "conn-el", position: 1, kind: "echolink", node: "12345" }),
      ],
    };
    const fetchMock = vi.fn(async (url: string) =>
      url === "/api/accounts/me"
        ? jsonResponse(401, { type: "/errors/unauthenticated", status: 401 })
        : jsonResponse(200, repeaterNet),
    );
    vi.stubGlobal("fetch", fetchMock);
    render(
      <RouterProvider
        router={createMemoryRouter(
          [{ path: "/nets/t/:token", element: <PublicNetPage /> }],
          { initialEntries: ["/nets/t/repeater"] },
        )}
      />,
    );

    await screen.findByRole("heading", { name: "Sunday Traffic Net" });
    const spec = screen.getByTestId("net-spec");

    // These came back from the API and were previously dropped on the floor —
    // a repeater net's offset/tone is the whole point of the shared link.
    expect(spec).toHaveTextContent(/offset/i);
    expect(spec).toHaveTextContent(/-0\.6|600/);
    expect(spec).toHaveTextContent(/146\.2/);
    expect(spec).toHaveTextContent(/12345/);
    expect(spec).toHaveTextContent(/FN31/);
    expect(spec).toHaveTextContent(/45/);
  });

  it("omits an access row the net does not define, rather than showing it empty", async () => {
    renderAt("the-token");

    await screen.findByRole("heading", { name: "Sunday Traffic Net" });
    const spec = screen.getByTestId("net-spec");

    // The default fixture is a simple HF net: no repeater, tone, or node.
    expect(spec).not.toHaveTextContent(/offset/i);
    expect(spec).not.toHaveTextContent(/tone/i);
    expect(spec).not.toHaveTextContent(/echolink/i);
  });

  it("carries the archived state as a full banner, not one muted line", async () => {
    const fetchMock = vi.fn(async (url: string) =>
      url === "/api/accounts/me"
        ? jsonResponse(401, { type: "/errors/unauthenticated", status: 401 })
        : jsonResponse(200, { ...publicView, archivedAt: "2026-07-16T12:00:00Z" }),
    );
    vi.stubGlobal("fetch", fetchMock);
    const router = createMemoryRouter(
      [{ path: "/nets/t/:token", element: <PublicNetPage /> }],
      { initialEntries: ["/nets/t/archived-token"] },
    );
    render(<RouterProvider router={router} />);

    await screen.findByRole("heading", { name: "Sunday Traffic Net" });
    const notice = screen.getByTestId("archived-notice");

    // "No longer active" is the single most decision-relevant fact on the
    // page for someone who just followed a link — it gets the amber banner
    // treatment (fill + border + icon), never color-on-text alone.
    expect(notice).toHaveStyle({
      background: "var(--catch-fill)",
      color: "var(--catch-text)",
    });
    expect(notice.querySelector("svg")).not.toBeNull();
  });

  it("fetches by the :token param and renders the net's title and frequency", async () => {
    const { calls } = renderAt("the-token");

    expect(await screen.findByRole("heading", { name: "Sunday Traffic Net" })).toBeInTheDocument();
    expect(screen.getByText(/14\.23/)).toBeInTheDocument();
    expect(calls).toContain("GET /api/net-definitions/by-token/the-token");
  });

  it("is public: it renders for a signed-out visitor and never redirects to sign-in", async () => {
    const { router } = renderAt("the-token", { account: null });

    await screen.findByRole("heading", { name: "Sunday Traffic Net" });
    expect(router.state.location.pathname).toBe("/nets/t/the-token");
    expect(screen.queryByRole("button", { name: "Favorite this net" })).not.toBeInTheDocument();
  });

  it("shows an archived notice when the net is archived, and the title still renders", async () => {
    const fetchMock = vi.fn(async (url: string) =>
      url === "/api/accounts/me"
        ? jsonResponse(401, { type: "/errors/unauthenticated", status: 401 })
        : jsonResponse(200, { ...publicView, archivedAt: "2026-07-16T12:00:00Z" }),
    );
    vi.stubGlobal("fetch", fetchMock);
    const router = createMemoryRouter(
      [{ path: "/nets/t/:token", element: <PublicNetPage /> }],
      { initialEntries: ["/nets/t/archived-token"] },
    );
    render(<RouterProvider router={router} />);

    expect(await screen.findByRole("heading", { name: "Sunday Traffic Net" })).toBeInTheDocument();
    expect(screen.getByTestId("archived-notice")).toBeInTheDocument();
  });

  it("renders the mapped not-found message on a 404 without gating", async () => {
    const fetchMock = vi.fn(async (url: string) =>
      url === "/api/accounts/me"
        ? jsonResponse(401, { type: "/errors/unauthenticated", status: 401 })
        : jsonResponse(404, { type: "/errors/net-definition-not-found", status: 404 }),
    );
    vi.stubGlobal("fetch", fetchMock);
    const router = createMemoryRouter(
      [{ path: "/nets/t/:token", element: <PublicNetPage /> }],
      { initialEntries: ["/nets/t/wrong-token"] },
    );
    render(<RouterProvider router={router} />);

    expect(
      await screen.findByText(
        "That net no longer exists — it may have been deleted.",
      ),
    ).toBeInTheDocument();
  });

  it("shows the favorite toggle for a signed-in visitor and favorites the net by id", async () => {
    const writes: string[] = [];
    renderAt("the-token", {
      account: signedInAccount,
      onFavoriteWrite: (method, url) => writes.push(`${method} ${url}`),
    });

    await screen.findByRole("heading", { name: "Sunday Traffic Net" });
    const toggle = await screen.findByRole("button", { name: "Favorite this net" });
    expect(toggle).toHaveAttribute("aria-pressed", "false");

    await userEvent.click(toggle);
    // Favoriting PUTs the net's id (from the public view), and the confirmed
    // state is then reflected.
    await waitFor(() => expect(writes).toContain("PUT /api/favorites/0192-abc"));
    await waitFor(() =>
      expect(
        screen.getByRole("button", { name: "Favorite this net" }),
      ).toHaveAttribute("aria-pressed", "true"),
    );
  });

  it("initializes the toggle as already-favorited when the account has already favorited this net", async () => {
    renderAt("the-token", {
      account: signedInAccount,
      favoritedIds: ["0192-abc"],
    });

    await screen.findByRole("heading", { name: "Sunday Traffic Net" });
    // The net's id (0192-abc) is present in the account's server-side
    // favorites — the toggle must reflect that on first render, not lie by
    // defaulting to unfavorited.
    await waitFor(() =>
      expect(
        screen.getByRole("button", { name: "Favorite this net" }),
      ).toHaveAttribute("aria-pressed", "true"),
    );
  });

  it("asks the membership read about exactly this net, and never reads the favorites list", async () => {
    // The favorites list is paged; walking it to answer "is THIS net favorited?"
    // spent the read budget in proportion to the account's favorite count and
    // could never finish for a large one. The star asks about one id instead.
    const { calls } = renderAt("the-token", {
      account: signedInAccount,
      favoritedIds: ["0192-abc"],
    });

    await waitFor(() =>
      expect(
        screen.getByRole("button", { name: "Favorite this net" }),
      ).toHaveAttribute("aria-pressed", "true"),
    );
    const membershipReads = calls.filter((call) => call.startsWith(`GET ${MEMBERSHIP_PATH}?`));
    expect(membershipReads).toHaveLength(1);
    expect(membershipIds(membershipReads[0].slice("GET ".length))).toEqual(["0192-abc"]);
    expect(calls.some((call) => /^GET \/api\/favorites(\?|$)/.test(call))).toBe(false);
  });

  it("keeps a toggle confirmed while the membership read was in flight — the stale answer does not put the star back", async () => {
    // The read describes the server as it stood when the read began. A toggle
    // confirmed after that is the newer fact; an answer landing later must not
    // overwrite it.
    let settle: (r: Response) => void = () => {};
    const pending = new Promise<Response>((r) => {
      settle = r;
    });
    renderAt("the-token", {
      account: signedInAccount,
      membershipResponder: () => pending,
    });

    const toggle = await screen.findByRole("button", { name: "Favorite this net" });
    expect(toggle).toHaveAttribute("aria-pressed", "false");
    await userEvent.click(toggle);
    await waitFor(() => expect(toggle).toHaveAttribute("aria-pressed", "true"));

    settle(jsonResponse(200, { favorited: [] }));
    // Inverted wait: the stale answer lands within a few ticks, so if it were
    // applied the star would read unpressed well inside this window. A
    // synchronous read right after `settle` would sample the pre-answer state
    // and pass for the wrong reason.
    await expect(
      waitFor(() => expect(toggle).toHaveAttribute("aria-pressed", "false"), { timeout: 250 }),
    ).rejects.toThrow();
  });

  it("initializes the toggle as unfavorited when the account's favorites do not include this net", async () => {
    renderAt("the-token", {
      account: signedInAccount,
      favoritedIds: ["some-other-net-id"],
    });

    const toggle = await screen.findByRole("button", {
      name: "Favorite this net",
    });
    expect(toggle).toHaveAttribute("aria-pressed", "false");
  });

  it("keeps exactly one description render on the page, still last in the panel", async () => {
    const descriptionValue = "First line.\n\nSecond line.";
    renderAt("the-token", {
      view: { ...publicView, description: descriptionValue },
    });

    const description = await screen.findByTestId("net-description");
    const panel = description.closest("section") as HTMLElement;

    // Counted BY VALUE, over every element rather than over `<p>`s carrying
    // `pre-line`. A style-filtered or testid-filtered count is blind to the
    // wrong implementation this guards against — the shared listing
    // preview dropped in "for consistency" — because that component carries
    // neither this node's testid nor its `pre-line`. Any element whose own text
    // IS the description is a render of the description, whatever draws it.
    const rendered = Array.from(panel.querySelectorAll("*")).filter(
      (el) => el.textContent === descriptionValue,
    );
    expect(rendered).toHaveLength(1);
    expect(rendered[0]).toBe(description);

    // Named directly as well, so the specific scenario reds with a diagnosis
    // rather than only as an off-by-one in the count above.
    expect(
      panel.querySelectorAll('[data-testid="net-description-preview"]'),
    ).toHaveLength(0);

    // "Not relocated": still the panel's last child, still right after the
    // spec rows.
    expect(description.previousElementSibling).toBe(
      screen.getByTestId("net-spec"),
    );
    expect(description.parentElement?.lastElementChild).toBe(description);
  });

  it("keeps the author's paragraph breaks on the detail page", async () => {
    renderAt("the-token", {
      view: { ...publicView, description: "First line.\n\nSecond line." },
    });

    const description = await screen.findByTestId("net-description");

    // `pre-line`, NOT `pre-wrap`, and asserted as the exact value so the
    // distinction is pinned: the field is bounded at 2000 characters AFTER
    // newline normalisation, so 2000 newlines is a legal input and `pre-wrap`
    // would render it as 2000 blank lines.
    expect(description.style.whiteSpace).toBe("pre-line");
  });

  it("wraps an unbroken run rather than pushing the panel past the viewport", async () => {
    renderAt("the-token", {
      view: { ...publicView, description: "x".repeat(2000) },
    });

    const description = await screen.findByTestId("net-description");
    expect(description).toHaveStyle({ overflowWrap: "anywhere" });
  });

  it("never clamps the detail page's description, unlike the listing preview", async () => {
    renderAt("the-token", {
      view: { ...publicView, description: "one\ntwo\nthree\nfour\nfive" },
    });

    const description = await screen.findByTestId("net-description");

    // Asserted on the description node itself: the card surface around it
    // legitimately carries `overflow: hidden` for its rounded corners, so a
    // container-scoped query would contradict a passing test.
    expect(description.style.maxHeight).toBe("");
    expect(description.style.overflow).toBe("");
  });

  it("prefers the server's problem detail over the slug-map fallback", async () => {
    // One of forty-two surfaces. The assertion is the preference
    // ORDER, not the copy.
    const detail = "the field-naming answer the server sent";
    const fetchMock = vi.fn(async (url: string) =>
      url === "/api/accounts/me"
        ? jsonResponse(401, { type: "/errors/unauthenticated", status: 401 })
        : jsonResponse(404, {
            type: "/errors/net-definition-not-found",
            status: 404,
            detail,
          }),
    );
    vi.stubGlobal("fetch", fetchMock);
    const router = createMemoryRouter(
      [{ path: "/nets/t/:token", element: <PublicNetPage /> }],
      { initialEntries: ["/nets/t/wrong-token"] },
    );
    render(<RouterProvider router={router} />);

    const alert = await screen.findByRole("alert");
    expect(alert).toHaveTextContent(detail);
    expect(alert).not.toHaveTextContent(
      messageForProblemType("/errors/net-definition-not-found"),
    );
  });

  it("keeps the permalink out of search indexes while it is open, and takes the directive with it on unmount", async () => {
    // Every Listed net's discovery title links here, so a JS-executing
    // crawler can walk the
    // public landing page into every permalink. `find_by_link_token` applies
    // no visibility filter and there is no token rotation, so an indexed URL
    // keeps resolving after the owner flips the net to Unlisted. The removal
    // half matters as much as the addition: this directive is scoped to this
    // route, and a leaked one would quietly de-index whatever page follows.
    renderAt("the-token");
    await screen.findByRole("heading", { name: /sunday traffic net/i });

    // COUNTED, not just found: every test in this file mounts this page, and `querySelector` returns the FIRST match —
    // so a leftover from an earlier test, or a second mounted instance, would
    // satisfy a not-null assertion while the hook was broken. The count also
    // catches the case the served `index.html` grows its own robots tag, which
    // would make the removal assertion below pass for the wrong reason.
    const robots = document.head.querySelectorAll('meta[name="robots"]');
    expect(robots).toHaveLength(1);
    expect(robots[0]).toHaveAttribute("content", "noindex, nofollow");

    cleanup();
    expect(document.head.querySelectorAll('meta[name="robots"]')).toHaveLength(0);
  });
});

// --- The access facts come from the connection set ---------

describe("PublicNetPage connection set", () => {
  function renderNet(net: unknown) {
    const fetchMock = vi.fn(async (url: string) =>
      url === "/api/accounts/me"
        ? jsonResponse(401, { type: "/errors/unauthenticated", status: 401 })
        : jsonResponse(200, net),
    );
    vi.stubGlobal("fetch", fetchMock);
    render(
      <RouterProvider
        router={createMemoryRouter(
          [{ path: "/nets/t/:token", element: <PublicNetPage /> }],
          { initialEntries: ["/nets/t/any"] },
        )}
      />,
    );
  }

  it("publishes an internet-only net's node and never its stale flat mirror", async () => {
    // The flat columns still hold what this net last had on HF; they were
    // knowingly left in place, and this page is where they would otherwise be
    // published as fact.
    renderNet({
      ...publicView,
      plannedFrequencyHz: 14_230_000,
      band: "20m",
      mode: "ssb",
      connections: [connection({ id: "conn-el", kind: "echolink", node: "12345" })],
    });

    await screen.findByRole("heading", { name: "Sunday Traffic Net" });
    const pill = screen.getByTestId("freq-pill");
    expect(pill).not.toHaveTextContent(/14\.23/);
    expect(pill).not.toHaveTextContent(/20m/);
    expect(screen.getByTestId("net-spec")).toHaveTextContent(/12345/);
  });

  it("renders every connection in the owner's order, not one connection's properties", async () => {
    renderNet({
      ...publicView,
      connections: [
        connection({
          id: "conn-rpt",
          kind: "repeater",
          plannedFrequencyHz: 146_940_000,
          band: "2m",
          mode: "fm",
          repeaterOffsetHz: -600_000,
        }),
        connection({ id: "conn-el", position: 1, kind: "echolink", node: "12345" }),
        connection({ id: "conn-dmr", position: 2, kind: "dmr", talkgroup: "3100" }),
      ],
    });

    await screen.findByRole("heading", { name: "Sunday Traffic Net" });
    const spec = screen.getByTestId("net-spec");
    expect(spec).toHaveTextContent(/12345/);
    expect(spec).toHaveTextContent(/3100/);
    expect(screen.getByTestId("freq-pill")).toHaveTextContent(/146\.940/);
  });
});
