// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
import {
  act,
  configure,
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
import { expectNoAxeViolations } from "../../test/axe";
import { LiveRegion, announce } from "../../ui/a11y/LiveRegion";
import { NetDefinitionFormPage } from "./NetDefinitionFormPage";

// PER-FILE ceilings, deliberately not a global `testTimeout` bump in
// vite.config.ts. This is the largest form in the app — a long owner panel plus
// the schedule and delivery sections — and its `userEvent` typing walks every
// field one keystroke at a time. Under a contended host that arithmetic crosses
// the 5000 ms default and a DIFFERENT handful of these tests fails on each
// full-suite run, while the file passes in isolation. The slowness is a
// property of the host, not of the product, so raising the ceiling globally
// would only hide genuinely slow tests everywhere else.
//
// TWO ceilings, not one: `vi.setConfig` governs
// the whole-test deadline, but `waitFor`/`findBy*` have their own 1000 ms
// budget that vitest knows nothing about. A `waitFor` that overruns does not
// report a timeout at all — it rethrows the last assertion error, which is why
// one observed failure here read as `toBeEnabled()` failing at ~1210 ms rather
// than as a timeout. Raising only the vitest deadline could never reach that
// class of failure.
//
// The two ceilings are NOT contained the same way. `vi.setConfig` is scoped
// and restored by vitest per file, but RTL's
// `configure` writes to module-global `@testing-library/dom` state — so the 5 000 ms
// below stays local to this file only because vitest gives each test file its own
// module registry. `vite.config.ts` sets neither `isolate` nor `pool`, so the
// default `forks` + `isolate: true` supplies that; set `isolate: false` (or run
// `--no-isolate`) for speed and this raise silently becomes the whole suite's async
// budget. `src/test/testingLibraryConfig.test.ts` asserts the 1 000 ms default is
// intact and reds with `expected 5000 to be 1000` if that ever happens — read that
// file before "fixing" it, and do not add an `afterAll` reset here, which would
// contain the leak and delete the signal along with it.
vi.setConfig({ testTimeout: 20_000 });
configure({ asyncUtilTimeout: 5_000 });

function jsonResponse(status: number, body: unknown): Response {
  return new Response(status === 204 ? null : JSON.stringify(body), {
    status,
    headers: {
      "content-type":
        status >= 400 ? "application/problem+json" : "application/json",
    },
  });
}

const account = {
  id: "me",
  email: "op@example.com",
  emailVerifiedAt: "2026-07-15T00:00:00Z",
  consentRequired: false,
  requiredTermsVersion: "2026-07-15",
  callsign: "W1AW",
  displayName: null,
  location: null,
  grid: null,
  avatarUrl: null,
  gravatarUrl: "https://gravatar.com/avatar/abc?d=mp",
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

const hfConnection = connection({
  id: "conn-hf",
  position: 0,
  kind: "hf",
  plannedFrequencyHz: 14_230_000,
  band: "20m",
  mode: "ssb",
});

const repeaterConnection = connection({
  id: "conn-rpt",
  position: 1,
  kind: "repeater",
  plannedFrequencyHz: 146_940_000,
  band: "2m",
  mode: "fm",
  repeaterOffsetHz: -600_000,
  toneMode: "ctcss",
  toneValue: "100.0",
});

const echolinkConnection = connection({
  id: "conn-el",
  position: 0,
  kind: "echolink",
  node: "12345",
});

const residueConnection = connection({
  id: "conn-res",
  position: 1,
  kind: "other",
  label: "unclassified-reflector",
  detail: "REF030 C",
});

const created = {
  id: "new-id",
  definitionVersion: 1,
  title: "Sunday Traffic Net",
  description: null,
  country: null,
  state: null,
  grid: null,
  netCategory: "traffic",
  netType: "open",
  expectedDurationMinutes: null,
  visibility: "listed",
  linkToken: "tok-share-abc",
  connections: [hfConnection],
  ownerAccountIds: ["me"],
  owners: [{ accountId: "me", callsign: "W1AW" }],
  archivedAt: null,
  createdAt: "2026-07-16T00:00:00Z",
  updatedAt: "2026-07-16T00:00:00Z",
};

/**
 * A fetch mock dispatching by method + path. `me` overrides the /me account;
 * `onMutate` lets a test observe or override the create/update response.
 */
function makeFetch(opts: {
  me?: unknown;
  meStatus?: number;
  definition?: unknown;
  /** Successive `GET /api/net-definitions/{id}` bodies, consumed in order —
   * how a test says "someone else changed the net between two reads". The last
   * one sticks once the queue is exhausted. */
  definitionResponses?: unknown[];
  /** Per-net-id definitions, for tests that navigate between two edit pages. */
  definitionsById?: Record<string, unknown>;
  /** Net ids whose `GET /api/net-definitions/{id}` never resolves, so a test
   * can hold the window between a route change and its load open. */
  definitionPendingIds?: string[];
  /** After this many successful definition GETs, every later one 500s. */
  definitionFailAfter?: number;
  definitionStatus?: number;
  definitionProblem?: unknown;
  mutateStatus?: number;
  mutateBody?: unknown;
  occurrences?: unknown[];
  scheduleStatus?: number;
  scheduleProblem?: unknown;
  /** The stored rule `GET .../schedule` resolves with. Omitted → 204 (the
   * "this net has no schedule" signal). */
  storedSchedule?: unknown;
  /** Per-net-id stored rules, for tests that navigate between two edit pages.
   * An id absent from the map resolves 204. Takes precedence over
   * `storedSchedule`. */
  schedulesById?: Record<string, unknown>;
  /** Leaves `GET .../schedule` forever pending, so a test can assert the
   * panel is unsavable while its load is unresolved. */
  scheduleGetPending?: boolean;
  /** Forces a failure status on `GET .../schedule`. */
  scheduleGetStatus?: number;
  deliveryConfig?: unknown;
  deliverySetStatus?: number;
  deliverySetBody?: unknown;
  deliverySetProblem?: unknown;
  /** Successive `PUT .../connections` outcomes, consumed in order. An
   * exhausted queue falls through to a plain 200. */
  connectionResponses?: Array<{
    status?: number;
    body?: unknown;
    problem?: unknown;
  }>;
  /** Successive scalar `PUT /api/net-definitions/{id}` bodies, consumed in
   * order — the server bumps the version on every one. */
  scalarResponses?: unknown[];
  calls?: Array<{ url: string; method: string; body: unknown }>;
}) {
  const offConfig = {
    emails: [],
    webhookUrl: null,
    discordWebhookUrl: null,
    webhookConfigured: false,
    webhookSecretSet: false,
  };
  let definitionGets = 0;
  return vi.fn(async (url: string, init?: RequestInit) => {
    const method = init?.method ?? "GET";
    opts.calls?.push({
      url,
      method,
      body: init?.body ? JSON.parse(init.body as string) : undefined,
    });
    if (url === "/api/accounts/me") {
      return opts.meStatus === 401
        ? jsonResponse(401, { type: "/errors/unauthenticated", status: 401 })
        : jsonResponse(200, opts.me ?? account);
    }
    if (url.endsWith("/delivery-config")) {
      if (method === "GET") {
        return jsonResponse(200, opts.deliveryConfig ?? offConfig);
      }
      if (method === "PUT") {
        if (
          opts.deliverySetStatus !== undefined &&
          opts.deliverySetStatus >= 400
        ) {
          return jsonResponse(opts.deliverySetStatus, opts.deliverySetProblem);
        }
        return jsonResponse(200, opts.deliverySetBody ?? offConfig);
      }
      if (method === "DELETE") {
        return jsonResponse(204, null);
      }
    }
    if (url.endsWith("/occurrences") && method === "GET") {
      return jsonResponse(200, { occurrences: opts.occurrences ?? [] });
    }
    if (url.endsWith("/schedule") && method === "GET") {
      if (opts.scheduleGetPending === true) {
        return new Promise<Response>(() => {});
      }
      if (
        opts.scheduleGetStatus !== undefined &&
        opts.scheduleGetStatus >= 400
      ) {
        return jsonResponse(opts.scheduleGetStatus, {
          type: "/errors/internal",
          status: opts.scheduleGetStatus,
        });
      }
      if (opts.schedulesById !== undefined) {
        const id = url.split("/")[3];
        const stored = opts.schedulesById[id];
        return stored === undefined
          ? jsonResponse(204, null)
          : jsonResponse(200, stored);
      }
      return opts.storedSchedule === undefined
        ? jsonResponse(204, null)
        : jsonResponse(200, opts.storedSchedule);
    }
    if (url.endsWith("/schedule") && method === "PUT") {
      if (opts.scheduleStatus !== undefined && opts.scheduleStatus >= 400) {
        return jsonResponse(opts.scheduleStatus, opts.scheduleProblem);
      }
      return jsonResponse(200, { occurrences: opts.occurrences ?? [] });
    }
    if (url.endsWith("/schedule") && method === "DELETE") {
      return jsonResponse(204, null);
    }
    if (url.startsWith("/api/net-definitions/") && method === "GET") {
      if (opts.definitionPendingIds?.includes(url.split("/")[3]) === true) {
        return new Promise<Response>(() => {});
      }
      if (opts.definitionFailAfter !== undefined) {
        definitionGets += 1;
        if (definitionGets > opts.definitionFailAfter) {
          return jsonResponse(500, {
            type: "/errors/internal",
            status: 500,
          });
        }
      }
      if (opts.definitionStatus !== undefined && opts.definitionStatus >= 400) {
        return jsonResponse(
          opts.definitionStatus,
          opts.definitionProblem ?? {
            type: "/errors/net-definition-not-found",
            status: opts.definitionStatus,
          },
        );
      }
      if (opts.definitionsById !== undefined) {
        const netId = url.split("/")[3];
        return jsonResponse(200, opts.definitionsById[netId] ?? created);
      }
      if (opts.definitionResponses !== undefined) {
        const next =
          opts.definitionResponses.length > 1
            ? opts.definitionResponses.shift()
            : opts.definitionResponses[0];
        return jsonResponse(200, next ?? created);
      }
      return jsonResponse(200, opts.definition ?? created);
    }
    if (url.endsWith("/connections") && method === "PUT") {
      const next = opts.connectionResponses?.shift();
      if (next !== undefined && next.status !== undefined && next.status >= 400) {
        return jsonResponse(next.status, next.problem);
      }
      return jsonResponse(
        200,
        next?.body ?? opts.mutateBody ?? opts.definition ?? created,
      );
    }
    if (method === "POST" || method === "PUT") {
      const next = opts.scalarResponses?.shift();
      if (next !== undefined) {
        return jsonResponse(200, next);
      }
      return jsonResponse(
        opts.mutateStatus ?? 201,
        opts.mutateBody ?? created,
      );
    }
    return jsonResponse(204, null);
  });
}

function renderAt(path: string, fetchMock: ReturnType<typeof vi.fn>) {
  vi.stubGlobal("fetch", fetchMock);
  const router = createMemoryRouter(
    [
      { path: "/nets/new", element: <NetDefinitionFormPage /> },
      { path: "/nets/:id/edit", element: <NetDefinitionFormPage /> },
      { path: "/sign-in", element: <div data-testid="sign-in" /> },
      { path: "/consent", element: <div data-testid="consent" /> },
      { path: "/profile", element: <div data-testid="profile" /> },
    ],
    { initialEntries: [path] },
  );
  render(<RouterProvider router={router} />);
  return router;
}

afterEach(() => {
  vi.unstubAllGlobals();
});

describe("NetDefinitionFormPage gating", () => {
  it("sends the signed-out to sign-in", async () => {
    renderAt("/nets/new", makeFetch({ meStatus: 401 }));
    expect(await screen.findByTestId("sign-in")).toBeInTheDocument();
  });

  it("sends an unconsented account to the consent gate", async () => {
    renderAt(
      "/nets/new",
      makeFetch({ me: { ...account, consentRequired: true } }),
    );
    expect(await screen.findByTestId("consent")).toBeInTheDocument();
  });

  it("sends a callsign-less account to the profile", async () => {
    renderAt("/nets/new", makeFetch({ me: { ...account, callsign: null } }));
    expect(await screen.findByTestId("profile")).toBeInTheDocument();
  });
});

describe("NetDefinitionFormPage chrome", () => {
  it("gives Description a multi-line box, not a one-line input", async () => {
    renderAt("/nets/new", makeFetch({}));

    const description = await screen.findByLabelText("Description");

    // A net's description is prose (agenda, area, who it's for) — a single
    // line hides everything past the first few words while typing.
    expect(description.tagName).toBe("TEXTAREA");
    expect(Number(description.getAttribute("rows"))).toBeGreaterThan(1);
  });

  it("still submits the typed description through the multi-line control", async () => {
    const calls: Array<{ url: string; method: string; body: unknown }> = [];
    renderAt("/nets/new", makeFetch({ calls }));

    await userEvent.type(await screen.findByLabelText("Title"), "Net");
    await userEvent.type(
      screen.getByLabelText("Description"),
      "Traffic net for the Boise area",
    );
    await userEvent.type(
      within(screen.getByTestId("connection-row")).getByLabelText("Frequency (MHz)"),
      "14.230",
    );
    await userEvent.click(screen.getByRole("button", { name: /create net/i }));

    await waitFor(() => {
      expect(
        calls.find((c) => c.method === "POST")?.body,
      ).toMatchObject({ description: "Traffic net for the Boise area" });
    });
  });

  it("frames the form in the same elevated panel as every other surface", async () => {
    renderAt("/nets/new", makeFetch({}));

    const title = await screen.findByLabelText("Title");
    const panel = title.closest("section");

    expect(panel).toHaveStyle({
      background: "var(--surface)",
      boxShadow: "var(--shadow)",
      overflow: "hidden",
    });
    // The sticky summary rail lives inside the same frame as the fields it
    // summarizes, so there is one panel, not a form card beside a rail card.
    expect(document.querySelectorAll("section[style*='box-shadow']")).toHaveLength(1);
  });
});

describe("NetDefinitionFormPage create", () => {
  it("POSTs the entered fields and navigates to the edit route on success", async () => {
    const calls: Array<{ url: string; method: string; body: unknown }> = [];
    const router = renderAt("/nets/new", makeFetch({ calls }));

    const title = await screen.findByLabelText("Title");
    await userEvent.type(title, "Sunday Traffic Net");
    await userEvent.type(
      within(screen.getByTestId("connection-row")).getByLabelText("Frequency (MHz)"),
      "14.230",
    );
    await userEvent.click(screen.getByRole("button", { name: /create net/i }));

    await waitFor(() => {
      expect(router.state.location.pathname).toBe("/nets/new-id/edit");
    });
    const post = calls.find((c) => c.method === "POST");
    expect(post?.url).toBe("/api/net-definitions");
    // The net is born with its ways in: the create body carries the connection
    // list in the SAME shape the edit page writes — the read's vocabulary, Hz
    // integers — and no flat operating key.
    expect(post?.body).toMatchObject({
      title: "Sunday Traffic Net",
      netType: "open",
      visibility: "listed",
      connections: [
        { kind: "hf", plannedFrequencyHz: 14_230_000, band: "20m", mode: "ssb" },
      ],
    });
    expect(post?.body).not.toHaveProperty("plannedFrequency");
    expect(post?.body).not.toHaveProperty("band");
  });

  it("submits the chosen unlisted visibility in the create payload", async () => {
    const calls: Array<{ url: string; method: string; body: unknown }> = [];
    renderAt("/nets/new", makeFetch({ calls }));

    await userEvent.type(
      await screen.findByLabelText("Title"),
      "Private Net",
    );
    await userEvent.type(
      within(screen.getByTestId("connection-row")).getByLabelText("Frequency (MHz)"),
      "14.230",
    );
    // Visibility is a radio-card picker in the sticky rail (task 90), not a
    // <select> — the accessible name is the card's title only ("Listed" /
    // "Unlisted"), its longer description rides on aria-describedby.
    await userEvent.click(screen.getByRole("radio", { name: "Unlisted" }));
    await userEvent.click(screen.getByRole("button", { name: /create net/i }));

    await waitFor(() => {
      const post = calls.find((c) => c.method === "POST");
      expect(post?.body).toMatchObject({ visibility: "unlisted" });
    });
  });

  it("shows the server's field-level detail when create is rejected", async () => {
    const fetchMock = makeFetch({
      mutateStatus: 400,
      mutateBody: {
        type: "/errors/net-definition-invalid",
        status: 400,
        detail: "band: is not a recognized band",
      },
    });
    renderAt("/nets/new", fetchMock);

    await userEvent.type(await screen.findByLabelText("Title"), "T");
    await userEvent.type(
      within(screen.getByTestId("connection-row")).getByLabelText("Frequency (MHz)"),
      "14.230",
    );
    await userEvent.click(screen.getByRole("button", { name: /create net/i }));

    expect(await screen.findByRole("alert")).toHaveTextContent(
      "band: is not a recognized band",
    );
  });

  it("prefers the server's problem detail over the slug-map fallback", async () => {
    // One of forty-two surfaces. This page already ordered
    // `detail` ahead of the map inline; the test pins the ORDER now that a
    // shared resolver owns it, with the map half computed rather than typed.
    const detail = "the field-naming answer the server sent";
    const fetchMock = makeFetch({
      mutateStatus: 400,
      mutateBody: {
        type: "/errors/net-definition-invalid",
        status: 400,
        detail,
      },
    });
    renderAt("/nets/new", fetchMock);

    await userEvent.type(await screen.findByLabelText("Title"), "T");
    await userEvent.type(
      within(screen.getByTestId("connection-row")).getByLabelText("Frequency (MHz)"),
      "14.230",
    );
    await userEvent.click(screen.getByRole("button", { name: /create net/i }));

    const alert = await screen.findByRole("alert");
    expect(alert).toHaveTextContent(detail);
    expect(alert).not.toHaveTextContent(
      messageForProblemType("/errors/net-definition-invalid"),
    );
  });
});

describe("NetDefinitionFormPage edit", () => {
  it("loads the existing definition, prefills the title, and PUTs on save", async () => {
    const calls: Array<{ url: string; method: string; body: unknown }> = [];
    const fetchMock = makeFetch({
      calls,
      definition: { ...created, id: "abc", title: "Old Title" },
      mutateBody: { ...created, id: "abc", title: "New Title", definitionVersion: 2 },
    });
    renderAt("/nets/abc/edit", fetchMock);

    const title = (await screen.findByLabelText("Title")) as HTMLInputElement;
    await waitFor(() => expect(title.value).toBe("Old Title"));

    await userEvent.clear(title);
    await userEvent.type(title, "New Title");
    await userEvent.click(screen.getByRole("button", { name: /save changes/i }));

    await waitFor(() => {
      const put = calls.find(
        (c) => c.method === "PUT" && c.url === "/api/net-definitions/abc",
      );
      expect(put?.body).toMatchObject({ title: "New Title" });
    });
    expect(await screen.findByText(/version 2/i)).toBeInTheDocument();
  });

  it("renders a share link built from the loaded link token in edit mode", async () => {
    const fetchMock = makeFetch({
      definition: { ...created, id: "abc", linkToken: "tok-share-abc" },
    });
    renderAt("/nets/abc/edit", fetchMock);

    const share = (await screen.findByLabelText(
      "Share link",
    )) as HTMLInputElement;
    expect(share.value).toContain("/nets/t/tok-share-abc");
  });

  it("surfaces an alert when the definition fails to load, instead of a silent blank form", async () => {
    const fetchMock = makeFetch({
      definitionStatus: 404,
      definitionProblem: {
        type: "/errors/net-definition-not-found",
        status: 404,
      },
    });
    renderAt("/nets/missing/edit", fetchMock);

    expect(await screen.findByRole("alert")).toHaveTextContent(
      "That net no longer exists — it may have been deleted.",
    );
  });
});

describe("NetDefinitionFormPage owner panel", () => {
  const twoOwners = {
    ...created,
    id: "abc",
    ownerAccountIds: ["me", "other"],
    owners: [
      { accountId: "me", callsign: "W1AW" },
      { accountId: "other", callsign: "K2XYZ" },
    ],
  };

  /**
   * A stateful fetch mock for the owner panel: GET reflects the current owner
   * set, POST /owners adds by callsign, DELETE /owners/:id removes. `problem`
   * forces the next owner mutation to fail with that problem body.
   */
  function ownerFetch(opts: {
    calls: Array<{ url: string; method: string; body: unknown }>;
    problem?: { status: number; type: string };
  }) {
    const owners = [
      { accountId: "me", callsign: "W1AW" },
      { accountId: "other", callsign: "K2XYZ" },
    ];
    return vi.fn(async (url: string, init?: RequestInit) => {
      const method = init?.method ?? "GET";
      opts.calls.push({
        url,
        method,
        body: init?.body ? JSON.parse(init.body as string) : undefined,
      });
      if (url === "/api/accounts/me") {
        return jsonResponse(200, account);
      }
      if (url.endsWith("/occurrences") && method === "GET") {
        return jsonResponse(200, { occurrences: [] });
      }
      if (url.endsWith("/delivery-config") && method === "GET") {
        return jsonResponse(200, {
          emails: [],
          webhookUrl: null,
          discordWebhookUrl: null,
          webhookConfigured: false,
          webhookSecretSet: false,
        });
      }
      const def = () => ({
        ...twoOwners,
        ownerAccountIds: owners.map((o) => o.accountId),
        owners: [...owners],
      });
      if (url.endsWith("/owners") && method === "POST") {
        if (opts.problem) {
          return jsonResponse(opts.problem.status, opts.problem);
        }
        owners.push({ accountId: "third", callsign: "N3ABC" });
        return jsonResponse(200, def());
      }
      if (url.includes("/owners/") && method === "DELETE") {
        if (opts.problem) {
          return jsonResponse(opts.problem.status, opts.problem);
        }
        const id = url.split("/owners/")[1];
        const idx = owners.findIndex((o) => o.accountId === id);
        if (idx >= 0) owners.splice(idx, 1);
        return jsonResponse(204, null);
      }
      if (url.startsWith("/api/net-definitions/") && method === "GET") {
        return jsonResponse(200, def());
      }
      return jsonResponse(200, def());
    });
  }

  it("renders the current owners by callsign in edit mode", async () => {
    const calls: Array<{ url: string; method: string; body: unknown }> = [];
    renderAt("/nets/abc/edit", ownerFetch({ calls }));

    expect(await screen.findByText("W1AW")).toBeInTheDocument();
    expect(await screen.findByText("K2XYZ")).toBeInTheDocument();
  });

  it("submitting the add-callsign control POSTs to the owners endpoint", async () => {
    const calls: Array<{ url: string; method: string; body: unknown }> = [];
    renderAt("/nets/abc/edit", ownerFetch({ calls }));

    await screen.findByText("W1AW");
    await userEvent.type(
      screen.getByLabelText("Add co-owner by callsign"),
      "n3abc",
    );
    await userEvent.click(screen.getByRole("button", { name: /add owner/i }));

    await waitFor(() => {
      const post = calls.find((c) => c.method === "POST");
      expect(post?.url).toBe("/api/net-definitions/abc/owners");
      expect(post?.body).toEqual({ callsign: "n3abc" });
    });
    expect(await screen.findByText("N3ABC")).toBeInTheDocument();
  });

  it("a remove control DELETEs the owner by accountId", async () => {
    const calls: Array<{ url: string; method: string; body: unknown }> = [];
    renderAt("/nets/abc/edit", ownerFetch({ calls }));

    await screen.findByText("K2XYZ");
    // Each owner row has a remove control keyed by callsign.
    await userEvent.click(
      screen.getByRole("button", { name: /remove K2XYZ/i }),
    );

    await waitFor(() => {
      const del = calls.find((c) => c.method === "DELETE");
      expect(del?.url).toBe("/api/net-definitions/abc/owners/other");
    });
  });

  it("surfaces a last-owner refusal via the mapped alert message", async () => {
    const calls: Array<{ url: string; method: string; body: unknown }> = [];
    renderAt(
      "/nets/abc/edit",
      ownerFetch({
        calls,
        problem: { status: 409, type: "/errors/last-owner" },
      }),
    );

    await screen.findByText("W1AW");
    await userEvent.click(
      screen.getByRole("button", { name: /remove W1AW/i }),
    );

    const alert = await screen.findByRole("alert");
    expect(alert).toHaveTextContent(
      "A net must keep at least one owner — add another owner before removing yourself, or delete the net.",
    );
  });
});

describe("NetDefinitionFormPage schedule", () => {
  it("does not show the schedule section in create mode", async () => {
    renderAt("/nets/new", makeFetch({}));
    await screen.findByLabelText("Title");
    expect(screen.queryByLabelText("Schedule kind")).not.toBeInTheDocument();
  });

  it("does not claim a fake cadence in the summary rail before a schedule can even be set", async () => {
    // Regression: the Cadence summary read `schedule` state's unset DEFAULTS
    // (e.g. "Weekly · Tuesday 20:00") on the create form, where the Schedule
    // fieldset doesn't exist yet — implying a schedule was already
    // configured when nothing had been set at all, and there was no way to
    // change it on this screen.
    renderAt("/nets/new", makeFetch({}));
    await screen.findByLabelText("Title");

    const summary = screen.getByRole("region", { name: "Summary" });
    expect(within(summary).getByText("Set after creating")).toBeInTheDocument();
    expect(within(summary).queryByText(/weekly|tuesday|monthly|one-off/i)).not.toBeInTheDocument();

    // A visible hint that schedule-setting comes right after creating, not a
    // dead end — the create flow redirects to the edit route on success.
    expect(
      screen.getByText(/you'll set the schedule.*next screen/i),
    ).toBeInTheDocument();
  });

  it("toggles one-off vs recurring inputs by the chosen kind", async () => {
    renderAt("/nets/abc/edit", makeFetch({}));
    // Recurring is the default: frequency/time visible, one-off start absent.
    expect(await screen.findByLabelText("Frequency")).toBeInTheDocument();
    expect(screen.getByLabelText("Time of day")).toBeInTheDocument();
    expect(screen.queryByLabelText("One-off start")).not.toBeInTheDocument();

    // Schedule kind is now a pill-style segmented control (task 89), not a
    // <select> — same underlying `schedule.kind` state, click instead of select.
    await userEvent.click(screen.getByRole("radio", { name: "One-off" }));
    expect(screen.getByLabelText("One-off start")).toBeInTheDocument();
    expect(screen.queryByLabelText("Frequency")).not.toBeInTheDocument();
  });

  it("shows the weekday input only for a weekly frequency", async () => {
    renderAt("/nets/abc/edit", makeFetch({}));
    expect(await screen.findByLabelText("Weekday")).toBeInTheDocument();

    await userEvent.selectOptions(screen.getByLabelText("Frequency"), "monthly");
    expect(screen.queryByLabelText("Weekday")).not.toBeInTheDocument();
    expect(screen.getByLabelText("Day of month")).toBeInTheDocument();
  });

  it("PUTs the schedule and renders the returned occurrences in local time", async () => {
    const calls: Array<{ url: string; method: string; body: unknown }> = [];
    const iso = "2027-01-05T20:00:00+00:00";
    renderAt(
      "/nets/abc/edit",
      makeFetch({ calls, occurrences: [{ id: "o1", scheduledStartAt: iso }] }),
    );

    await screen.findByLabelText("Frequency");
    await userEvent.click(
      screen.getByRole("button", { name: /save schedule/i }),
    );

    // The PUT hits the schedule sub-resource with the recurring body.
    await waitFor(() => {
      const put = calls.find(
        (c) => c.method === "PUT" && c.url.endsWith("/schedule"),
      );
      expect(put).toBeTruthy();
      expect(put?.body).toMatchObject({
        kind: "recurring",
        frequency: "weekly",
        timeOfDay: "20:00",
        weekday: "tuesday",
      });
    });

    // The occurrence renders via the local-time formatter (the exact string is
    // whatever the platform's toLocaleString produces for that instant).
    const list = await screen.findByLabelText("Upcoming occurrences");
    expect(list).toHaveTextContent(new Date(iso).toLocaleString());
  });

  it("PUTs a one-off schedule as an RFC 3339 UTC instant, not the raw datetime-local value", async () => {
    // <input type="datetime-local"> yields a bare "YYYY-MM-DDTHH:MM" with no
    // seconds and no offset — never valid RFC 3339. The wire body must carry
    // the wall-clock time interpreted in the CHOSEN timezone, converted to a
    // real UTC instant.
    const calls: Array<{ url: string; method: string; body: unknown }> = [];
    renderAt("/nets/abc/edit", makeFetch({ calls, occurrences: [] }));

    await screen.findByLabelText("Frequency");
    await userEvent.click(screen.getByRole("radio", { name: "One-off" }));
    await userEvent.selectOptions(
      screen.getByLabelText("Timezone"),
      "America/New_York",
    );
    const startInput = screen.getByLabelText("One-off start");
    await userEvent.clear(startInput);
    await userEvent.type(startInput, "2027-06-15T20:00");

    await userEvent.click(
      screen.getByRole("button", { name: /save schedule/i }),
    );

    await waitFor(() => {
      const put = calls.find(
        (c) => c.method === "PUT" && c.url.endsWith("/schedule"),
      );
      expect(put).toBeTruthy();
      const body = put?.body as { kind: string; oneOffStartAt: string };
      expect(body.kind).toBe("one-off");
      // Must be a real RFC 3339 instant the backend can parse — never the
      // bare datetime-local string. 2027-06-15 is in EDT (UTC-4), so 20:00
      // local == 00:00 UTC the next day.
      expect(new Date(body.oneOffStartAt).toISOString()).toBe(
        "2027-06-16T00:00:00.000Z",
      );
      expect(body.oneOffStartAt).not.toBe("2027-06-15T20:00");
    });
  });

  it("surfaces a schedule-invalid failure via the mapped alert, not a hardcoded string", async () => {
    renderAt(
      "/nets/abc/edit",
      makeFetch({
        scheduleStatus: 400,
        scheduleProblem: {
          type: "/errors/schedule-invalid",
          status: 400,
          detail: "weekday: is required for a weekly schedule",
        },
      }),
    );

    await screen.findByLabelText("Frequency");
    await userEvent.click(
      screen.getByRole("button", { name: /save schedule/i }),
    );

    // The field-level detail rides through (the net-definition-invalid pattern).
    const alert = await screen.findByRole("alert");
    expect(alert).toHaveTextContent("weekday: is required for a weekly schedule");
  });

  it("clears the schedule and empties the occurrence list", async () => {
    const calls: Array<{ url: string; method: string; body: unknown }> = [];
    renderAt(
      "/nets/abc/edit",
      makeFetch({
        calls,
        occurrences: [
          { id: "o1", scheduledStartAt: "2027-01-05T20:00:00+00:00" },
        ],
      }),
    );

    // The initial load shows one occurrence.
    const list = await screen.findByLabelText("Upcoming occurrences");
    await waitFor(() => expect(list.children.length).toBe(1));

    await userEvent.click(
      screen.getByRole("button", { name: /clear schedule/i }),
    );

    await waitFor(() => {
      const del = calls.find(
        (c) => c.method === "DELETE" && c.url.endsWith("/schedule"),
      );
      expect(del?.url).toBe("/api/net-definitions/abc/schedule");
    });
    await waitFor(() =>
      expect(
        screen.getByLabelText("Upcoming occurrences").children.length,
      ).toBe(0),
    );
  });
});

describe("NetDefinitionFormPage schedule hydration", () => {
  it("hydrates the panel from the stored rule, so an unchanged save round-trips it", async () => {
    // The panel once mounted on hardcoded defaults (weekly/tuesday/20:00) and
    // its full-replace PUT rewrote the real rule.
    const calls: Array<{ url: string; method: string; body: unknown }> = [];
    const stored = {
      kind: "recurring",
      timezone: "America/New_York",
      frequency: "weekly",
      timeOfDay: "21:30",
      weekday: "friday",
    };
    renderAt(
      "/nets/abc/edit",
      makeFetch({ calls, storedSchedule: stored, occurrences: [] }),
    );

    // Every control reads the STORED value, not the default.
    await waitFor(() =>
      expect(screen.getByLabelText("Weekday")).toHaveValue("friday"),
    );
    expect(screen.getByLabelText("Frequency")).toHaveValue("weekly");
    expect(screen.getByLabelText("Time of day")).toHaveValue("21:30");
    expect(screen.getByLabelText("Timezone")).toHaveValue("America/New_York");

    // A save with no user edit sends back exactly the rule that was stored —
    // the no-op the old panel could not produce.
    await userEvent.click(
      screen.getByRole("button", { name: /save schedule/i }),
    );
    await waitFor(() => {
      const put = calls.find(
        (c) => c.method === "PUT" && c.url.endsWith("/schedule"),
      );
      expect(put).toBeTruthy();
      expect(put?.body).toEqual(stored);
    });
  });

  it("hydrates a stored one-off back into its wall-clock instant", async () => {
    // The stored instant is UTC; the form field is wall-clock in the net's own
    // timezone. 2027-06-16T00:00Z is 2027-06-15 20:00 in EDT — re-saving must
    // resolve to the SAME instant, not shift it.
    const calls: Array<{ url: string; method: string; body: unknown }> = [];
    renderAt(
      "/nets/abc/edit",
      makeFetch({
        calls,
        storedSchedule: {
          kind: "one-off",
          timezone: "America/New_York",
          oneOffStartAt: "2027-06-16T00:00:00+00:00",
        },
        occurrences: [],
      }),
    );

    await waitFor(() =>
      expect(screen.getByLabelText("One-off start")).toHaveValue(
        "2027-06-15T20:00",
      ),
    );

    await userEvent.click(
      screen.getByRole("button", { name: /save schedule/i }),
    );
    await waitFor(() => {
      const put = calls.find(
        (c) => c.method === "PUT" && c.url.endsWith("/schedule"),
      );
      const body = put?.body as { kind: string; oneOffStartAt: string };
      expect(body.kind).toBe("one-off");
      expect(new Date(body.oneOffStartAt).toISOString()).toBe(
        "2027-06-16T00:00:00.000Z",
      );
    });
  });

  it("summarises the loaded rule in the cadence rail, not the mount defaults", async () => {
    // Trap: `cadenceSummary` reads the same `schedule` state, so an unhydrated
    // panel made the rail assert a cadence the net never had.
    renderAt(
      "/nets/abc/edit",
      makeFetch({
        storedSchedule: {
          kind: "recurring",
          timezone: "America/New_York",
          frequency: "weekly",
          timeOfDay: "21:30",
          weekday: "friday",
        },
      }),
    );

    expect(await screen.findByText("Weekly · Friday 21:30")).toBeInTheDocument();
    expect(screen.queryByText(/Tuesday 20:00/)).not.toBeInTheDocument();
  });

  it("cannot be submitted while the stored rule is still loading", async () => {
    // "unscheduled" and "scheduled, still loading" must never
    // be indistinguishable — an unresolved panel is not savable at all, so a
    // fast click can't PUT the defaults over a rule that is still in flight.
    const calls: Array<{ url: string; method: string; body: unknown }> = [];
    renderAt(
      "/nets/abc/edit",
      makeFetch({ calls, scheduleGetPending: true }),
    );

    const save = await screen.findByRole("button", { name: /save schedule/i });
    expect(save).toBeDisabled();
    expect(
      screen.getByRole("button", { name: /clear schedule/i }),
    ).toBeDisabled();

    // A disabled control with no stated reason is indistinguishable from a dead
    // panel — and a request that never settles stays here forever.
    expect(screen.getByRole("status")).toBeInTheDocument();

    await userEvent.click(save);
    expect(
      calls.filter((c) => c.method === "PUT" && c.url.endsWith("/schedule")),
    ).toHaveLength(0);
  });

  it("does not carry one net's rule into another net that has none", async () => {
    // The edit route reuses this component instance across an id change
    // (`router.tsx` registers it with no `key`), so schedule state survives the
    // navigation. A net with NO stored rule must fall back to the defaults — if
    // it inherited the previous net's rule, the full-replace PUT would write
    // net A's schedule onto net B on a single unwitting click.
    const calls: Array<{ url: string; method: string; body: unknown }> = [];
    const router = renderAt(
      "/nets/scheduled-net/edit",
      makeFetch({
        calls,
        schedulesById: {
          "scheduled-net": {
            kind: "recurring",
            timezone: "America/New_York",
            frequency: "weekly",
            timeOfDay: "21:30",
            weekday: "friday",
          },
        },
      }),
    );

    await waitFor(() =>
      expect(screen.getByLabelText("Weekday")).toHaveValue("friday"),
    );

    await router.navigate("/nets/unscheduled-net/edit");

    // Back to the defaults, not Friday 21:30 carried over.
    await waitFor(() =>
      expect(screen.getByLabelText("Weekday")).toHaveValue("tuesday"),
    );
    expect(screen.getByLabelText("Time of day")).toHaveValue("20:00");

    await waitFor(() =>
      expect(
        screen.getByRole("button", { name: /save schedule/i }),
      ).toBeEnabled(),
    );
    await userEvent.click(
      screen.getByRole("button", { name: /save schedule/i }),
    );

    await waitFor(() => {
      const put = calls.find(
        (c) => c.method === "PUT" && c.url.endsWith("/schedule"),
      );
      expect(put?.url).toBe("/api/net-definitions/unscheduled-net/schedule");
      expect(put?.body).not.toMatchObject({ weekday: "friday" });
      expect(put?.body).toMatchObject({ weekday: "tuesday" });
    });
  });

  it("offers the defaults as a starting point for a net with no schedule", async () => {
    // The 204 arm: no stored rule, so the defaults stand — and the panel IS
    // savable, because its load resolved.
    renderAt("/nets/abc/edit", makeFetch({}));

    await waitFor(() =>
      expect(
        screen.getByRole("button", { name: /save schedule/i }),
      ).toBeEnabled(),
    );
    expect(screen.getByLabelText("Weekday")).toHaveValue("tuesday");
    expect(screen.getByLabelText("Time of day")).toHaveValue("20:00");
  });

  it("does not leave the panel permanently unsavable when the read fails", async () => {
    // The sibling sub-resource loads swallow errors so a transient failure does
    // not blank the form; the schedule load follows that posture, but must
    // still RESOLVE, or a blip would lock the owner out of their own schedule.
    renderAt(
      "/nets/abc/edit",
      makeFetch({ scheduleGetStatus: 500 }),
    );

    await waitFor(() =>
      expect(
        screen.getByRole("button", { name: /save schedule/i }),
      ).toBeEnabled(),
    );
  });
});

describe("NetDefinitionFormPage delivery config", () => {
  it("sends the entered emails and webhook URL on save", async () => {
    const calls: Array<{ url: string; method: string; body: unknown }> = [];
    const fetchMock = makeFetch({
      calls,
      definition: { ...created, id: "abc" },
      deliverySetBody: {
        emails: ["alerts@example.com"],
        webhookUrl: "https://hooks.example.com/net",
        discordWebhookUrl: "https://discord.com/api/webhooks/12/tok",
        webhookConfigured: true,
        webhookSecretSet: true,
        webhookSecret: "one-time-secret",
      },
    });
    renderAt("/nets/abc/edit", fetchMock);

    await userEvent.click(
      await screen.findByRole("button", { name: /add email/i }),
    );
    await userEvent.type(
      screen.getByLabelText("Delivery email 1"),
      "alerts@example.com",
    );
    await userEvent.type(
      screen.getByLabelText("Webhook URL"),
      "https://hooks.example.com/net",
    );
    // A single change event rather than `userEvent.type`. This file's header
    // records that its per-keystroke typing is what pushes these tests past
    // their async budget on a contended host; a 39-character URL is 39 more
    // keystrokes in the slowest suite in the app, and nothing here is testing
    // the browser's keyboard handling.
    fireEvent.change(screen.getByLabelText("Discord webhook URL"), {
      target: { value: "https://discord.com/api/webhooks/12/tok" },
    });
    await userEvent.click(
      screen.getByRole("button", { name: /save delivery/i }),
    );

    await waitFor(() => {
      const put = calls.find(
        (c) => c.method === "PUT" && c.url.endsWith("/delivery-config"),
      );
      expect(put?.body).toEqual({
        emails: ["alerts@example.com"],
        webhookUrl: "https://hooks.example.com/net",
        discordWebhookUrl: "https://discord.com/api/webhooks/12/tok",
      });
    });
  });

  it("loads a stored Discord URL into its own field and clears it on save", async () => {
    // The Discord field is a full peer of the Webhook URL field: loaded from the
    // GET, sent on the PUT, and cleared by emptying it — not a write-only flag.
    const calls: Array<{ url: string; method: string; body: unknown }> = [];
    const fetchMock = makeFetch({
      calls,
      definition: { ...created, id: "abc" },
      deliveryConfig: {
        emails: [],
        webhookUrl: null,
        discordWebhookUrl: "https://discord.com/api/webhooks/12/tok",
        webhookConfigured: false,
        webhookSecretSet: false,
      },
      deliverySetBody: {
        emails: [],
        webhookUrl: null,
        discordWebhookUrl: null,
        webhookConfigured: false,
        webhookSecretSet: false,
      },
    });
    renderAt("/nets/abc/edit", fetchMock);

    const field = await screen.findByLabelText("Discord webhook URL");
    await waitFor(() =>
      expect(field).toHaveValue("https://discord.com/api/webhooks/12/tok"),
    );

    // One atomic change, for the same reason as above: `userEvent.clear`
    // deletes character by character, and a run that overran its budget
    // mid-delete captured a PARTIALLY cleared value in the PUT body.
    fireEvent.change(field, { target: { value: "" } });
    await userEvent.click(
      screen.getByRole("button", { name: /save delivery/i }),
    );

    await waitFor(() => {
      const put = calls.find(
        (c) => c.method === "PUT" && c.url.endsWith("/delivery-config"),
      );
      expect(put?.body).toEqual({
        emails: [],
        webhookUrl: null,
        discordWebhookUrl: null,
      });
    });
    await waitFor(() => expect(field).toHaveValue(""));
  });

  it("takes the Discord URL the save RESPONSE returns, not the one that was typed", async () => {
    // The save-side write-back, made assertable. The clearing test above cannot
    // prove it: that test sets the field to "" itself, so its closing
    // `toHaveValue("")` is true before, during and after the save — deleting
    // `setDiscordWebhookUrl(result.discordWebhookUrl ?? "")` left all 44 tests
    // in this file green (found 2026-08-27 by independent review). The only
    // fixture that can fail is one where the response DIFFERS from the typed
    // value, which is exactly what the read-back is for: the server trims and
    // the field must show what is actually stored.
    const calls: Array<{ url: string; method: string; body: unknown }> = [];
    const stored = "https://discord.com/api/webhooks/99/stored-tok";
    const fetchMock = makeFetch({
      calls,
      definition: { ...created, id: "abc" },
      deliveryConfig: {
        emails: [],
        webhookUrl: null,
        discordWebhookUrl: null,
        webhookConfigured: false,
        webhookSecretSet: false,
      },
      deliverySetBody: {
        emails: [],
        webhookUrl: null,
        discordWebhookUrl: stored,
        webhookConfigured: false,
        webhookSecretSet: false,
      },
    });
    renderAt("/nets/abc/edit", fetchMock);

    const field = await screen.findByLabelText("Discord webhook URL");
    fireEvent.change(field, {
      target: { value: "  https://discord.com/api/webhooks/12/typed-tok  " },
    });
    await userEvent.click(
      screen.getByRole("button", { name: /save delivery/i }),
    );

    // The PUT carries the trimmed TYPED value…
    await waitFor(() => {
      const put = calls.find(
        (c) => c.method === "PUT" && c.url.endsWith("/delivery-config"),
      );
      expect(put?.body).toEqual({
        emails: [],
        webhookUrl: null,
        discordWebhookUrl: "https://discord.com/api/webhooks/12/typed-tok",
      });
    });
    // …and the field then shows the RESPONSE's value, which is a different
    // string, so only the write-back can put it there.
    await waitFor(() => expect(field).toHaveValue(stored));
  });

  it("reveals a minted secret once with a copy affordance", async () => {
    const fetchMock = makeFetch({
      definition: { ...created, id: "abc" },
      deliverySetBody: {
        emails: [],
        webhookUrl: "https://hooks.example.com/net",
        webhookConfigured: true,
        webhookSecretSet: true,
        webhookSecret: "one-time-secret",
      },
    });
    renderAt("/nets/abc/edit", fetchMock);

    await userEvent.type(
      await screen.findByLabelText("Webhook URL"),
      "https://hooks.example.com/net",
    );
    await userEvent.click(
      screen.getByRole("button", { name: /save delivery/i }),
    );

    // The reveal-once panel shows the secret VALUE in a field, plus a copy button.
    const secretField = (await screen.findByLabelText(
      "Webhook signing secret",
    )) as HTMLInputElement;
    expect(secretField.value).toBe("one-time-secret");
    expect(
      screen.getByRole("button", { name: /copy webhook signing secret/i }),
    ).toBeInTheDocument();
  });

  it("shows a secret is set on load without ever exposing the plaintext", async () => {
    const fetchMock = makeFetch({
      definition: { ...created, id: "abc" },
      deliveryConfig: {
        emails: ["a@example.com"],
        webhookUrl: "https://hooks.example.com/net",
        webhookConfigured: true,
        webhookSecretSet: true,
      },
    });
    renderAt("/nets/abc/edit", fetchMock);

    // The "secret is set" indicator appears; no reveal-once field is rendered.
    expect(
      await screen.findByTestId("webhook-secret-set"),
    ).toBeInTheDocument();
    expect(
      screen.queryByLabelText("Webhook signing secret"),
    ).not.toBeInTheDocument();
  });

  it("surfaces a config-time rejection via the mapped alert", async () => {
    const fetchMock = makeFetch({
      definition: { ...created, id: "abc" },
      deliverySetStatus: 422,
      deliverySetProblem: {
        type: "/errors/delivery-config-invalid",
        status: 422,
        detail: "webhook url: must use https",
      },
    });
    renderAt("/nets/abc/edit", fetchMock);

    await userEvent.type(
      await screen.findByLabelText("Webhook URL"),
      "http://hooks.example.com/net",
    );
    await userEvent.click(
      screen.getByRole("button", { name: /save delivery/i }),
    );

    expect(await screen.findByRole("alert")).toHaveTextContent(
      "webhook url: must use https",
    );
  });
});

describe("NetDefinitionFormPage design-handoff layout (3a)", () => {
  it("renders a Nets > My Nets breadcrumb trail", async () => {
    renderAt("/nets/new", makeFetch({}));
    await screen.findByLabelText("Title");

    const nav = screen.getByRole("navigation", { name: "Breadcrumb" });
    expect(nav).toHaveTextContent("Nets");
    expect(nav).toHaveTextContent("My Nets");
    expect(nav).toHaveTextContent("New net");
  });

  it("groups the edit-mode form into Identity/Operating/Geography/Schedule/Owners fieldsets", async () => {
    renderAt("/nets/abc/edit", makeFetch({}));
    await screen.findByLabelText("Title");

    expect(screen.getByRole("group", { name: "Identity" })).toBeInTheDocument();
    expect(screen.getByRole("group", { name: "Operating" })).toBeInTheDocument();
    expect(screen.getByRole("group", { name: "Geography" })).toBeInTheDocument();
    expect(screen.getByRole("group", { name: "Schedule" })).toBeInTheDocument();
    expect(screen.getByRole("group", { name: "Owners" })).toBeInTheDocument();
  });

  it("renders Net type as a segmented control defaulting to Open", async () => {
    renderAt("/nets/new", makeFetch({}));
    await screen.findByLabelText("Title");

    const openRadio = screen.getByRole("radio", { name: "Open" });
    const rollCallRadio = screen.getByRole("radio", { name: "Roll-call" });
    expect(openRadio).toHaveAttribute("aria-checked", "true");
    expect(rollCallRadio).toHaveAttribute("aria-checked", "false");

    await userEvent.click(rollCallRadio);
    expect(rollCallRadio).toHaveAttribute("aria-checked", "true");
    expect(openRadio).toHaveAttribute("aria-checked", "false");
  });

  it("defaults visibility to a checked Listed radio-card", async () => {
    renderAt("/nets/new", makeFetch({}));
    await screen.findByLabelText("Title");

    expect(screen.getByRole("radio", { name: "Listed" })).toHaveAttribute(
      "aria-checked",
      "true",
    );
    expect(screen.getByRole("radio", { name: "Unlisted" })).toHaveAttribute(
      "aria-checked",
      "false",
    );
  });

  it("gives BOTH visibility cards a non-empty accessible description", async () => {
    // The token-publishing consequence used to sit on the `unlisted` card
    // alone; it belongs on `listed` too, because the publishing happens the
    // moment `listed` is chosen. Neither placement had ANY test: `VISIBILITY_DESCRIPTIONS[value] ?? ""` means
    // deleting either entry renders an empty description span and every gate
    // stays green, so this was unguarded.
    //
    // The STRINGS are deliberately not asserted — pinning rendered prose
    // breaches the house testing contract, and both are expected to be
    // reworded. What is asserted is the required behaviour: each
    // card exposes a description to assistive tech at all.
    renderAt("/nets/new", makeFetch({}));
    await screen.findByLabelText("Title");

    for (const name of ["Listed", "Unlisted"]) {
      const radio = screen.getByRole("radio", { name });
      const descId = radio.getAttribute("aria-describedby");
      expect(descId).not.toBeNull();
      const desc = document.getElementById(descId ?? "");
      expect(desc).not.toBeNull();
      expect(desc?.textContent ?? "").toMatch(/\S/);
    }
  });

  it("live-updates the sticky summary panel as form fields change", async () => {
    renderAt("/nets/new", makeFetch({}));
    await userEvent.type(await screen.findByLabelText("Title"), "Test Net");
    await userEvent.type(
      within(screen.getByTestId("connection-row")).getByLabelText("Frequency (MHz)"),
      "14.230",
    );
    await userEvent.click(screen.getByRole("radio", { name: "Roll-call" }));

    const summary = screen.getByRole("region", { name: "Summary" });
    expect(summary).toHaveTextContent("14.230 MHz");
    expect(summary).toHaveTextContent("20m");
    expect(summary).toHaveTextContent("ssb");
    expect(summary).toHaveTextContent("Roll-call");
  });

  it("Cancel navigates back to the previous entry", async () => {
    const fetchMock = makeFetch({});
    vi.stubGlobal("fetch", fetchMock);
    const router = createMemoryRouter(
      [
        { path: "/nets", element: <div data-testid="nets-root" /> },
        { path: "/nets/new", element: <NetDefinitionFormPage /> },
      ],
      { initialEntries: ["/nets", "/nets/new"], initialIndex: 1 },
    );
    render(<RouterProvider router={router} />);
    await screen.findByLabelText("Title");

    await userEvent.click(screen.getByRole("button", { name: "Cancel" }));

    await waitFor(() => {
      expect(router.state.location.pathname).toBe("/nets");
    });
  });

  it("shows a You badge on the signed-in account's own owner chip only", async () => {
    const calls: Array<{ url: string; method: string; body: unknown }> = [];
    const twoOwners = {
      ...created,
      id: "abc",
      ownerAccountIds: ["me", "other"],
      owners: [
        { accountId: "me", callsign: "W1AW" },
        { accountId: "other", callsign: "K2XYZ" },
      ],
    };
    renderAt(
      "/nets/abc/edit",
      makeFetch({ calls, definition: twoOwners }),
    );

    const meChip = (await screen.findByText("W1AW")).closest("li");
    const otherChip = screen.getByText("K2XYZ").closest("li");
    expect(meChip).toHaveTextContent("You");
    expect(otherChip).not.toHaveTextContent("You");
  });

  it(
    "has no WCAG 2.1 AA violations on the edit-mode form",
    async () => {
      vi.stubGlobal(
        "fetch",
        makeFetch({ definition: { ...created, id: "abc" } }),
      );
      const router = createMemoryRouter(
        [{ path: "/nets/:id/edit", element: <NetDefinitionFormPage /> }],
        { initialEntries: ["/nets/abc/edit"] },
      );
      const { container } = render(<RouterProvider router={router} />);
      await screen.findByLabelText("Title");

      await expectNoAxeViolations(container);
    },
  );
});

// --- The connection editor ------------------------------------

/** The connection PUT body a save sent, or `undefined` if it sent none. */
function connectionsBody(
  calls: Array<{ url: string; method: string; body: unknown }>,
): { connections: Record<string, unknown>[]; expectedDefinitionVersion: number } | undefined {
  const call = calls.find(
    (c) => c.method === "PUT" && c.url.endsWith("/connections"),
  );
  return call?.body as
    | { connections: Record<string, unknown>[]; expectedDefinitionVersion: number }
    | undefined;
}

const rows = (): HTMLElement[] => screen.getAllByTestId("connection-row");

async function save(): Promise<void> {
  await userEvent.click(screen.getByRole("button", { name: /save changes/i }));
}

describe("NetDefinitionFormPage connection editor", () => {
  it("drops the previous kind's properties from the entry when the kind changes", async () => {
    const calls: Array<{ url: string; method: string; body: unknown }> = [];
    renderAt(
      "/nets/abc/edit",
      makeFetch({
        calls,
        definition: { ...created, id: "abc", connections: [hfConnection] },
      }),
    );
    await screen.findByTestId("connection-row");

    await userEvent.selectOptions(
      within(rows()[0]).getByLabelText("Connection type"),
      "dmr",
    );
    await userEvent.type(
      within(rows()[0]).getByLabelText("Talkgroup"),
      "3100",
    );
    await save();

    await waitFor(() => expect(connectionsBody(calls)).toBeDefined());
    const entry = connectionsBody(calls)!.connections[0];
    expect(Object.keys(entry).sort()).toEqual(["id", "kind", "talkgroup"]);
    expect(entry.talkgroup).toBe("3100");
  });

  it("reorders the list without changing any other key on any entry", async () => {
    const calls: Array<{ url: string; method: string; body: unknown }> = [];
    renderAt(
      "/nets/abc/edit",
      makeFetch({
        calls,
        definition: {
          ...created,
          id: "abc",
          connections: [hfConnection, repeaterConnection],
        },
      }),
    );
    await screen.findAllByTestId("connection-row");

    await userEvent.click(
      within(rows()[1]).getByRole("button", { name: /move up/i }),
    );
    await save();

    await waitFor(() => expect(connectionsBody(calls)).toBeDefined());
    const sent = connectionsBody(calls)!.connections;
    expect(sent.map((e) => e.id)).toEqual(["conn-rpt", "conn-hf"]);
    // Every entry is otherwise byte-identical to what a straight echo would be.
    expect(sent[0]).toEqual({
      id: "conn-rpt",
      kind: "repeater",
      plannedFrequencyHz: 146_940_000,
      band: "2m",
      mode: "fm",
      repeaterOffsetHz: -600_000,
      toneMode: "ctcss",
      toneValue: "100.0",
    });
    expect(sent[1]).toEqual({
      id: "conn-hf",
      kind: "hf",
      plannedFrequencyHz: 14_230_000,
      band: "20m",
      mode: "ssb",
    });
  });

  it("names the export connection on whichever row is first, and follows a reorder", async () => {
    renderAt(
      "/nets/abc/edit",
      makeFetch({
        definition: {
          ...created,
          id: "abc",
          connections: [hfConnection, repeaterConnection],
        },
      }),
    );
    await screen.findAllByTestId("connection-row");

    expect(
      within(rows()[0]).queryByTestId("adif-export-note"),
    ).toBeInTheDocument();
    expect(
      within(rows()[1]).queryByTestId("adif-export-note"),
    ).not.toBeInTheDocument();

    await userEvent.click(
      within(rows()[1]).getByRole("button", { name: /move up/i }),
    );

    expect(
      within(rows()[0]).getByLabelText("Connection type"),
    ).toHaveValue("repeater");
    expect(
      within(rows()[0]).queryByTestId("adif-export-note"),
    ).toBeInTheDocument();
  });

  it("refuses to remove the only remaining connection without asking the server", async () => {
    const calls: Array<{ url: string; method: string; body: unknown }> = [];
    renderAt(
      "/nets/abc/edit",
      makeFetch({
        calls,
        definition: { ...created, id: "abc", connections: [hfConnection] },
      }),
    );
    await screen.findByTestId("connection-row");

    const remove = within(rows()[0]).getByRole("button", { name: /remove/i });
    expect(remove).toBeDisabled();
    await userEvent.click(remove);

    expect(rows()).toHaveLength(1);
    expect(
      calls.filter((c) => c.method === "PUT" && c.url.endsWith("/connections")),
    ).toHaveLength(0);
  });

  it("removes a connection when another remains, and the save carries the shorter list", async () => {
    const calls: Array<{ url: string; method: string; body: unknown }> = [];
    renderAt(
      "/nets/abc/edit",
      makeFetch({
        calls,
        definition: {
          ...created,
          id: "abc",
          connections: [hfConnection, repeaterConnection],
        },
      }),
    );
    await screen.findAllByTestId("connection-row");

    await userEvent.click(
      within(rows()[1]).getByRole("button", { name: /remove/i }),
    );
    expect(rows()).toHaveLength(1);
    await save();

    await waitFor(() => expect(connectionsBody(calls)).toBeDefined());
    expect(
      calls.filter((c) => c.method === "PUT" && c.url.endsWith("/connections")),
    ).toHaveLength(1);
    expect(connectionsBody(calls)!.connections.map((e) => e.id)).toEqual([
      "conn-hf",
    ]);
  });

  it("puts a per-entry refusal against the row the server named, not at the form head", async () => {
    renderAt(
      "/nets/abc/edit",
      makeFetch({
        definition: {
          ...created,
          id: "abc",
          connections: [hfConnection, repeaterConnection],
        },
        connectionResponses: [
          {
            status: 400,
            problem: {
              type: "/errors/net-definition-invalid",
              status: 400,
              detail: "connection 1: a `repeater` connection's `frequency` is required",
              connectionIndex: 1,
            },
          },
        ],
      }),
    );
    await screen.findAllByTestId("connection-row");
    await save();

    await waitFor(() =>
      expect(within(rows()[1]).getByRole("alert")).toBeInTheDocument(),
    );
    expect(within(rows()[0]).queryByRole("alert")).not.toBeInTheDocument();
    // The action stack's alert stays clear. Asserted as "exactly one alert on
    // the whole page, and it is inside row 2" — a containment check against the
    // Summary rail's card cannot fail, because the action stack is that card's
    // SIBLING and never its descendant.
    const alerts = screen.getAllByRole("alert");
    expect(alerts).toHaveLength(1);
    expect(rows()[1]).toContainElement(alerts[0]);
    expect(screen.queryByTestId("save-error")).not.toBeInTheDocument();
  });

  it("puts a per-entry refusal against the row the server named on CREATE too, not at the form head", async () => {
    // `POST /api/net-definitions` refuses a
    // connection through the same `connectionIndex`-bearing problem the
    // sub-resource sends, so the create page owes the row the same treatment
    // the edit page gives it.
    renderAt(
      "/nets/new",
      makeFetch({
        mutateStatus: 400,
        mutateBody: {
          type: "/errors/net-definition-invalid",
          status: 400,
          detail: "connection 0: a `hf` connection's `frequency` is required",
          connectionIndex: 0,
        },
      }),
    );
    await userEvent.type(await screen.findByLabelText("Title"), "T");
    await userEvent.type(
      within(screen.getByTestId("connection-row")).getByLabelText("Frequency (MHz)"),
      "14.230",
    );
    await userEvent.click(screen.getByRole("button", { name: /create net/i }));

    await waitFor(() =>
      expect(within(rows()[0]).getByRole("alert")).toBeInTheDocument(),
    );
    const alerts = screen.getAllByRole("alert");
    expect(alerts).toHaveLength(1);
    expect(rows()[0]).toContainElement(alerts[0]);
    expect(screen.queryByTestId("save-error")).not.toBeInTheDocument();
  });

  it("offers exactly the eight named kinds, and reaches `other` by a separate control", async () => {
    renderAt(
      "/nets/abc/edit",
      makeFetch({
        definition: { ...created, id: "abc", connections: [hfConnection] },
      }),
    );
    await screen.findByTestId("connection-row");

    const picker = within(rows()[0]).getByLabelText(
      "Connection type",
    ) as HTMLSelectElement;
    expect([...picker.options].map((o) => o.value)).toEqual([
      "hf",
      "repeater",
      "echolink",
      "allstar",
      "dmr",
      "dstar",
      "ysf",
      "urf",
    ]);

    await userEvent.click(
      screen.getByRole("button", { name: /something the list/i }),
    );
    expect(rows()).toHaveLength(2);
    expect(
      within(rows()[1]).queryByLabelText("Connection type"),
    ).not.toBeInTheDocument();
    expect(within(rows()[1]).getByLabelText("Label")).toBeInTheDocument();
  });

  it("echoes a machine-minted label verbatim and never binds it to an editable control", async () => {
    const calls: Array<{ url: string; method: string; body: unknown }> = [];
    renderAt(
      "/nets/abc/edit",
      makeFetch({
        calls,
        definition: {
          ...created,
          id: "abc",
          connections: [hfConnection, residueConnection],
        },
      }),
    );
    await screen.findAllByTestId("connection-row");

    expect(
      within(rows()[1]).getByTestId("unclassified-residue"),
    ).toBeInTheDocument();
    expect(within(rows()[1]).queryByLabelText("Label")).not.toBeInTheDocument();

    await save();
    await waitFor(() => expect(connectionsBody(calls)).toBeDefined());
    expect(connectionsBody(calls)!.connections[1]).toEqual({
      id: "conn-res",
      kind: "other",
      label: "unclassified-reflector",
      detail: "REF030 C",
    });
  });

  it("reclassifies unclassified residue into a reflector kind, carrying the detail across", async () => {
    const calls: Array<{ url: string; method: string; body: unknown }> = [];
    renderAt(
      "/nets/abc/edit",
      makeFetch({
        calls,
        definition: {
          ...created,
          id: "abc",
          connections: [hfConnection, residueConnection],
        },
      }),
    );
    await screen.findAllByTestId("connection-row");

    await userEvent.selectOptions(
      within(rows()[1]).getByLabelText("Reclassify as"),
      "dstar",
    );
    await save();

    await waitFor(() => expect(connectionsBody(calls)).toBeDefined());
    expect(connectionsBody(calls)!.connections[1]).toEqual({
      id: "conn-res",
      kind: "dstar",
      reflector: "REF030 C",
    });
  });

  it("refuses a label an owner typed into the machine-only namespace, before any request", async () => {
    const calls: Array<{ url: string; method: string; body: unknown }> = [];
    renderAt(
      "/nets/abc/edit",
      makeFetch({
        calls,
        definition: { ...created, id: "abc", connections: [hfConnection] },
      }),
    );
    await screen.findByTestId("connection-row");

    await userEvent.click(
      screen.getByRole("button", { name: /something the list/i }),
    );
    await userEvent.type(
      within(rows()[1]).getByLabelText("Label"),
      "Unclassified Reflector",
    );
    await save();

    await waitFor(() =>
      expect(within(rows()[1]).getByRole("alert")).toBeInTheDocument(),
    );
    expect(calls.filter((c) => c.method === "PUT")).toHaveLength(0);
  });

  it("sends the connection list on the version the page loaded, and a scalar write that names no connection fact", async () => {
    // This test used to pin the older
    // two-write ORDER: scalar first, connection list last, on the version the
    // scalar minted — because the scalar request could not say "no RF way" and
    // re-minted a connection the list write then had to delete. The scalar
    // request no longer carries any connection fact, so nothing needs cleaning
    // up after it, and the connection write's compare-and-swap can be aimed at
    // the version the PAGE LOADED — which is the only version that catches a
    // co-owner's edit made while this page was open.
    const calls: Array<{ url: string; method: string; body: unknown }> = [];
    renderAt(
      "/nets/abc/edit",
      makeFetch({
        calls,
        definition: {
          ...created,
          id: "abc",
          definitionVersion: 4,
          connections: [echolinkConnection],
        },
        connectionResponses: [
          {
            body: {
              ...created,
              id: "abc",
              definitionVersion: 5,
              connections: [echolinkConnection],
            },
          },
        ],
        scalarResponses: [
          { ...created, id: "abc", definitionVersion: 6, connections: [echolinkConnection] },
        ],
      }),
    );
    await screen.findByTestId("connection-row");
    await userEvent.type(await screen.findByLabelText("Title"), "!");
    await save();

    await waitFor(() => expect(connectionsBody(calls)).toBeDefined());
    expect(connectionsBody(calls)!.expectedDefinitionVersion).toBe(4);
    expect(connectionsBody(calls)!.connections).toEqual([
      { id: "conn-el", kind: "echolink", node: "12345" },
    ]);
    const scalar = await waitFor(() => {
      const put = calls.find(
        (c) => c.method === "PUT" && c.url === "/api/net-definitions/abc",
      );
      expect(put).toBeDefined();
      return put!.body as Record<string, unknown>;
    });
    for (const key of [
      "plannedFrequency",
      "band",
      "mode",
      "repeaterOffset",
      "toneMode",
      "toneValue",
      "echolinkNode",
      "reflector",
      "allstarNode",
      "connections",
    ]) {
      expect(scalar).not.toHaveProperty(key);
    }
  });

  it("recovers from a conflict: it re-reads, keeps the edits, and the next save succeeds", async () => {
    const calls: Array<{ url: string; method: string; body: unknown }> = [];
    renderAt(
      "/nets/abc/edit",
      makeFetch({
        calls,
        definition: {
          ...created,
          id: "abc",
          definitionVersion: 7,
          connections: [hfConnection],
        },
        connectionResponses: [
          {
            status: 409,
            problem: { type: "/errors/stale-version", status: 409 },
          },
          {
            body: {
              ...created,
              id: "abc",
              definitionVersion: 8,
              connections: [hfConnection],
            },
          },
        ],
        scalarResponses: [
          { ...created, id: "abc", definitionVersion: 9, connections: [hfConnection] },
        ],
      }),
    );
    await screen.findByTestId("connection-row");
    const frequency = within(rows()[0]).getByLabelText(
      "Frequency (MHz)",
    ) as HTMLInputElement;
    await userEvent.clear(frequency);
    await userEvent.type(frequency, "14.240");

    await save();
    await waitFor(() =>
      expect(
        calls.filter((c) => c.url.endsWith("/connections")),
      ).toHaveLength(1),
    );
    // The conflict re-reads the definition rather than leaving state stale.
    await waitFor(() =>
      expect(
        calls.filter(
          (c) => c.method === "GET" && c.url === "/api/net-definitions/abc",
        ).length,
      ).toBeGreaterThan(1),
    );
    expect(
      (within(rows()[0]).getByLabelText("Frequency (MHz)") as HTMLInputElement)
        .value,
    ).toBe("14.240");

    await save();
    await waitFor(() =>
      expect(
        calls.filter((c) => c.url.endsWith("/connections")),
      ).toHaveLength(2),
    );
    // The retry carries the version the re-read returned — the page never
    // guesses past what the server told it — and the scalar write lands after
    // the list, so its response is the one on screen.
    const second = calls.filter((c) => c.url.endsWith("/connections"))[1]
      .body as { expectedDefinitionVersion: number };
    expect(second.expectedDefinitionVersion).toBe(7);
    await waitFor(() =>
      expect(screen.getByText(/version 9/i)).toBeInTheDocument(),
    );
  });

  it("offers the connection editor on create, seeded with one way in, and no flat operating input anywhere", async () => {
    // Create used to be the ONE place the
    // flat frequency/band/mode/repeater inputs survived, because the create
    // request required them. It now carries a connection list like the edit
    // page does, so a net is born with the ways in its owner declared rather
    // than with one re-minted from a shape that could name only four.
    renderAt("/nets/new", makeFetch({}));
    expect(await screen.findByTestId("connection-list-editor")).toBeInTheDocument();
    expect(rows()).toHaveLength(1);
    expect(screen.queryByLabelText("Planned frequency (MHz)")).not.toBeInTheDocument();
    expect(screen.queryByLabelText("Repeater offset (MHz)")).not.toBeInTheDocument();
    expect(screen.queryByLabelText("EchoLink node")).not.toBeInTheDocument();
  });

  it("drops the flat operating inputs in edit mode, where the facts are per connection", async () => {
    renderAt(
      "/nets/abc/edit",
      makeFetch({
        definition: { ...created, id: "abc", connections: [hfConnection] },
      }),
    );
    expect(await screen.findByTestId("connection-list-editor")).toBeInTheDocument();
    expect(
      screen.queryByLabelText("Planned frequency (MHz)"),
    ).not.toBeInTheDocument();
    expect(
      screen.queryByLabelText("Repeater offset (MHz)"),
    ).not.toBeInTheDocument();
    expect(screen.queryByLabelText("EchoLink node")).not.toBeInTheDocument();
  });
});

// --- A DMR connection names its network ------------------------

const dmrConnection = connection({
  id: "conn-dmr",
  position: 0,
  kind: "dmr",
  talkgroup: "3100",
  network: "Brandmeister",
});

const networklessDmrConnection = connection({
  id: "conn-dmr-bare",
  position: 0,
  kind: "dmr",
  talkgroup: "3100",
});

/** The `<datalist>` an input's `list` attribute points at, or null. */
function suggestionListOf(input: HTMLElement): HTMLDataListElement | null {
  const id = input.getAttribute("list");
  return id === null
    ? null
    : (document.getElementById(id) as HTMLDataListElement | null);
}

describe("NetDefinitionFormPage connection editor — the DMR network", () => {
  it("opens a stored network in its own control and ships it back on an untouched save", async () => {
    const calls: Array<{ url: string; method: string; body: unknown }> = [];
    renderAt(
      "/nets/abc/edit",
      makeFetch({
        calls,
        definition: { ...created, id: "abc", connections: [dmrConnection] },
      }),
    );
    await screen.findByTestId("connection-row");

    expect(
      (within(rows()[0]).getByLabelText("Network") as HTMLInputElement).value,
    ).toBe("Brandmeister");

    await save();
    await waitFor(() => expect(connectionsBody(calls)).toBeDefined());
    const entry = connectionsBody(calls)!.connections[0];
    expect(entry.network).toBe("Brandmeister");
    expect(entry.talkgroup).toBe("3100");
  });

  it("offers the common networks as suggestions and still accepts one that is not on the list", async () => {
    const calls: Array<{ url: string; method: string; body: unknown }> = [];
    renderAt(
      "/nets/abc/edit",
      makeFetch({
        calls,
        definition: {
          ...created,
          id: "abc",
          connections: [networklessDmrConnection],
        },
      }),
    );
    await screen.findByTestId("connection-row");

    const input = within(rows()[0]).getByLabelText("Network");
    const list = suggestionListOf(input);
    expect(list).not.toBeNull();
    expect(
      [...list!.querySelectorAll("option")].map((option) => option.value),
    ).toEqual(["Brandmeister", "TGIF", "FreeDMR"]);

    // Suggestions, not the permitted set: a private Hytera XPT system has a
    // name no list will ever carry.
    await userEvent.type(input, "Hytera-XPT-Local");
    await save();
    await waitFor(() => expect(connectionsBody(calls)).toBeDefined());
    expect(connectionsBody(calls)!.connections[0].network).toBe(
      "Hytera-XPT-Local",
    );
  });

  it("never lets two DMR rows collide on a suggestion-list id", async () => {
    renderAt(
      "/nets/abc/edit",
      makeFetch({
        definition: {
          ...created,
          id: "abc",
          connections: [
            dmrConnection,
            connection({
              id: "conn-dmr-2",
              position: 1,
              kind: "dmr",
              talkgroup: "31000",
              network: "TGIF",
            }),
          ],
        },
      }),
    );
    await screen.findAllByTestId("connection-row");

    const ids = rows().map((row) =>
      within(row).getByLabelText("Network").getAttribute("list"),
    );
    expect(ids.every((id) => id !== null)).toBe(true);
    // A duplicate DOM id makes every row after the first resolve its `list` to
    // the FIRST row's element. axe-core has no active `duplicate-id` rule for
    // non-ARIA ids, so nothing else in this suite would notice.
    for (const id of ids) {
      expect(document.querySelectorAll(`#${CSS.escape(id!)}`)).toHaveLength(1);
    }
  });

  it("saves a DMR connection whose network nobody recorded, with no per-row refusal", async () => {
    const calls: Array<{ url: string; method: string; body: unknown }> = [];
    renderAt(
      "/nets/abc/edit",
      makeFetch({
        calls,
        definition: {
          ...created,
          id: "abc",
          connections: [networklessDmrConnection],
        },
      }),
    );
    await screen.findByTestId("connection-row");

    await save();
    await waitFor(() => expect(connectionsBody(calls)).toBeDefined());
    expect(
      calls.filter((call) => call.url.includes("/connections")),
    ).toHaveLength(1);
    expect(screen.queryByRole("alert")).not.toBeInTheDocument();
    const entry = connectionsBody(calls)!.connections[0];
    expect(Object.keys(entry).sort()).toEqual(["id", "kind", "talkgroup"]);
  });
});

// --- connection editor: concurrency and lifecycle -------------------------

describe("NetDefinitionFormPage connection editor — concurrency and lifecycle", () => {
  it("refuses a save the server calls stale, and never deletes the connection someone else added", async () => {
    // This test used to pin a read-before-write guard: the page re-read the definition before saving because the
    // list write's compare-and-swap was aimed at the version the SCALAR write
    // had just minted, and so could never fail. The guard went with its
    // reason — the list write now carries the version the page LOADED, so the
    // server's compare-and-swap is what refuses a stale save, and this test
    // drives the refusal the way the server raises it.
    const calls: Array<{ url: string; method: string; body: unknown }> = [];
    const atLoad = {
      ...created,
      id: "abc",
      definitionVersion: 3,
      connections: [hfConnection],
    };
    // A co-owner added an EchoLink node and bumped the version while this page
    // sat open. Every read after the first sees their version.
    const afterCoOwner = {
      ...created,
      id: "abc",
      definitionVersion: 4,
      connections: [hfConnection, echolinkConnection],
    };
    renderAt(
      "/nets/abc/edit",
      makeFetch({
        calls,
        definitionResponses: [atLoad, afterCoOwner],
        connectionResponses: [
          { status: 409, problem: { type: "/errors/stale-version", status: 409 } },
        ],
      }),
    );
    await screen.findByTestId("connection-row");
    await userEvent.type(await screen.findByLabelText("Title"), "!");

    await save();

    await waitFor(() =>
      expect(screen.getAllByRole("alert").length).toBeGreaterThan(0),
    );
    // The list write carried the LOADED version — which is what let the server
    // refuse it — and NOTHING landed: the scalar write never follows a refused
    // list, so the net is exactly as the co-owner left it.
    expect(connectionsBody(calls)!.expectedDefinitionVersion).toBe(3);
    expect(
      calls.filter((c) => c.method === "PUT" && c.url === "/api/net-definitions/abc"),
    ).toHaveLength(0);

    // And the co-owner's connection is now in the form, so the next save
    // carries it — on their version — rather than replacing the list without it.
    await waitFor(() => expect(rows()).toHaveLength(2));
    await save();
    await waitFor(() =>
      expect(calls.filter((c) => c.url.endsWith("/connections"))).toHaveLength(2),
    );
    const second = calls.filter((c) => c.url.endsWith("/connections"))[1].body as {
      connections: Array<{ id?: string }>;
      expectedDefinitionVersion: number;
    };
    expect(second.connections.map((e) => e.id)).toEqual(["conn-hf", "conn-el"]);
    expect(second.expectedDefinitionVersion).toBe(4);
  });

  it("drops the previous net's connections when the route id changes, and refuses to save until the new ones arrive", async () => {
    const netA = { ...created, id: "a", connections: [hfConnection] };
    const router = renderAt(
      "/nets/a/edit",
      makeFetch({
        definitionsById: { a: netA },
        // Net B's definition never resolves, so the window this test is about
        // stays open for the length of the assertion.
        definitionPendingIds: ["b"],
      }),
    );
    await screen.findByTestId("connection-row");
    expect(rows()).toHaveLength(1);

    await router.navigate("/nets/b/edit");

    // The edit route has no `key`, so React reuses this component instance
    // across an id change. Net A's rows — with net A's connection ids — must
    // not survive into net B, whose connections PUT is a full replace.
    await waitFor(() =>
      expect(screen.queryAllByTestId("connection-row")).toHaveLength(0),
    );
    expect(screen.getByRole("button", { name: /save changes/i })).toBeDisabled();
  });

  it("cannot save a net whose definition never loaded", async () => {
    renderAt("/nets/abc/edit", makeFetch({ definitionStatus: 500 }));

    await screen.findByRole("alert");
    expect(screen.getByRole("button", { name: /save changes/i })).toBeDisabled();
  });

  it("keeps a lost race as the conflict it is when the re-read also fails", async () => {
    renderAt(
      "/nets/abc/edit",
      makeFetch({
        definition: { ...created, id: "abc", connections: [hfConnection] },
        // The load succeeds; the 409's re-read does not. (A pre-write read
        // once sat between the two, so this used to be 2.)
        definitionFailAfter: 1,
        connectionResponses: [
          { status: 409, problem: { type: "/errors/stale-version", status: 409 } },
        ],
      }),
    );
    await screen.findByTestId("connection-row");
    await save();

    await waitFor(() =>
      expect(screen.getByTestId("save-error")).toHaveTextContent(
        messageForProblemType("/errors/stale-version"),
      ),
    );
  });

  it("sends no scalar write when the connection list is refused, so nothing half lands", async () => {
    // This test used to pin the partial-save note: the scalar write went FIRST, so a refused list left
    // the owner with their details saved and their ways in not, and the page
    // had to say so. The list now goes first and the scalar write only follows
    // a list that landed — a refusal leaves the net exactly as it was, and
    // there is no half-landed state to report.
    const calls: Array<{ url: string; method: string; body: unknown }> = [];
    renderAt(
      "/nets/abc/edit",
      makeFetch({
        calls,
        definition: { ...created, id: "abc", connections: [hfConnection] },
        connectionResponses: [
          {
            status: 400,
            problem: {
              type: "/errors/net-definition-invalid",
              status: 400,
              detail: "the list is not acceptable",
            },
          },
        ],
      }),
    );
    await screen.findByTestId("connection-row");

    await save();

    await waitFor(() =>
      expect(screen.getByTestId("save-error")).toHaveTextContent(
        "the list is not acceptable",
      ),
    );
    expect(
      calls.filter((c) => c.method === "PUT" && c.url === "/api/net-definitions/abc"),
    ).toHaveLength(0);
    expect(screen.queryByTestId("partial-save-note")).not.toBeInTheDocument();
  });
});

describe("NetDefinitionFormPage connection editor — the rail, the picker and the list", () => {
  it("never publishes the flat mirror in the Summary rail for a net with no RF connection", async () => {
    renderAt(
      "/nets/abc/edit",
      makeFetch({
        definition: {
          ...created,
          id: "abc",
          // Reached only over the internet: nothing on the definition carries a
          // frequency, band or mode any more, so the only way a
          // number could appear in the rail is by being invented.
          connections: [echolinkConnection],
        },
      }),
    );
    await screen.findByTestId("connection-row");

    const summary = screen.getByRole("region", { name: "Summary" });
    expect(within(summary).queryByText(/14\.23/)).not.toBeInTheDocument();
    expect(within(summary).queryByText(/20m/)).not.toBeInTheDocument();
    expect(within(summary).queryByText(/ssb/)).not.toBeInTheDocument();
  });

  it("still summarises the RF connection's own frequency when the net has one", async () => {
    renderAt(
      "/nets/abc/edit",
      makeFetch({
        definition: {
          ...created,
          id: "abc",
          connections: [echolinkConnection, repeaterConnection],
        },
      }),
    );
    await screen.findAllByTestId("connection-row");

    const summary = screen.getByRole("region", { name: "Summary" });
    expect(within(summary).getByText(/146\.94/)).toBeInTheDocument();
    expect(within(summary).getByText(/2m/)).toBeInTheDocument();
  });

  it("names the kinds in the picker instead of showing their wire tokens", async () => {
    renderAt(
      "/nets/abc/edit",
      makeFetch({
        definition: { ...created, id: "abc", connections: [hfConnection] },
      }),
    );
    await screen.findByTestId("connection-row");

    const picker = within(rows()[0]).getByLabelText(
      "Connection type",
    ) as HTMLSelectElement;
    // The values are the wire tokens and stay so; the text is what the
    // owner reads, and no owner knows what `urf` is.
    expect([...picker.options].map((o) => o.textContent)).toEqual([
      "HF",
      "Repeater",
      "EchoLink",
      "AllStar",
      "DMR",
      "D-Star",
      "System Fusion",
      "URF",
    ]);
  });

  it("refuses a thirty-third connection before asking the server", async () => {
    const full = Array.from({ length: 32 }, (_, i) =>
      connection({ id: `conn-${i}`, position: i, kind: "echolink", node: `${1000 + i}` }),
    );
    renderAt(
      "/nets/abc/edit",
      makeFetch({ definition: { ...created, id: "abc", connections: full } }),
    );
    await screen.findAllByTestId("connection-row");
    expect(rows()).toHaveLength(32);

    expect(
      screen.getByRole("button", { name: /add connection/i }),
    ).toBeDisabled();
    expect(
      screen.getByRole("button", { name: /something the list/i }),
    ).toBeDisabled();
  });

  it("lets a reclassified `other` go back to being unclassified", async () => {
    renderAt(
      "/nets/abc/edit",
      makeFetch({
        definition: {
          ...created,
          id: "abc",
          connections: [hfConnection, residueConnection],
        },
      }),
    );
    await screen.findAllByTestId("connection-row");

    await userEvent.selectOptions(
      within(rows()[1]).getByLabelText("Reclassify as"),
      "dstar",
    );
    expect(within(rows()[1]).getByLabelText("Reflector")).toHaveValue("REF030 C");

    // The door swings both ways: an owner who picked the wrong network can
    // undo it without reloading the page and losing every other edit.
    await userEvent.selectOptions(
      within(rows()[1]).getByLabelText("Connection type"),
      "other",
    );
    expect(within(rows()[1]).getByTestId("unclassified-residue")).toBeInTheDocument();
  });

  it("clears a row refusal when the owner changes the list, so it cannot re-attach to another connection", async () => {
    const calls: Array<{ url: string; method: string; body: unknown }> = [];
    renderAt(
      "/nets/abc/edit",
      makeFetch({
        calls,
        definition: {
          ...created,
          id: "abc",
          connections: [hfConnection, repeaterConnection],
        },
        connectionResponses: [
          {
            status: 400,
            problem: {
              type: "/errors/net-definition-invalid",
              status: 400,
              detail: "connection 1: a `repeater` connection's `frequency` is required",
              connectionIndex: 1,
            },
          },
        ],
      }),
    );
    await screen.findAllByTestId("connection-row");
    await save();
    await waitFor(() =>
      expect(within(rows()[1]).getByRole("alert")).toBeInTheDocument(),
    );

    // The refusal was attributed to a POSITION. Reorder and that position now
    // holds a different connection, so the message must go rather than follow.
    await userEvent.click(
      within(rows()[1]).getByRole("button", { name: /move up/i }),
    );
    expect(screen.queryAllByRole("alert")).toHaveLength(0);
  });

  it("builds a System Fusion, a URF and an AllStar connection through the editor", async () => {
    const calls: Array<{ url: string; method: string; body: unknown }> = [];
    renderAt(
      "/nets/abc/edit",
      makeFetch({
        calls,
        definition: { ...created, id: "abc", connections: [hfConnection] },
      }),
    );
    await screen.findByTestId("connection-row");

    for (const [kind, label, value] of [
      ["ysf", "Reflector", "US-Ohio"],
      ["urf", "Reflector", "URF001 B"],
      ["allstar", "Node", "56789"],
    ] as const) {
      await userEvent.click(
        screen.getByRole("button", { name: /add connection/i }),
      );
      const row = rows()[rows().length - 1];
      await userEvent.selectOptions(
        within(row).getByLabelText("Connection type"),
        kind,
      );
      await userEvent.type(within(row).getByLabelText(label), value);
    }
    await save();

    await waitFor(() => expect(connectionsBody(calls)).toBeDefined());
    expect(connectionsBody(calls)!.connections.slice(1)).toEqual([
      { kind: "ysf", reflector: "US-Ohio" },
      { kind: "urf", reflector: "URF001 B" },
      { kind: "allstar", node: "56789" },
    ]);
  });
});

describe("NetDefinitionFormPage connection editor — assistive technology", () => {
  it("gives every row its own named group, so three frequency inputs are three different controls", async () => {
    renderAt(
      "/nets/abc/edit",
      makeFetch({
        definition: {
          ...created,
          id: "abc",
          connections: [hfConnection, repeaterConnection, echolinkConnection],
        },
      }),
    );
    await screen.findAllByTestId("connection-row");

    const groups = screen
      .getAllByRole("group")
      .filter((g) => g.dataset.testid === "connection-row");
    expect(groups).toHaveLength(3);
    const names = groups.map((g) => g.querySelector("legend")?.textContent);
    expect(new Set(names).size).toBe(3);
  });

  it("wires a row refusal to the control it names", async () => {
    renderAt(
      "/nets/abc/edit",
      makeFetch({
        definition: {
          ...created,
          id: "abc",
          connections: [connection({ id: "conn-blank", kind: "hf", band: "20m", mode: "ssb" })],
        },
      }),
    );
    await screen.findByTestId("connection-row");
    // An HF row with no frequency: the pre-write gate refuses it and knows
    // WHICH property is missing.
    await save();

    const alert = await within(rows()[0]).findByRole("alert");
    const input = within(rows()[0]).getByLabelText("Frequency (MHz)");
    expect(input).toHaveAttribute("aria-invalid", "true");
    expect(input.getAttribute("aria-describedby")).toBe(alert.id);
    expect(alert.id).not.toBe("");
  });

  it("announces a reorder and puts focus on the row that moved", async () => {
    vi.stubGlobal(
      "fetch",
      makeFetch({
        definition: {
          ...created,
          id: "abc",
          connections: [hfConnection, repeaterConnection],
        },
      }),
    );
    const router = createMemoryRouter(
      [{ path: "/nets/:id/edit", element: <NetDefinitionFormPage /> }],
      { initialEntries: ["/nets/abc/edit"] },
    );
    render(
      <>
        <LiveRegion />
        <RouterProvider router={router} />
      </>,
    );
    await screen.findAllByTestId("connection-row");
    // The announcer is a module singleton, so an earlier test's message is
    // still in it; clear it rather than reading a stale one as this one.
    act(() => announce(""));
    expect(screen.getByRole("status")).toBeEmptyDOMElement();

    await userEvent.click(
      within(rows()[1]).getByRole("button", { name: /move up/i }),
    );

    // The operation destroys the focused button (Move up disables itself at
    // position 1), so focus must be placed deliberately rather than dropped
    // to the body.
    expect(rows()[0]).toHaveFocus();
    expect(screen.getByRole("status").textContent).toMatch(/1.*2/);
  });

  it("has no WCAG 2.1 AA violations with a multi-row connection list", async () => {
    vi.stubGlobal(
      "fetch",
      makeFetch({
        definition: {
          ...created,
          id: "abc",
          connections: [hfConnection, repeaterConnection, echolinkConnection],
        },
      }),
    );
    const router = createMemoryRouter(
      [{ path: "/nets/:id/edit", element: <NetDefinitionFormPage /> }],
      { initialEntries: ["/nets/abc/edit"] },
    );
    const { container } = render(<RouterProvider router={router} />);
    await screen.findAllByTestId("connection-row");

    await expectNoAxeViolations(container);
  });
});
