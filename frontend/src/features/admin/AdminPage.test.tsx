// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
import { fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { createMemoryRouter, RouterProvider } from "react-router";
import { afterEach, describe, expect, it, vi } from "vitest";

import { AdminPage } from "./AdminPage";
import { expectNoAxeViolations } from "../../test/axe";
import {
  GENERIC_PROBLEM_MESSAGE,
  messageForProblemType,
} from "../../errors/problemMessages";

const ADMIN = {
  id: "acct-admin",
  email: "admin@example.com",
  emailVerifiedAt: "2026-07-15T00:00:00Z",
  consentRequired: false,
  requiredTermsVersion: "1",
  callsign: "W1ADM",
  displayName: null,
  location: null,
  grid: null,
  avatarUrl: null,
  gravatarUrl: "https://example.test/a",
  qrzCredentialsSet: false,
  isAdmin: true,
};

function jsonResponse(status: number, body: unknown): Response {
  return new Response(JSON.stringify(body), {
    status,
    headers: {
      "content-type":
        status >= 400 ? "application/problem+json" : "application/json",
    },
  });
}

interface RouteStubs {
  /** Responses for `/api/admin/abuse-reports` GETs, in call order. */
  reports?: Response[];
  auditLog?: Response[];
  accounts?: Response[];
  /** Response for any POST (resolve / disable / reenable). */
  action?: Response;
  me?: Response;
}

/**
 * Routes stubbed fetches by URL so a test only declares the endpoints it
 * exercises, and asserts on the recorded calls.
 */
function stubRoutes(stubs: RouteStubs): ReturnType<typeof vi.fn> {
  const queues = {
    reports: [...(stubs.reports ?? [])],
    auditLog: [...(stubs.auditLog ?? [])],
    accounts: [...(stubs.accounts ?? [])],
  };
  const fetchMock = vi.fn((url: unknown, init?: RequestInit) => {
    const href = String(url);
    if (init?.method === "POST") {
      return Promise.resolve(
        stubs.action?.clone() ?? new Response(null, { status: 204 }),
      );
    }
    if (href.startsWith("/api/accounts/me")) {
      return Promise.resolve(stubs.me?.clone() ?? jsonResponse(200, ADMIN));
    }
    if (href.startsWith("/api/admin/abuse-reports")) {
      return Promise.resolve(
        queues.reports.shift() ??
          jsonResponse(200, { items: [], nextCursor: null }),
      );
    }
    if (href.startsWith("/api/admin/audit-log")) {
      return Promise.resolve(
        queues.auditLog.shift() ??
          jsonResponse(200, { items: [], nextCursor: null }),
      );
    }
    if (href.startsWith("/api/admin/search")) {
      return Promise.resolve(
        queues.accounts.shift() ?? jsonResponse(200, { items: [], truncatedTypes: [] }),
      );
    }
    return Promise.resolve(new Response(null, { status: 404 }));
  });
  vi.stubGlobal("fetch", fetchMock);
  return fetchMock;
}

/** Mounts the page on a memory router and reports where it navigated. */
function renderPage(): { router: ReturnType<typeof createMemoryRouter> } {
  const router = createMemoryRouter(
    [
      { path: "/admin", element: <AdminPage /> },
      { path: "/", element: <p>discovery</p> },
      { path: "/sign-in", element: <p>sign in</p> },
    ],
    { initialEntries: ["/admin"] },
  );
  render(<RouterProvider router={router} />);
  return { router };
}

/** URLs of every GET recorded on the mock. */
function getUrls(fetchMock: ReturnType<typeof vi.fn>): string[] {
  return fetchMock.mock.calls
    .filter((call) => (call[1] as RequestInit | undefined)?.method !== "POST")
    .map((call) => String(call[0]));
}

const REPORT = {
  id: "r1",
  createdAt: "2026-08-01T10:00:00Z",
  reporterContact: "W1RPT",
  body: "spam net titles",
  contextUrl: "https://example.test/nets",
};

const MATCH = {
  objectType: "account",
  id: "acct-target",
  label: "W1ABC",
  sublabel: "target@example.com",
  disabledAt: null,
  inactiveAt: null,
};

const NET_MATCH = {
  objectType: "net-definition",
  id: "net-9",
  label: "Sunday Traffic Net",
  sublabel: null,
  disabledAt: null,
  inactiveAt: null,
};

afterEach(() => {
  vi.unstubAllGlobals();
});

describe("AdminPage gating", () => {
  it("redirects a signed-in non-admin away from the dashboard", async () => {
    stubRoutes({ me: jsonResponse(200, { ...ADMIN, isAdmin: false }) });
    const { router } = renderPage();

    await waitFor(() => expect(router.state.location.pathname).toBe("/"));
  });

  it("redirects a signed-out visitor to sign in", async () => {
    stubRoutes({
      me: jsonResponse(401, { type: "/errors/unauthenticated", status: 401 }),
    });
    const { router } = renderPage();

    await waitFor(() => expect(router.state.location.pathname).toBe("/sign-in"));
  });

  it("keeps an admin on the dashboard", async () => {
    stubRoutes({});
    const { router } = renderPage();

    await screen.findByRole("tab", { name: /reports/i });
    expect(router.state.location.pathname).toBe("/admin");
  });

  it("issues no admin request before the session check answers", () => {
    // Firing admin reads while /me is still in flight would 403-spam the server
    // for every non-admin who lands on the URL.
    const fetchMock = stubRoutes({});
    renderPage();

    expect(getUrls(fetchMock).some((u) => u.startsWith("/api/admin"))).toBe(
      false,
    );
  });
});

describe("AdminPage reports tab", () => {
  it("moves from loading to the loaded queue", async () => {
    // The loading state is transient, so the response is held open explicitly
    // rather than raced against — otherwise this asserts on scheduler luck.
    let release: (() => void) | undefined;
    const held = new Promise<void>((resolve) => {
      release = resolve;
    });
    const fetchMock = vi.fn((url: unknown) => {
      const href = String(url);
      if (href.startsWith("/api/accounts/me")) {
        return Promise.resolve(jsonResponse(200, ADMIN));
      }
      return held.then(() =>
        jsonResponse(200, { items: [REPORT], nextCursor: null }),
      );
    });
    vi.stubGlobal("fetch", fetchMock);
    renderPage();

    expect(await screen.findByRole("status")).toBeInTheDocument();

    release?.();

    expect(await screen.findByText(REPORT.body)).toBeInTheDocument();
    expect(screen.queryByRole("status")).not.toBeInTheDocument();
  });

  it("surfaces a failed load with a retry that re-requests", async () => {
    const fetchMock = stubRoutes({
      reports: [
        jsonResponse(500, { type: "/errors/internal", status: 500 }),
        jsonResponse(200, { items: [REPORT], nextCursor: null }),
      ],
    });
    renderPage();

    const alert = await screen.findByRole("alert");
    expect(alert).toBeInTheDocument();
    await userEvent.click(screen.getByRole("button", { name: /try again/i }));

    expect(await screen.findByText(REPORT.body)).toBeInTheDocument();
    expect(
      getUrls(fetchMock).filter((u) => u.startsWith("/api/admin/abuse-reports"))
        .length,
    ).toBe(2);
  });

  it("removes a resolved report only after the server confirms", async () => {
    stubRoutes({
      reports: [jsonResponse(200, { items: [REPORT], nextCursor: null })],
      action: new Response(null, { status: 204 }),
    });
    renderPage();

    await screen.findByText(REPORT.body);
    await userEvent.click(screen.getByRole("button", { name: /resolve/i }));

    await waitFor(() =>
      expect(screen.queryByText(REPORT.body)).not.toBeInTheDocument(),
    );
  });

  it("keeps a report listed when resolving it fails", async () => {
    // Optimistically dropping the row would hide a report that is still open.
    stubRoutes({
      reports: [jsonResponse(200, { items: [REPORT], nextCursor: null })],
      action: jsonResponse(404, {
        type: "/errors/abuse-report-not-found",
        status: 404,
      }),
    });
    renderPage();

    await screen.findByText(REPORT.body);
    await userEvent.click(screen.getByRole("button", { name: /resolve/i }));

    await screen.findByRole("alert");
    expect(screen.getByText(REPORT.body)).toBeInTheDocument();
  });

  it("loads the next page with the server's cursor and appends it", async () => {
    const second = { ...REPORT, id: "r2", body: "second report" };
    const fetchMock = stubRoutes({
      reports: [
        jsonResponse(200, { items: [REPORT], nextCursor: "cursor-1" }),
        jsonResponse(200, { items: [second], nextCursor: null }),
      ],
    });
    renderPage();

    await screen.findByText(REPORT.body);
    await userEvent.click(screen.getByRole("button", { name: /load more/i }));

    expect(await screen.findByText(second.body)).toBeInTheDocument();
    // Both pages are shown, not just the newest.
    expect(screen.getByText(REPORT.body)).toBeInTheDocument();
    const paged = getUrls(fetchMock).find((u) => u.includes("cursor="));
    expect(new URL(paged ?? "", "https://x.test").searchParams.get("cursor")).toBe(
      "cursor-1",
    );
  });

  it("keeps the loaded pages when loading the next one fails", async () => {
    // Replacing the list with an error would throw away reports the admin has
    // already been shown, for a failure that only affects the NEXT page.
    stubRoutes({
      reports: [
        jsonResponse(200, { items: [REPORT], nextCursor: "cursor-1" }),
        jsonResponse(500, { type: "/errors/internal", status: 500 }),
      ],
    });
    renderPage();

    await screen.findByText(REPORT.body);
    await userEvent.click(screen.getByRole("button", { name: /load more/i }));

    await screen.findByRole("alert");
    expect(screen.getByText(REPORT.body)).toBeInTheDocument();
    // Still resumable from the same cursor.
    expect(
      screen.getByRole("button", { name: /load more/i }),
    ).toBeInTheDocument();
  });

  it("offers no load-more once the last page arrives", async () => {
    stubRoutes({
      reports: [jsonResponse(200, { items: [REPORT], nextCursor: null })],
    });
    renderPage();

    await screen.findByText(REPORT.body);
    expect(
      screen.queryByRole("button", { name: /load more/i }),
    ).not.toBeInTheDocument();
  });
});

describe("AdminPage search tab", () => {
  async function openSearchTab(): Promise<void> {
    await userEvent.click(await screen.findByRole("tab", { name: /^search$/i }));
  }

  it("looks up the typed term and shows the match", async () => {
    const fetchMock = stubRoutes({ accounts: [jsonResponse(200, { items: [MATCH], truncatedTypes: [] })] });
    renderPage();
    await openSearchTab();

    await userEvent.type(screen.getByLabelText(/callsign, name, net title/i), "W1ABC");
    await userEvent.click(screen.getByRole("button", { name: /^search$/i }));

    expect(await screen.findByText("target@example.com")).toBeInTheDocument();
    const looked = getUrls(fetchMock).find((u) =>
      u.startsWith("/api/admin/search?"),
    );
    expect(new URL(looked ?? "", "https://x.test").searchParams.get("q")).toBe(
      "W1ABC",
    );
  });

  it("says the result is incomplete when the server cut a type", async () => {
    // Twenty rows with no signal read as "that is all of them";
    // the notice is what stops "no such account" and "not in the first page"
    // looking identical. Asserted on the element, not its wording.
    stubRoutes({
      accounts: [
        jsonResponse(200, { items: [MATCH], truncatedTypes: ["account"] }),
      ],
    });
    renderPage();
    await openSearchTab();

    await userEvent.type(screen.getByLabelText(/callsign, name, net title/i), "W1");
    await userEvent.click(screen.getByRole("button", { name: /^search$/i }));
    await screen.findByText("target@example.com");

    // With hits rendered, neither "Searching…" nor "Nothing matches that." is
    // present, so the only status line on the tab is the truncation notice.
    expect(screen.getByRole("status")).toHaveTextContent(/more .*matched/i);
  });

  it("shows no incompleteness notice when every type is complete", async () => {
    // The other half of the pair: an empty signal must render nothing, or the
    // notice becomes noise an admin learns to ignore.
    stubRoutes({
      accounts: [jsonResponse(200, { items: [MATCH], truncatedTypes: [] })],
    });
    renderPage();
    await openSearchTab();

    await userEvent.type(screen.getByLabelText(/callsign, name, net title/i), "W1ABC");
    await userEvent.click(screen.getByRole("button", { name: /^search$/i }));
    await screen.findByText("target@example.com");

    expect(screen.queryByRole("status")).not.toBeInTheDocument();
  });

  it("reports a no-match distinctly from an error", async () => {
    stubRoutes({ accounts: [jsonResponse(200, { items: [], truncatedTypes: [] })] });
    renderPage();
    await openSearchTab();

    await userEvent.type(screen.getByLabelText(/callsign, name, net title/i), "W9ZZZ");
    await userEvent.click(screen.getByRole("button", { name: /^search$/i }));

    expect(await screen.findByRole("status")).toHaveTextContent(/nothing matches/i);
    expect(screen.queryByRole("alert")).not.toBeInTheDocument();
  });

  it("requires a confirm step before disabling", async () => {
    // Disabling another operator is destructive and cross-account; a stray
    // click must not carry it out.
    const fetchMock = stubRoutes({
      accounts: [jsonResponse(200, { items: [MATCH], truncatedTypes: [] })],
    });
    renderPage();
    await openSearchTab();
    await userEvent.type(screen.getByLabelText(/callsign, name, net title/i), "W1ABC");
    await userEvent.click(screen.getByRole("button", { name: /^search$/i }));
    await screen.findByText("target@example.com");

    await userEvent.click(screen.getByRole("button", { name: /^disable$/i }));

    const posts = fetchMock.mock.calls.filter(
      (call) => (call[1] as RequestInit | undefined)?.method === "POST",
    );
    expect(posts).toHaveLength(0);
    expect(
      screen.getByRole("button", { name: /confirm disable/i }),
    ).toBeInTheDocument();
  });

  it("disables on confirm and re-reads the account's state", async () => {
    const fetchMock = stubRoutes({
      accounts: [
        jsonResponse(200, { items: [MATCH], truncatedTypes: [] }),
        jsonResponse(200, {
          items: [{ ...MATCH, disabledAt: "2026-08-01T12:00:00Z" }],
          truncatedTypes: [],
        }),
      ],
    });
    renderPage();
    await openSearchTab();
    await userEvent.type(screen.getByLabelText(/callsign, name, net title/i), "W1ABC");
    await userEvent.click(screen.getByRole("button", { name: /^search$/i }));
    await screen.findByText("target@example.com");

    await userEvent.click(screen.getByRole("button", { name: /^disable$/i }));
    await userEvent.click(
      screen.getByRole("button", { name: /confirm disable/i }),
    );

    // The re-read swaps the offered action, proving state came from the server
    // rather than a local guess.
    expect(
      await screen.findByRole("button", { name: /re-enable/i }),
    ).toBeInTheDocument();
    const disablePost = fetchMock.mock.calls.find(
      (call) =>
        (call[1] as RequestInit | undefined)?.method === "POST" &&
        String(call[0]).endsWith("/disable"),
    );
    expect(disablePost).toBeDefined();
  });

  it("maps a refused self-disable to its own message", async () => {
    stubRoutes({
      accounts: [
        jsonResponse(200, {
          items: [{ ...MATCH, id: ADMIN.id, sublabel: ADMIN.email }],
          truncatedTypes: [],
        }),
      ],
      action: jsonResponse(409, {
        type: "/errors/cannot-disable-self",
        status: 409,
      }),
    });
    renderPage();
    await openSearchTab();
    await userEvent.type(screen.getByLabelText(/callsign, name, net title/i), "W1ADM");
    await userEvent.click(screen.getByRole("button", { name: /^search$/i }));
    await screen.findByText(ADMIN.email);

    await userEvent.click(screen.getByRole("button", { name: /^disable$/i }));
    await userEvent.click(
      screen.getByRole("button", { name: /confirm disable/i }),
    );

    expect(await screen.findByRole("alert")).toHaveTextContent(/own account/i);
  });
});

describe("AdminPage audit tab", () => {
  const ENTRY = {
    id: "a1",
    occurredAt: "2026-08-01T11:00:00Z",
    actorAccountId: "acct-admin",
    action: "disable-account",
    targetType: "account",
    targetId: "acct-target",
    metadata: { newlyDisabled: true },
  };

  it("reads the log only once the tab is opened", async () => {
    // Reading the audit log writes its own audit row, so an unopened tab must
    // not silently generate one on every dashboard visit.
    const fetchMock = stubRoutes({
      auditLog: [jsonResponse(200, { items: [ENTRY], nextCursor: null })],
    });
    renderPage();
    await screen.findByRole("tab", { name: /reports/i });

    expect(
      getUrls(fetchMock).some((u) => u.startsWith("/api/admin/audit-log")),
    ).toBe(false);

    await userEvent.click(screen.getByRole("tab", { name: /audit/i }));

    await waitFor(() =>
      expect(
        getUrls(fetchMock).filter((u) => u.startsWith("/api/admin/audit-log")),
      ).toHaveLength(1),
    );
  });

  it("shows each record's actor, action and target", async () => {
    stubRoutes({
      auditLog: [jsonResponse(200, { items: [ENTRY], nextCursor: null })],
    });
    renderPage();
    await userEvent.click(await screen.findByRole("tab", { name: /audit/i }));

    const row = await screen.findByRole("row", { name: /disable-account/i });
    expect(within(row).getByText(ENTRY.actorAccountId)).toBeInTheDocument();
    expect(within(row).getByText(ENTRY.targetId)).toBeInTheDocument();
  });
});

describe("AdminPage paged Load-more", () => {
  // "Load more" is not idempotent
  // — a second press with the same cursor re-fetches and re-appends the SAME
  // page — so the affordance has to be withdrawn while a page request is
  // outstanding. The two tabs are separate components with separate state, so
  // each is covered on its own.
  //
  // The withdrawal is the `disabled={pageLoading}` binding on the button, and
  // it is the ONLY thing that blocks the second press: the loaders'
  // `|| xPageLoading` guard clause was proven inert (each loader is redefined
  // per render and closed over that render's flag, so two invocations from one
  // render both read `false`) and removed. These tests therefore pin the
  // binding directly — the flag down before the press, up during, and the
  // request count either side — rather than pinning a clause that never fired.
  const SECOND_REPORT = {
    id: "r2",
    createdAt: "2026-08-02T10:00:00Z",
    reporterContact: null,
    body: "second page report",
    contextUrl: null,
  };

  const AUDIT_ENTRY = {
    id: "a1",
    occurredAt: "2026-08-01T11:00:00Z",
    actorAccountId: "acct-admin",
    action: "disable-account",
    targetType: "account",
    targetId: "acct-target",
    metadata: null,
  };

  const SECOND_AUDIT_ENTRY = {
    ...AUDIT_ENTRY,
    id: "a2",
    action: "reenable-account",
  };

  /**
   * Stubs fetch so the FIRST page of `prefix` resolves immediately and the
   * SECOND is held open, and records every call to `prefix`.
   */
  function stubHeldSecondPage(
    prefix: string,
    firstPage: unknown,
  ): {
    calls: string[];
    release: (body: unknown) => void;
  } {
    const calls: string[] = [];
    let resolveSecond: ((response: Response) => void) | undefined;
    const fetchMock = vi.fn((url: unknown) => {
      const href = String(url);
      if (href.startsWith("/api/accounts/me")) {
        return Promise.resolve(jsonResponse(200, ADMIN));
      }
      if (href.startsWith(prefix)) {
        calls.push(href);
        if (calls.length === 1) {
          return Promise.resolve(jsonResponse(200, firstPage));
        }
        return new Promise<Response>((resolve) => {
          resolveSecond = resolve;
        });
      }
      return Promise.resolve(jsonResponse(200, { items: [], nextCursor: null }));
    });
    vi.stubGlobal("fetch", fetchMock);
    return {
      calls,
      release: (body: unknown) => resolveSecond?.(jsonResponse(200, body)),
    };
  }

  it("issues one reports request and appends one page when load more is pressed twice in flight", async () => {
    const { calls, release } = stubHeldSecondPage("/api/admin/abuse-reports", {
      items: [REPORT],
      nextCursor: "cursor-1",
    });
    renderPage();

    const loadMore = await screen.findByRole("button", { name: /load more/i });
    // The affordance is OFFERED before the press. This adds no fault detection
    // over the request count below — a button hard-wired `disabled` swallows the
    // FIRST press too, so `calls` is 1 and `toHaveLength(2)` already reds
    // (measured against the unfixed file: "expected [ '/api/admin/abuse-reports'
    // ] to have a length of 2 but got 1"). What it adds is LOCALISATION: against
    // the same mutant this line reds first and names `disabled` as the cause,
    // instead of a call-count mismatch the reader has to diagnose. It also states
    // the start of the transition that `toBeDisabled()` below measures, which was
    // previously left implicit.
    expect(loadMore).not.toBeDisabled();
    // Two SYNCHRONOUS presses. `userEvent.click` awaits between actions, so the
    // first request would already have settled and the race this guards could
    // never be reproduced. Each `fireEvent` wraps its own `act()`, so the first
    // press's `disabled=true` is committed before the second is dispatched —
    // which is exactly the path the `disabled` binding covers.
    fireEvent.click(loadMore);
    fireEvent.click(loadMore);

    // One initial page plus exactly one load-more. A second load-more would
    // re-send the SAME cursor and append the same rows twice.
    expect(calls).toHaveLength(2);
    // …because the second press landed on a withdrawn affordance.
    expect(loadMore).toBeDisabled();

    release({ items: [SECOND_REPORT], nextCursor: null });

    expect(await screen.findByText(SECOND_REPORT.body)).toBeInTheDocument();
    expect(screen.getAllByText(REPORT.body)).toHaveLength(1);
    expect(calls).toHaveLength(2);
  });

  it("issues one audit request and appends one page when load more is pressed twice in flight", async () => {
    const { calls, release } = stubHeldSecondPage("/api/admin/audit-log", {
      items: [AUDIT_ENTRY],
      nextCursor: "cursor-1",
    });
    renderPage();
    await userEvent.click(await screen.findByRole("tab", { name: /audit/i }));

    const loadMore = await screen.findByRole("button", { name: /load more/i });
    expect(loadMore).not.toBeDisabled();
    fireEvent.click(loadMore);
    fireEvent.click(loadMore);

    expect(calls).toHaveLength(2);
    expect(loadMore).toBeDisabled();

    release({ items: [SECOND_AUDIT_ENTRY], nextCursor: null });

    expect(
      await screen.findByRole("row", { name: /reenable-account/i }),
    ).toBeInTheDocument();
    expect(
      screen.getAllByRole("row", { name: /disable-account/i }),
    ).toHaveLength(1);
    expect(calls).toHaveLength(2);
  });

  it("pages the reports tab while an audit page is still in flight", async () => {
    // Each tab's `pageLoading` PROP must be fed its OWN in-flight flag. A full
    // cross-wire — the obvious copy-paste error — reds BOTH this case and the
    // reports double-press case above: `pageLoading={auditPageLoading}` on the
    // reports tab gives 2 failed / 32 passed of this file's 34 —
    // because a swapped flag both fails to withdraw this tab's affordance when
    // this tab is busy and withdraws it when the sibling is. What THIS case pins
    // that the double-press case cannot is that second half alone: an over-broad
    // prop (`pageLoading={reportsPageLoading || auditPageLoading}`) leaves every
    // double-press test green, because this tab's own flag still rises on its own
    // press, and reds only here — 1 failed / 33 passed of 34. That is why the
    // audit tab's page is deliberately held open while the reports tab's is not:
    // a reports affordance withdrawn by the SIBLING's request is visible in no
    // other test.
    //
    // This case was written against the loader's `|| xPageLoading` guard
    // clause. That clause was inert and is gone; the assertions below did not
    // change, because what they actually reached was always the prop wiring.
    const reportsCalls: string[] = [];
    const auditCalls: string[] = [];
    let releaseAudit: ((response: Response) => void) | undefined;
    const fetchMock = vi.fn((url: unknown) => {
      const href = String(url);
      if (href.startsWith("/api/accounts/me")) {
        return Promise.resolve(jsonResponse(200, ADMIN));
      }
      if (href.startsWith("/api/admin/audit-log")) {
        auditCalls.push(href);
        if (auditCalls.length === 1) {
          return Promise.resolve(
            jsonResponse(200, { items: [AUDIT_ENTRY], nextCursor: "audit-1" }),
          );
        }
        // The audit tab's next page never settles, so `auditPageLoading`
        // stays true for the rest of the test.
        return new Promise<Response>((resolve) => {
          releaseAudit = resolve;
        });
      }
      if (href.startsWith("/api/admin/abuse-reports")) {
        reportsCalls.push(href);
        return Promise.resolve(
          jsonResponse(200, {
            items: [reportsCalls.length === 1 ? REPORT : SECOND_REPORT],
            nextCursor: reportsCalls.length === 1 ? "cursor-1" : null,
          }),
        );
      }
      return Promise.resolve(jsonResponse(200, { items: [], nextCursor: null }));
    });
    vi.stubGlobal("fetch", fetchMock);
    renderPage();

    await screen.findByText(REPORT.body);
    await userEvent.click(screen.getByRole("tab", { name: /audit/i }));
    const auditLoadMore = await screen.findByRole("button", {
      name: /load more/i,
    });
    fireEvent.click(auditLoadMore);
    await waitFor(() => expect(auditCalls).toHaveLength(2));

    await userEvent.click(screen.getByRole("tab", { name: /reports/i }));
    const reportsLoadMore = await screen.findByRole("button", {
      name: /load more/i,
    });
    // The reports tab owns its own affordance: the audit tab's outstanding
    // page must not withdraw it.
    expect(reportsLoadMore).not.toBeDisabled();
    fireEvent.click(reportsLoadMore);

    await waitFor(() => expect(reportsCalls).toHaveLength(2));
    expect(await screen.findByText(SECOND_REPORT.body)).toBeInTheDocument();
    releaseAudit?.(jsonResponse(200, { items: [], nextCursor: null }));
  });

  it("pages the audit tab while a reports page is still in flight", async () => {
    // The mirror of the case above, so BOTH `pageLoading` props are pinned to
    // their own flag rather than one direction being covered by accident
    // (each surface is required independently). `ProfilePage`'s check-in
    // history has a single paging surface and no sibling to cross-wire, so it
    // has no counterpart to these two — the asymmetry is deliberate.
    const reportsCalls: string[] = [];
    const auditCalls: string[] = [];
    let releaseReports: ((response: Response) => void) | undefined;
    const fetchMock = vi.fn((url: unknown) => {
      const href = String(url);
      if (href.startsWith("/api/accounts/me")) {
        return Promise.resolve(jsonResponse(200, ADMIN));
      }
      if (href.startsWith("/api/admin/abuse-reports")) {
        reportsCalls.push(href);
        if (reportsCalls.length === 1) {
          return Promise.resolve(
            jsonResponse(200, { items: [REPORT], nextCursor: "cursor-1" }),
          );
        }
        return new Promise<Response>((resolve) => {
          releaseReports = resolve;
        });
      }
      if (href.startsWith("/api/admin/audit-log")) {
        auditCalls.push(href);
        return Promise.resolve(
          jsonResponse(200, {
            items: [
              auditCalls.length === 1 ? AUDIT_ENTRY : SECOND_AUDIT_ENTRY,
            ],
            nextCursor: auditCalls.length === 1 ? "audit-1" : null,
          }),
        );
      }
      return Promise.resolve(jsonResponse(200, { items: [], nextCursor: null }));
    });
    vi.stubGlobal("fetch", fetchMock);
    renderPage();

    const reportsLoadMore = await screen.findByRole("button", {
      name: /load more/i,
    });
    fireEvent.click(reportsLoadMore);
    await waitFor(() => expect(reportsCalls).toHaveLength(2));

    await userEvent.click(screen.getByRole("tab", { name: /audit/i }));
    const auditLoadMore = await screen.findByRole("button", {
      name: /load more/i,
    });
    expect(auditLoadMore).not.toBeDisabled();
    fireEvent.click(auditLoadMore);

    await waitFor(() => expect(auditCalls).toHaveLength(2));
    expect(
      await screen.findByRole("row", { name: /reenable-account/i }),
    ).toBeInTheDocument();
    releaseReports?.(jsonResponse(200, { items: [], nextCursor: null }));
  });

  it("keeps the fetched reports on screen and re-arms load more when the next page fails", async () => {
    // The in-flight flag must be cleared in a `finally`: cleared only on
    // success, one failed page would disable Load-more for good — trading a
    // double-append for a dead button. With the inert `|| reportsPageLoading`
    // guard clause gone, the `disabled` binding is the SOLE double-submit
    // protection, so a flag that is
    // never lowered is not a cosmetic wart — it is the protection stuck on.
    const calls: string[] = [];
    const fetchMock = vi.fn((url: unknown) => {
      const href = String(url);
      if (href.startsWith("/api/accounts/me")) {
        return Promise.resolve(jsonResponse(200, ADMIN));
      }
      if (href.startsWith("/api/admin/abuse-reports")) {
        calls.push(href);
        if (calls.length === 1) {
          return Promise.resolve(
            jsonResponse(200, { items: [REPORT], nextCursor: "cursor-1" }),
          );
        }
        return Promise.resolve(
          jsonResponse(500, { type: "/errors/internal", status: 500 }),
        );
      }
      return Promise.resolve(jsonResponse(200, { items: [], nextCursor: null }));
    });
    vi.stubGlobal("fetch", fetchMock);
    renderPage();

    const loadMore = await screen.findByRole("button", { name: /load more/i });
    fireEvent.click(loadMore);

    // `findByRole` rejects when no alert appears, so the await IS the assertion;
    // wrapping it in `toBeInTheDocument()` adds a check that cannot fail.
    await screen.findByRole("alert");
    // The first page plus the one failed next page. Asserted so the recorded
    // call log is load-bearing: without it a harness that never issued the
    // second request would still satisfy every assertion below.
    expect(calls).toHaveLength(2);
    // The rows already fetched are still rendered — the failure appears BESIDE
    // the list, not instead of it — and the cursor is resumable again.
    expect(screen.getByText(REPORT.body)).toBeInTheDocument();
    await waitFor(() => expect(loadMore).not.toBeDisabled());
  });

  it("keeps the fetched audit entries on screen and re-arms load more when the next page fails", async () => {
    // The mirror of the reports case above, which was the only one of the three
    // paging surfaces with a re-arm assertion. The two tabs have separate
    // `finally` clears, so covering one covers neither the other tab's nor, by
    // extension, the whole behaviour.
    const calls: string[] = [];
    const fetchMock = vi.fn((url: unknown) => {
      const href = String(url);
      if (href.startsWith("/api/accounts/me")) {
        return Promise.resolve(jsonResponse(200, ADMIN));
      }
      if (href.startsWith("/api/admin/audit-log")) {
        calls.push(href);
        if (calls.length === 1) {
          return Promise.resolve(
            jsonResponse(200, { items: [AUDIT_ENTRY], nextCursor: "audit-1" }),
          );
        }
        return Promise.resolve(
          jsonResponse(500, { type: "/errors/internal", status: 500 }),
        );
      }
      return Promise.resolve(jsonResponse(200, { items: [], nextCursor: null }));
    });
    vi.stubGlobal("fetch", fetchMock);
    renderPage();
    await userEvent.click(await screen.findByRole("tab", { name: /audit/i }));

    const loadMore = await screen.findByRole("button", { name: /load more/i });
    fireEvent.click(loadMore);

    await screen.findByRole("alert");
    expect(calls).toHaveLength(2);
    // The entry already fetched is still rendered — the failure appears BESIDE
    // the table, not instead of it — and the cursor is resumable again.
    expect(
      screen.getByRole("row", { name: /disable-account/i }),
    ).toBeInTheDocument();
    await waitFor(() => expect(loadMore).not.toBeDisabled());
  });
});

describe("AdminPage validation copy", () => {
  // Joining query rejections to the problem+json
  // contract made `/errors/validation` reachable on this surface for the first
  // time, and the shared slug map's copy for it is written for the one surface
  // where an email address is the only possible cause. On the admin dashboard
  // the causes are a search term, a filter, or a stale page cursor — so the
  // surface has to map the slug itself, the way it already does for
  // `/errors/forbidden`.
  it("does not describe an admin validation refusal as an email problem", async () => {
    stubRoutes({
      reports: [
        jsonResponse(200, { items: [REPORT], nextCursor: "cursor-1" }),
        jsonResponse(400, { type: "/errors/validation", status: 400 }),
      ],
    });
    renderPage();

    await screen.findByText(REPORT.body);
    await userEvent.click(screen.getByRole("button", { name: /load more/i }));

    const alert = await screen.findByRole("alert");
    expect(alert).not.toHaveTextContent(
      messageForProblemType("/errors/validation"),
    );
    expect(alert.textContent ?? "").not.toMatch(/email/i);
    // Still actionable — overriding the slug must not degrade it to the
    // catch-all either.
    expect(alert).not.toHaveTextContent(GENERIC_PROBLEM_MESSAGE);
  });
});

describe("AdminPage accessibility", () => {
  it("has no axe violations with the queue loaded", async () => {
    stubRoutes({
      reports: [jsonResponse(200, { items: [REPORT], nextCursor: null })],
    });
    const { container } = render(
      <RouterProvider
        router={createMemoryRouter([{ path: "/", element: <AdminPage /> }], {
          initialEntries: ["/"],
        })}
      />,
    );

    await screen.findByText(REPORT.body);
    await expectNoAxeViolations(container);
  });
});

describe("AdminPage search-to-audit pivots", () => {
  async function openSearchTab(): Promise<void> {
    await userEvent.click(await screen.findByRole("tab", { name: /^search$/i }));
  }

  async function searchFor(term: string): Promise<void> {
    await openSearchTab();
    await userEvent.type(
      screen.getByLabelText(/callsign, name, net title/i),
      term,
    );
    await userEvent.click(screen.getByRole("button", { name: /^search$/i }));
  }

  it("exposes the object's full id with a copy control", async () => {
    // The whole point of the search: an id you can actually take away.
    stubRoutes({ accounts: [jsonResponse(200, { items: [NET_MATCH], truncatedTypes: [] })] });
    renderPage();
    await searchFor("Sunday");

    expect(await screen.findByText(NET_MATCH.id)).toBeInTheDocument();
    expect(
      screen.getByRole("button", { name: /copy id/i }),
    ).toBeInTheDocument();
  });

  it("pivots an account into the actor filter and switches to the audit tab", async () => {
    const fetchMock = stubRoutes({
      accounts: [jsonResponse(200, { items: [MATCH], truncatedTypes: [] })],
    });
    renderPage();
    await searchFor("W1ABC");
    await screen.findByText("target@example.com");

    await userEvent.click(
      screen.getByRole("button", { name: /actions by this/i }),
    );

    await waitFor(() => {
      const audited = getUrls(fetchMock).find((u) =>
        u.startsWith("/api/admin/audit-log"),
      );
      expect(
        new URL(audited ?? "", "https://x.test").searchParams.get("actor"),
      ).toBe(MATCH.id);
    });
    expect(
      screen.getByRole("tab", { name: /audit/i }),
    ).toHaveAttribute("aria-selected", "true");
  });

  it("pivots a net into the object filter", async () => {
    // "Who acted on this net?" — the question the whole feature exists for.
    const fetchMock = stubRoutes({
      accounts: [jsonResponse(200, { items: [NET_MATCH], truncatedTypes: [] })],
    });
    renderPage();
    await searchFor("Sunday");
    await screen.findByText(NET_MATCH.label);

    await userEvent.click(
      screen.getByRole("button", { name: /actions on this/i }),
    );

    await waitFor(() => {
      const audited = getUrls(fetchMock).find((u) =>
        u.startsWith("/api/admin/audit-log"),
      );
      expect(
        new URL(audited ?? "", "https://x.test").searchParams.get("object"),
      ).toBe(NET_MATCH.id);
    });
  });

  it("offers no actor pivot for a non-account object", async () => {
    // A net cannot be an actor; offering the pivot would produce an empty page.
    stubRoutes({ accounts: [jsonResponse(200, { items: [NET_MATCH], truncatedTypes: [] })] });
    renderPage();
    await searchFor("Sunday");
    await screen.findByText(NET_MATCH.label);

    expect(
      screen.queryByRole("button", { name: /actions by this/i }),
    ).not.toBeInTheDocument();
  });
});

describe("AdminPage audit filters", () => {
  const ENTRY = {
    id: "a1",
    occurredAt: "2026-08-01T11:00:00Z",
    actorAccountId: "acct-admin",
    action: "disable-account",
    targetType: "account",
    targetId: "acct-target",
    metadata: { newlyDisabled: true },
  };

  async function openAuditTab(): Promise<void> {
    await userEvent.click(await screen.findByRole("tab", { name: /audit/i }));
  }

  it("refetches from the first page when a filter changes", async () => {
    // A cursor is bound to the filter set that issued it, so carrying one
    // across a change would be refused by the server.
    const fetchMock = stubRoutes({
      auditLog: [
        jsonResponse(200, { items: [ENTRY], nextCursor: "cursor-1" }),
        jsonResponse(200, { items: [ENTRY], nextCursor: null }),
      ],
    });
    renderPage();
    await openAuditTab();
    await screen.findByRole("row", { name: /disable-account/i });

    await userEvent.selectOptions(
      screen.getByLabelText(/^action$/i),
      "net-created",
    );

    await waitFor(() => {
      const calls = getUrls(fetchMock).filter((u) =>
        u.startsWith("/api/admin/audit-log"),
      );
      const latest = calls[calls.length - 1] ?? "";
      const params = new URL(latest, "https://x.test").searchParams;
      expect(params.get("action")).toBe("net-created");
      expect(params.has("cursor")).toBe(false);
    });
  });

  it("clicking an actor id in the table pivots the filter in place", async () => {
    const fetchMock = stubRoutes({
      auditLog: [jsonResponse(200, { items: [ENTRY], nextCursor: null })],
    });
    renderPage();
    await openAuditTab();
    await screen.findByRole("row", { name: /disable-account/i });

    await userEvent.click(
      screen.getByRole("button", { name: ENTRY.actorAccountId }),
    );

    await waitFor(() => {
      const calls = getUrls(fetchMock).filter((u) =>
        u.startsWith("/api/admin/audit-log"),
      );
      const latest = calls[calls.length - 1] ?? "";
      expect(
        new URL(latest, "https://x.test").searchParams.get("actor"),
      ).toBe(ENTRY.actorAccountId);
    });
  });

  it("removing a chip clears only that filter", async () => {
    const fetchMock = stubRoutes({
      auditLog: [jsonResponse(200, { items: [ENTRY], nextCursor: null })],
    });
    renderPage();
    await openAuditTab();
    await screen.findByRole("row", { name: /disable-account/i });

    // Two filters active: an actor pivot plus an action.
    await userEvent.click(
      screen.getByRole("button", { name: ENTRY.actorAccountId }),
    );
    await userEvent.selectOptions(
      screen.getByLabelText(/^action$/i),
      "disable-account",
    );

    await userEvent.click(
      screen.getByRole("button", { name: /clear actor filter/i }),
    );

    await waitFor(() => {
      const calls = getUrls(fetchMock).filter((u) =>
        u.startsWith("/api/admin/audit-log"),
      );
      const params = new URL(calls[calls.length - 1] ?? "", "https://x.test")
        .searchParams;
      expect(params.has("actor")).toBe(false);
      expect(params.get("action")).toBe("disable-account");
    });
  });

  it("clear all drops every filter at once", async () => {
    const fetchMock = stubRoutes({
      auditLog: [jsonResponse(200, { items: [ENTRY], nextCursor: null })],
    });
    renderPage();
    await openAuditTab();
    await screen.findByRole("row", { name: /disable-account/i });
    await userEvent.click(
      screen.getByRole("button", { name: ENTRY.actorAccountId }),
    );

    await userEvent.click(screen.getByRole("button", { name: /clear all/i }));

    await waitFor(() => {
      const calls = getUrls(fetchMock).filter((u) =>
        u.startsWith("/api/admin/audit-log"),
      );
      const params = new URL(calls[calls.length - 1] ?? "", "https://x.test")
        .searchParams;
      expect(params.has("actor")).toBe(false);
      expect(params.has("action")).toBe(false);
    });
  });
});

describe("AdminPage problem-detail ordering", () => {
  it("prefers the server's problem detail over this surface's own override", async () => {
    // `detail` beats `ADMIN_MESSAGES`. The only
    // overridden slug that ever carries one is `/errors/validation`, whose
    // `detail` names the offending query key — strictly more useful to an
    // admin than "check the search term and filters". The assertion is the
    // preference ORDER, not the copy.
    const detail = "the field-naming answer the server sent";
    stubRoutes({
      reports: [
        jsonResponse(200, { items: [REPORT], nextCursor: "cursor-1" }),
        jsonResponse(400, { type: "/errors/validation", status: 400, detail }),
      ],
    });
    renderPage();

    await screen.findByText(REPORT.body);
    await userEvent.click(screen.getByRole("button", { name: /load more/i }));

    const alert = await screen.findByRole("alert");
    expect(alert).toHaveTextContent(detail);
    expect(alert).not.toHaveTextContent(
      messageForProblemType("/errors/validation"),
    );
  });

  it("keeps this surface's forbidden override when no detail arrives", async () => {
    // `/errors/forbidden` is detail-free at source, which is the ONLY reason
    // putting `detail` first is safe here. If that ever changed, the admin
    // copy ("your account isn't an administrator") would be displaced by the
    // shared net-ownership copy's replacement, so this pins the override.
    stubRoutes({
      reports: [jsonResponse(403, { type: "/errors/forbidden", status: 403 })],
    });
    renderPage();

    const alert = await screen.findByRole("alert");
    expect(alert).not.toHaveTextContent(
      messageForProblemType("/errors/forbidden"),
    );
    expect(alert).not.toHaveTextContent(GENERIC_PROBLEM_MESSAGE);
    expect(alert.textContent ?? "").toBeTruthy();
  });
});
