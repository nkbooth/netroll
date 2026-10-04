// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
import { act, render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";

import { QuickAddRow } from "./QuickAddRow";
import { MAX_VIA_CHARS } from "./ViaPicker";
import { createSessionStore, selectRoster } from "./sessionStore";
import type { SessionSummaryBody } from "./sessionWire";
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


/**
 * Quick-add row tests. Behavior-first: they assert
 * store pending/rollback state, focus targets, the POSTed clientEventId, the
 * bounded-timeout rollback (fake clock), and the `n` hotkey guard — never copy.
 */

const uuid = (n: number): string =>
  `00000000-0000-0000-0000-${n.toString(16).padStart(12, "0")}`;

function summary(overrides: Partial<SessionSummaryBody> = {}): SessionSummaryBody {
  return {
    id: uuid(1),
    definitionId: uuid(7),
    definitionVersion: 3,
    lifecycle: "live",
    connections: [hfWay(14_250_000)],
    startedAt: "2026-07-16T00:00:00Z",
    closedAt: null,
    durationSeconds: null,
    latestSeq: 1,
    participantCount: 0,
    workingCheckInId: null,
    netNote: null,
    controlState: "active",
    roster: [],
    definition: {
      title: "Sunday Traffic Net",
      connections: [hfWay()],
      netCategory: "traffic",
      netType: "open",
    },
    ...overrides,
  };
}

function jsonResponse(status: number, body: unknown): Response {
  return new Response(JSON.stringify(body), {
    status,
    headers: {
      "content-type": status >= 400 ? "application/problem+json" : "application/json",
    },
  });
}

/** A promise whose resolution the test drives, to hold a send mid-flight. */
function deferred<T>() {
  let resolve!: (value: T) => void;
  let reject!: (reason: unknown) => void;
  const promise = new Promise<T>((res, rej) => {
    resolve = res;
    reject = rej;
  });
  return { promise, resolve, reject };
}

function seededStore(overrides: Partial<SessionSummaryBody> = {}) {
  const store = createSessionStore();
  store.getState().seedFromSnapshot(summary(overrides));
  return store;
}

/** A controllable fake clock: the test fires the armed timeout by hand. */
function fakeClock() {
  const armed: Array<() => void> = [];
  const scheduleTimeout = (fn: () => void): (() => void) => {
    armed.push(fn);
    return () => {
      const i = armed.indexOf(fn);
      if (i >= 0) armed.splice(i, 1);
    };
  };
  return { scheduleTimeout, fireAll: () => armed.slice().forEach((fn) => fn()), armed };
}

const setWidth = (width: number): void => {
  Object.defineProperty(window, "innerWidth", {
    configurable: true,
    writable: true,
    value: width,
  });
};

afterEach(() => {
  vi.unstubAllGlobals();
  setWidth(1024);
});

describe("QuickAddRow", () => {
  it("mints a clientEventId, adds a dimmed pending entry, and POSTs the callsign with that id", async () => {
    const user = userEvent.setup();
    const store = seededStore();
    const clock = fakeClock();
    const pending = deferred<Response>();
    const fetchMock = vi.fn().mockReturnValue(pending.promise);
    vi.stubGlobal("fetch", fetchMock);

    render(
      <QuickAddRow
        sessionId="abc"
        store={store}
        scheduleTimeout={clock.scheduleTimeout}
        mintClientEventId={() => uuid(0xc1)}
      />,
    );
    await user.type(screen.getByLabelText(/callsign/i), "K2ABC");
    await user.keyboard("{Enter}");

    // The optimistic pending entry rendered before the send resolved.
    await waitFor(() => expect(store.getState().pending.size).toBe(1));
    const entry = [...store.getState().pending.values()][0];
    expect(entry).toMatchObject({ clientEventId: uuid(0xc1), callsign: "K2ABC" });

    const [url, init] = fetchMock.mock.calls[0] as [string, RequestInit];
    expect(url).toBe("/api/net-sessions/abc/check-ins");
    expect(init.method).toBe("POST");
    expect(JSON.parse(init.body as string)).toEqual({
      callsign: "K2ABC",
      clientEventId: uuid(0xc1),
      staying: "staying-for-comments",
    });

    pending.resolve(jsonResponse(201, summary({ latestSeq: 2 })));
  });

  it("clears and refocuses the callsign field on a successful Enter (the commit-clear-refocus loop)", async () => {
    const user = userEvent.setup();
    const store = seededStore();
    const clock = fakeClock();
    const fetchMock = vi.fn().mockResolvedValue(jsonResponse(201, summary({ latestSeq: 2 })));
    vi.stubGlobal("fetch", fetchMock);

    render(
      <QuickAddRow
        sessionId="abc"
        store={store}
        scheduleTimeout={clock.scheduleTimeout}
        mintClientEventId={() => uuid(0xc1)}
      />,
    );
    const input = screen.getByLabelText(/callsign/i);
    await user.type(input, "K2ABC");
    await user.keyboard("{Enter}");

    await waitFor(() => expect(input).toHaveValue(""));
    expect(document.activeElement).toBe(input);
  });

  it("Save & close commits but does NOT refocus the callsign (stops the rapid loop)", async () => {
    const user = userEvent.setup();
    const store = seededStore();
    const clock = fakeClock();
    const fetchMock = vi.fn().mockResolvedValue(jsonResponse(201, summary({ latestSeq: 2 })));
    vi.stubGlobal("fetch", fetchMock);

    render(
      <QuickAddRow
        sessionId="abc"
        store={store}
        scheduleTimeout={clock.scheduleTimeout}
        mintClientEventId={() => uuid(0xc1)}
        lookupRosterMemory={() => Promise.resolve(null)}
      />,
    );
    const input = screen.getByLabelText(/callsign/i);
    await user.type(input, "K2ABC");
    await user.click(screen.getByRole("button", { name: /save & close/i }));

    await waitFor(() => expect(input).toHaveValue(""));
    expect(document.activeElement).not.toBe(input);
    expect(fetchMock).toHaveBeenCalledTimes(1);
  });

  it("rolls the pending entry back via removePending and surfaces the mapped error on a failed send", async () => {
    const user = userEvent.setup();
    const store = seededStore();
    const clock = fakeClock();
    const fetchMock = vi
      .fn()
      .mockResolvedValue(jsonResponse(400, { type: "/errors/callsign-invalid", status: 400 }));
    vi.stubGlobal("fetch", fetchMock);

    render(
      <QuickAddRow
        sessionId="abc"
        store={store}
        scheduleTimeout={clock.scheduleTimeout}
        mintClientEventId={() => uuid(0xc1)}
      />,
    );
    const input = screen.getByLabelText(/callsign/i);
    await user.type(input, "K2ABC");
    await user.keyboard("{Enter}");

    // The optimistic row is rolled back (no phantom dimmed row lingers).
    await waitFor(() => expect(store.getState().pending.size).toBe(0));
    expect(await screen.findByRole("alert")).toHaveTextContent(
      messageForProblemType("/errors/callsign-invalid"),
    );
    // The callsign is kept so the operator can correct it (no clear on error).
    expect(input).toHaveValue("K2ABC");
  });

  it("removes a never-echoed pending entry when the bounded timeout fires (safety net)", async () => {
    const user = userEvent.setup();
    const store = seededStore();
    const clock = fakeClock();
    const fetchMock = vi.fn().mockResolvedValue(jsonResponse(201, summary({ latestSeq: 2 })));
    vi.stubGlobal("fetch", fetchMock);

    render(
      <QuickAddRow
        sessionId="abc"
        store={store}
        scheduleTimeout={clock.scheduleTimeout}
        mintClientEventId={() => uuid(0xc1)}
      />,
    );
    await user.type(screen.getByLabelText(/callsign/i), "K2ABC");
    await user.keyboard("{Enter}");

    // The send succeeded but the authoritative echo never arrived (no WS here),
    // so the pending entry lingers until the bounded timeout rolls it back.
    await waitFor(() => expect(store.getState().pending.size).toBe(1));
    clock.fireAll();
    expect(store.getState().pending.size).toBe(0);
  });

  it("rolls back a still-pending entry when the row unmounts before its echo arrives (session closed by another operator mid-flight)", async () => {
    const user = userEvent.setup();
    const store = seededStore();
    const clock = fakeClock();
    const pending = deferred<Response>();
    const fetchMock = vi.fn().mockReturnValue(pending.promise);
    vi.stubGlobal("fetch", fetchMock);

    const { unmount } = render(
      <QuickAddRow
        sessionId="abc"
        store={store}
        scheduleTimeout={clock.scheduleTimeout}
        mintClientEventId={() => uuid(0xc1)}
      />,
    );
    await user.type(screen.getByLabelText(/callsign/i), "K2ABC");
    await user.keyboard("{Enter}");
    await waitFor(() => expect(store.getState().pending.size).toBe(1));

    // The console unmounts (e.g. `isLive` flips false because another
    // operator closed the net) while our own add is still in flight and its
    // echo timeout is still armed. Merely cancelling that timer would leave
    // the dimmed row orphaned forever — unmount must roll it back itself.
    unmount();
    expect(store.getState().pending.size).toBe(0);

    // The timeout that would have fired later is disarmed; firing it now must
    // stay a safe no-op (no crash, no re-add).
    clock.fireAll();
    expect(store.getState().pending.size).toBe(0);

    pending.resolve(jsonResponse(201, summary({ latestSeq: 2 })));
  });

  it("the n hotkey focuses the callsign field from a non-input focus", async () => {
    const user = userEvent.setup();
    const store = seededStore();
    const clock = fakeClock();
    vi.stubGlobal("fetch", vi.fn());

    render(
      <div>
        <button type="button">elsewhere</button>
        <QuickAddRow sessionId="abc" store={store} scheduleTimeout={clock.scheduleTimeout} />
      </div>,
    );
    const outside = screen.getByRole("button", { name: "elsewhere" });
    outside.focus();
    expect(document.activeElement).toBe(outside);

    await user.keyboard("n");
    expect(document.activeElement).toBe(screen.getByLabelText(/callsign/i));
  });

  it("the n hotkey does NOT fire while typing in the callsign field (callsigns contain N)", async () => {
    const user = userEvent.setup();
    const store = seededStore();
    const clock = fakeClock();
    vi.stubGlobal("fetch", vi.fn());

    render(<QuickAddRow sessionId="abc" store={store} scheduleTimeout={clock.scheduleTimeout} />);
    const input = screen.getByLabelText(/callsign/i);
    await user.click(input);
    // Typing a callsign that contains N must land verbatim — the hotkey must not
    // hijack the keystroke while the caret is inside a text input.
    await user.keyboard("N1CCK");
    expect(input).toHaveValue("N1CCK");
  });

  it("the n hotkey ignores modifier combinations", async () => {
    const user = userEvent.setup();
    const store = seededStore();
    const clock = fakeClock();
    vi.stubGlobal("fetch", vi.fn());

    render(
      <div>
        <button type="button">elsewhere</button>
        <QuickAddRow sessionId="abc" store={store} scheduleTimeout={clock.scheduleTimeout} />
      </div>,
    );
    const outside = screen.getByRole("button", { name: "elsewhere" });
    outside.focus();
    await user.keyboard("{Control>}n{/Control}");
    expect(document.activeElement).toBe(outside);
  });

  it("shapes the report input's aria hint from the net mode", () => {
    const store = createSessionStore();
    // The net's mode is the first RF connection's, not a top-level
    // snapshot field, so a cw net is a cw CONNECTION.
    store
      .getState()
      .seedFromSnapshot(summary({ connections: [{ ...hfWay(), mode: "cw" }] }));
    const clock = fakeClock();
    vi.stubGlobal("fetch", vi.fn());

    render(
      <QuickAddRow sessionId="abc" store={store} mode="cw" scheduleTimeout={clock.scheduleTimeout} />,
    );
    // CW → RST family: the report field's accessible name names RST.
    expect(screen.getByLabelText(/signal report — rst/i)).toBeInTheDocument();
  });

  it("shapes the report input differently for a digital net (dB SNR)", () => {
    const store = seededStore();
    const clock = fakeClock();
    vi.stubGlobal("fetch", vi.fn());

    render(
      <QuickAddRow
        sessionId="abc"
        store={store}
        mode="digital"
        scheduleTimeout={clock.scheduleTimeout}
      />,
    );
    expect(screen.getByLabelText(/signal report — db snr/i)).toBeInTheDocument();
  });

  it("commits the selected staying status and report through addPending and the POST body", async () => {
    const user = userEvent.setup();
    const store = seededStore();
    const clock = fakeClock();
    const pending = deferred<Response>();
    const fetchMock = vi.fn().mockReturnValue(pending.promise);
    vi.stubGlobal("fetch", fetchMock);

    render(
      <QuickAddRow
        sessionId="abc"
        store={store}
        mode="ssb"
        scheduleTimeout={clock.scheduleTimeout}
        mintClientEventId={() => uuid(0xc1)}
        lookupRosterMemory={() => Promise.resolve(null)}
      />,
    );
    await user.type(screen.getByLabelText(/callsign/i), "K2ABC");
    await user.type(screen.getByLabelText(/signal report/i), "59");
    // Select "Staying" on the segmented toggle (default is In & out).
    await user.click(screen.getByRole("radio", { name: /staying for comments|^staying$/i }));
    await user.click(screen.getByRole("button", { name: /save & add new/i }));

    await waitFor(() => expect(store.getState().pending.size).toBe(1));
    const entry = [...store.getState().pending.values()][0];
    expect(entry).toMatchObject({
      callsign: "K2ABC",
      signalReport: "59",
      staying: "staying-for-comments",
    });

    const [, init] = fetchMock.mock.calls[0] as [string, RequestInit];
    expect(JSON.parse(init.body as string)).toEqual({
      callsign: "K2ABC",
      clientEventId: uuid(0xc1),
      signalReport: "59",
      staying: "staying-for-comments",
    });

    pending.resolve(jsonResponse(201, summary({ latestSeq: 2 })));
  });

  it("defaults staying to staying-for-comments and commits a blank report as no report", async () => {
    const user = userEvent.setup();
    const store = seededStore();
    const clock = fakeClock();
    const pending = deferred<Response>();
    const fetchMock = vi.fn().mockReturnValue(pending.promise);
    vi.stubGlobal("fetch", fetchMock);

    render(
      <QuickAddRow
        sessionId="abc"
        store={store}
        mode="ssb"
        scheduleTimeout={clock.scheduleTimeout}
        mintClientEventId={() => uuid(0xc1)}
      />,
    );
    await user.type(screen.getByLabelText(/callsign/i), "K2ABC");
    await user.keyboard("{Enter}");

    await waitFor(() => expect(store.getState().pending.size).toBe(1));
    const entry = [...store.getState().pending.values()][0];
    expect(entry.staying).toBe("staying-for-comments");
    expect(entry.signalReport).toBeNull();

    const [, init] = fetchMock.mock.calls[0] as [string, RequestInit];
    const parsed = JSON.parse(init.body as string) as Record<string, unknown>;
    // A blank report is omitted from the body (no empty-string report).
    expect("signalReport" in parsed).toBe(false);

    pending.resolve(jsonResponse(201, summary({ latestSeq: 2 })));
  });

  it("clears the report and resets staying to the default on a successful commit", async () => {
    const user = userEvent.setup();
    const store = seededStore();
    const clock = fakeClock();
    const fetchMock = vi.fn().mockResolvedValue(jsonResponse(201, summary({ latestSeq: 2 })));
    vi.stubGlobal("fetch", fetchMock);

    render(
      <QuickAddRow
        sessionId="abc"
        store={store}
        mode="ssb"
        scheduleTimeout={clock.scheduleTimeout}
        mintClientEventId={() => uuid(0xc1)}
        lookupRosterMemory={() => Promise.resolve(null)}
      />,
    );
    const report = screen.getByLabelText(/signal report/i);
    await user.type(screen.getByLabelText(/callsign/i), "K2ABC");
    await user.type(report, "59");
    // Flip away from the default to prove the reset actually happens, not
    // just that it was never touched.
    await user.click(screen.getByRole("radio", { name: /in ?& ?out|in and out/i }));
    await user.click(screen.getByRole("button", { name: /save & add new/i }));

    await waitFor(() => expect(report).toHaveValue(""));
    // Staying resets to the staying-for-comments default for the next
    // station (most check-ins stay for the whole net; in-and-out is the
    // exception, not the common case).
    await waitFor(() =>
      expect(
        screen.getByRole("radio", { name: /staying for comments|^staying$/i }),
      ).toHaveAttribute("aria-checked", "true"),
    );
  });

  it("has no WCAG 2.1 AA violations", async () => {
    const store = seededStore();
    const clock = fakeClock();
    vi.stubGlobal("fetch", vi.fn());
    const { container } = render(
      <QuickAddRow sessionId="abc" store={store} scheduleTimeout={clock.scheduleTimeout} />,
    );
    await expectNoAxeViolations(container);
  });

  it("collapses to a full-width one-handed stack below the desktop breakpoint", () => {
    setWidth(340);
    const store = seededStore();
    const clock = fakeClock();
    vi.stubGlobal("fetch", vi.fn());

    render(<QuickAddRow sessionId="abc" store={store} scheduleTimeout={clock.scheduleTimeout} />);

    const form = screen.getByLabelText("Quick add check-in");
    expect(form).toHaveAttribute("data-layout-mode", "stacked-card");
    expect(screen.getByLabelText(/callsign/i)).toHaveStyle({ width: "100%" });
    expect(screen.getByRole("button", { name: /save & add new/i })).toHaveStyle({
      width: "100%",
    });
    expect(screen.getByRole("button", { name: /save & close/i })).toHaveStyle({ width: "100%" });
  });

  it("keeps the way-in chips wrapping inside their own box at the phone breakpoint", () => {
    // The wrapping behaviour shipped as CSS and prose with no test behind it.
    // This project has shipped a checked-off responsive task with no breakpoint
    // code behind it before, so assert the order rather than describing it.
    //
    // jsdom performs no layout, so no test in this suite can measure a real
    // box: the test above asserts responsive intent the same way, through
    // the styles actually on the shipped elements. What this pins is the pair
    // that makes wrapping possible at all — a flex container allowed to wrap,
    // and a `min-width: 0` that lets it shrink below its content's intrinsic
    // width instead of forcing the row wider. Drop either and a six-connection
    // net pushes the quick-add past a 340px phone.
    setWidth(340);
    const store = seededStore({
      connections: [
        hfWay(14_230_000),
        { ...hfWay(14_250_000), id: uuid(0x31), position: 1 },
        { ...hfWay(21_300_000), id: uuid(0x32), position: 2, band: "15m" },
        { ...hfWay(28_400_000), id: uuid(0x33), position: 3, band: "10m" },
        { ...hfWay(7_200_000), id: uuid(0x34), position: 4, band: "40m" },
        { ...hfWay(3_900_000), id: uuid(0x35), position: 5, band: "80m" },
      ],
    });
    const clock = fakeClock();
    vi.stubGlobal("fetch", vi.fn());

    render(<QuickAddRow sessionId="abc" store={store} scheduleTimeout={clock.scheduleTimeout} />);

    const group = screen.getByRole("radiogroup", { name: /way in/i });
    expect(within(group).getAllByRole("radio")).toHaveLength(8); // 6 + not-recorded + Other
    expect(group).toHaveStyle({ flexWrap: "wrap", minWidth: "0px", maxWidth: "100%" });
  });

  it("stays a single inline row at/above the desktop breakpoint", () => {
    setWidth(900);
    const store = seededStore();
    const clock = fakeClock();
    vi.stubGlobal("fetch", vi.fn());

    render(<QuickAddRow sessionId="abc" store={store} scheduleTimeout={clock.scheduleTimeout} />);

    expect(screen.getByLabelText("Quick add check-in")).toHaveAttribute(
      "data-layout-mode",
      "row",
    );
  });
  it("prefers the server's problem detail over the slug-map fallback", async () => {
    // One of forty-two surfaces. The slug is deliberately NOT one
    // of the three whose `detail` is a token echo — `/errors/precedence-invalid`
    // reaches this row and would assert the inverse of the ordering.
    const detail = "the field-naming answer the server sent";
    const user = userEvent.setup();
    const store = seededStore();
    const clock = fakeClock();
    const fetchMock = vi.fn().mockResolvedValue(
      jsonResponse(400, {
        type: "/errors/callsign-invalid",
        status: 400,
        detail,
      }),
    );
    vi.stubGlobal("fetch", fetchMock);

    render(
      <QuickAddRow
        sessionId="abc"
        store={store}
        scheduleTimeout={clock.scheduleTimeout}
        mintClientEventId={() => uuid(0xc1)}
      />,
    );
    await user.type(screen.getByLabelText(/callsign/i), "K2ABC");
    await user.keyboard("{Enter}");

    const alert = await screen.findByRole("alert");
    expect(alert).toHaveTextContent(detail);
    expect(alert).not.toHaveTextContent(
      messageForProblemType("/errors/callsign-invalid"),
    );
  });

  it("keeps the map's copy when a precedence token echo comes back as the detail", async () => {
    // `/errors/precedence-invalid`'s `detail` is
    // "unrecognized precedence token: <echo>" — an implementation phrase, not
    // operator copy. This row is where an operator can actually produce one.
    const detail = "unrecognized precedence token: rutine";
    const user = userEvent.setup();
    const store = seededStore();
    const clock = fakeClock();
    const fetchMock = vi.fn().mockResolvedValue(
      jsonResponse(400, {
        type: "/errors/precedence-invalid",
        status: 400,
        detail,
      }),
    );
    vi.stubGlobal("fetch", fetchMock);

    render(
      <QuickAddRow
        sessionId="abc"
        store={store}
        scheduleTimeout={clock.scheduleTimeout}
        mintClientEventId={() => uuid(0xc2)}
      />,
    );
    await user.type(screen.getByLabelText(/callsign/i), "K2ABC");
    await user.keyboard("{Enter}");

    const alert = await screen.findByRole("alert");
    expect(alert).toHaveTextContent(
      messageForProblemType("/errors/precedence-invalid"),
    );
    expect(alert).not.toHaveTextContent(detail);
  });
});

describe("QuickAddRow roster-memory prefill", () => {
  it("renders editable Name and Location fields", async () => {
    const user = userEvent.setup();
    const store = seededStore();
    const clock = fakeClock();
    vi.stubGlobal("fetch", vi.fn());

    render(<QuickAddRow sessionId="abc" store={store} scheduleTimeout={clock.scheduleTimeout} />);

    const name = screen.getByLabelText(/^name$/i);
    const location = screen.getByLabelText(/^location$/i);
    await user.type(name, "Maria");
    await user.type(location, "Hartford, CT");
    expect(name).toHaveValue("Maria");
    expect(location).toHaveValue("Hartford, CT");
  });

  it("fills EMPTY name/location from the remembered station on callsign blur (seed-if-empty)", async () => {
    const user = userEvent.setup();
    const store = seededStore();
    const clock = fakeClock();
    vi.stubGlobal("fetch", vi.fn());
    const lookup = vi
      .fn()
      .mockResolvedValue({ name: "Maria", location: "Hartford, CT" });

    render(
      <QuickAddRow
        sessionId="abc"
        store={store}
        scheduleTimeout={clock.scheduleTimeout}
        lookupRosterMemory={lookup}
      />,
    );
    await user.type(screen.getByLabelText(/callsign/i), "W1AW");
    // Blur / tab-out fires the best-effort lookup (NOT per keystroke).
    await user.tab();

    await waitFor(() => expect(screen.getByLabelText(/^name$/i)).toHaveValue("Maria"));
    expect(screen.getByLabelText(/^location$/i)).toHaveValue("Hartford, CT");
    expect(lookup).toHaveBeenCalledWith("abc", "W1AW");
  });

  it("does NOT clobber operator-typed name/location on blur", async () => {
    const user = userEvent.setup();
    const store = seededStore();
    const clock = fakeClock();
    vi.stubGlobal("fetch", vi.fn());
    const lookup = vi
      .fn()
      .mockResolvedValue({ name: "Maria", location: "Hartford, CT" });

    render(
      <QuickAddRow
        sessionId="abc"
        store={store}
        scheduleTimeout={clock.scheduleTimeout}
        lookupRosterMemory={lookup}
      />,
    );
    // Operator types a name first, then the callsign, then blurs.
    await user.type(screen.getByLabelText(/^name$/i), "Bob");
    await user.type(screen.getByLabelText(/callsign/i), "W1AW");
    await user.tab();

    // The remembered name must not overwrite the operator-typed one.
    await waitFor(() => expect(lookup).toHaveBeenCalled());
    expect(screen.getByLabelText(/^name$/i)).toHaveValue("Bob");
    // Location was empty, so it is seeded from memory.
    expect(screen.getByLabelText(/^location$/i)).toHaveValue("Hartford, CT");
  });

  it("is a silent no-op on a miss (fields stay empty, no error)", async () => {
    const user = userEvent.setup();
    const store = seededStore();
    const clock = fakeClock();
    vi.stubGlobal("fetch", vi.fn());
    const lookup = vi.fn().mockResolvedValue({ name: null, location: null });

    render(
      <QuickAddRow
        sessionId="abc"
        store={store}
        scheduleTimeout={clock.scheduleTimeout}
        lookupRosterMemory={lookup}
      />,
    );
    await user.type(screen.getByLabelText(/callsign/i), "K9NEW");
    await user.tab();

    await waitFor(() => expect(lookup).toHaveBeenCalled());
    expect(screen.getByLabelText(/^name$/i)).toHaveValue("");
    expect(screen.getByLabelText(/^location$/i)).toHaveValue("");
    expect(screen.queryByRole("alert")).toBeNull();
  });

  it("drops a stale lookup result if the callsign changed before it resolved (race guard)", async () => {
    const user = userEvent.setup();
    const store = seededStore();
    const clock = fakeClock();
    vi.stubGlobal("fetch", vi.fn());

    let resolveFirst: (value: { name: string | null; location: string | null }) => void =
      () => {};
    const firstLookup = new Promise<{ name: string | null; location: string | null }>(
      (resolve) => {
        resolveFirst = resolve;
      },
    );
    const lookup = vi
      .fn()
      .mockImplementationOnce(() => firstLookup)
      .mockResolvedValueOnce({ name: null, location: null });

    render(
      <QuickAddRow
        sessionId="abc"
        store={store}
        scheduleTimeout={clock.scheduleTimeout}
        lookupRosterMemory={lookup}
      />,
    );

    // Operator enters W1AW and blurs — fires a lookup that will resolve LATE.
    await user.type(screen.getByLabelText(/callsign/i), "W1AW");
    await user.tab();
    await waitFor(() => expect(lookup).toHaveBeenCalledWith("abc", "W1AW"));

    // Before it resolves, the operator clears the field and enters a DIFFERENT
    // callsign, blurring again — a second (fast) lookup that misses.
    await user.click(screen.getByLabelText(/callsign/i));
    await user.clear(screen.getByLabelText(/callsign/i));
    await user.type(screen.getByLabelText(/callsign/i), "K9NEW");
    await user.tab();
    await waitFor(() => expect(lookup).toHaveBeenCalledWith("abc", "K9NEW"));

    // Now the FIRST (stale) lookup for W1AW finally resolves.
    resolveFirst({ name: "Maria", location: "Hartford, CT" });

    // W1AW's remembered identity must NOT land on the K9NEW row.
    await waitFor(() => expect(lookup).toHaveBeenCalledTimes(2));
    expect(screen.getByLabelText(/^name$/i)).toHaveValue("");
    expect(screen.getByLabelText(/^location$/i)).toHaveValue("");
  });

  it("a failed lookup never blocks the commit", async () => {
    const user = userEvent.setup();
    const store = seededStore();
    const clock = fakeClock();
    const fetchMock = vi.fn().mockResolvedValue(jsonResponse(201, summary({ latestSeq: 2 })));
    vi.stubGlobal("fetch", fetchMock);
    const lookup = vi.fn().mockResolvedValue(null); // best-effort failure → null

    render(
      <QuickAddRow
        sessionId="abc"
        store={store}
        scheduleTimeout={clock.scheduleTimeout}
        lookupRosterMemory={lookup}
      />,
    );
    await user.type(screen.getByLabelText(/callsign/i), "W1AW");
    await user.tab();
    await waitFor(() => expect(lookup).toHaveBeenCalled());

    await user.keyboard("{Enter}");
    // The commit still POSTed despite the failed lookup.
    await waitFor(() => expect(fetchMock).toHaveBeenCalled());
    const [url] = fetchMock.mock.calls[0] as [string, RequestInit];
    expect(url).toBe("/api/net-sessions/abc/check-ins");
  });

  it("commits the confirmed name and location on the add", async () => {
    const user = userEvent.setup();
    const store = seededStore();
    const clock = fakeClock();
    const fetchMock = vi.fn().mockResolvedValue(jsonResponse(201, summary({ latestSeq: 2 })));
    vi.stubGlobal("fetch", fetchMock);
    const lookup = vi
      .fn()
      .mockResolvedValue({ name: "Maria", location: "Hartford, CT" });

    render(
      <QuickAddRow
        sessionId="abc"
        store={store}
        scheduleTimeout={clock.scheduleTimeout}
        mintClientEventId={() => uuid(0xc1)}
        lookupRosterMemory={lookup}
      />,
    );
    await user.type(screen.getByLabelText(/callsign/i), "W1AW");
    await user.tab();
    await waitFor(() => expect(screen.getByLabelText(/^name$/i)).toHaveValue("Maria"));
    await user.keyboard("{Enter}");

    await waitFor(() => expect(fetchMock).toHaveBeenCalled());
    const [, init] = fetchMock.mock.calls[0] as [string, RequestInit];
    expect(JSON.parse(init.body as string)).toMatchObject({
      callsign: "W1AW",
      name: "Maria",
      location: "Hartford, CT",
    });
  });

  it("commits a typed grid on the add and omits the key when the field is blank", async () => {
    // Quick-add is the create path an operator actually uses during a net. A
    // blank grid must stay OUT of the body, so a callsign-only add is
    // byte-identical to one sent before the grid field existed.
    const user = userEvent.setup();
    const store = seededStore();
    const clock = fakeClock();
    const fetchMock = vi.fn().mockResolvedValue(jsonResponse(201, summary({ latestSeq: 2 })));
    vi.stubGlobal("fetch", fetchMock);

    render(
      <QuickAddRow
        sessionId="abc"
        store={store}
        scheduleTimeout={clock.scheduleTimeout}
        mintClientEventId={() => uuid(0xc2)}
        lookupRosterMemory={vi.fn().mockResolvedValue(null)}
      />,
    );
    await user.type(screen.getByLabelText(/callsign/i), "W1AW");
    await user.type(screen.getByLabelText(/^grid$/i), "FN31");
    await user.keyboard("{Enter}");

    await waitFor(() => expect(fetchMock).toHaveBeenCalled());
    const [, init] = fetchMock.mock.calls[0] as [string, RequestInit];
    expect(JSON.parse(init.body as string)).toMatchObject({
      callsign: "W1AW",
      grid: "FN31",
    });

    // Second add with the grid field left blank: the key is absent entirely.
    await user.type(screen.getByLabelText(/callsign/i), "N1CCK");
    await user.keyboard("{Enter}");
    await waitFor(() => expect(fetchMock).toHaveBeenCalledTimes(2));
    const [, second] = fetchMock.mock.calls[1] as [string, RequestInit];
    expect(JSON.parse(second.body as string)).not.toHaveProperty("grid");
  });
});

describe("QuickAddRow eyebrow + step indicator (design handoff `2b`)", () => {
  it("renders a 'Quick add' eyebrow naming the n hotkey", () => {
    const store = createSessionStore();
    store.getState().seedFromSnapshot(summary());
    vi.stubGlobal("fetch", vi.fn());

    render(<QuickAddRow sessionId="abc" store={store} />);

    expect(screen.getByText(/quick add/i)).toBeInTheDocument();
    const group = screen.getByRole("group", { name: /quick add/i });
    expect(within(group).getByText("n")).toBeInTheDocument();
  });

  it("names the same field order the form actually tabs through", async () => {
    // The step indicator and `docs/live-sessions/log-check-ins.md` had each
    // drifted from the rendered form (one dropped Grid, the other dropped
    // Location and Grid). This walks the real focus order and holds the
    // indicator to it, so a new field cannot land without both being updated.
    const user = userEvent.setup();
    const store = createSessionStore();
    store.getState().seedFromSnapshot(summary());
    vi.stubGlobal("fetch", vi.fn());

    render(<QuickAddRow sessionId="abc" store={store} />);

    const labelled: string[] = [];
    screen.getByLabelText(/callsign/i).focus();
    for (let i = 0; i < 12; i += 1) {
      const active = document.activeElement;
      if (!(active instanceof HTMLInputElement)) {
        break;
      }
      const fieldLabel = active.closest("label")?.querySelector("span")?.textContent ?? "";
      labelled.push(fieldLabel);
      await user.tab();
    }
    expect(labelled).toEqual(["Callsign", "Name", "Location", "Grid", "Report"]);
    // The trailing steps are the two Staying radios and the two Save buttons,
    // which are not inputs — the indicator collapses each pair into one step.
    const steps = screen.getByRole("list", { name: /field order/i });
    expect(
      within(steps)
        .getAllByRole("listitem")
        .map((item) => item.textContent?.replace(/›/g, "").trim()),
    ).toEqual([...labelled, "Staying", "Save"]);
  });

});


describe("QuickAddRow duplicate check-in warning", () => {
  /** A roster station on the seeded snapshot, worked or not. */
  function station(n: number, callsign: string, worked: boolean) {
    return {
      checkInId: uuid(500 + n),
      callsign,
      addedAt: "2026-07-16T00:00:01Z",
      addedBy: uuid(200),
      source: "staff" as const,
      staying: "in-and-out" as const,
      precedence: "routine" as const,
      worked,
      version: 1,
      corrections: [],
    };
  }

  const warning = (): HTMLElement =>
    screen.getByRole("status", { name: /duplicate check-in/i });

  /** No prefill traffic — these tests own the fetch mock's call count. */
  const noPrefill = () => Promise.resolve(null);

  it("announces an unworked duplicate when the callsign field blurs", async () => {
    const user = userEvent.setup();
    const store = seededStore({ roster: [station(1, "W1AW", false)] });
    vi.stubGlobal("fetch", vi.fn());

    render(
      <QuickAddRow
        sessionId="abc"
        store={store}
        scheduleTimeout={fakeClock().scheduleTimeout}
        lookupRosterMemory={noPrefill}
      />,
    );
    expect(warning()).toHaveAttribute("data-duplicate-state", "none");
    await user.type(screen.getByLabelText(/callsign/i), "W1AW");
    await user.tab();

    await waitFor(() => expect(warning()).toHaveAttribute("data-duplicate-state", "unworked"));
  });

  it("announces a worked duplicate distinguishably from an unworked one", async () => {
    const user = userEvent.setup();
    const store = seededStore({ roster: [station(1, "W1AW", true)] });
    vi.stubGlobal("fetch", vi.fn());

    render(
      <QuickAddRow
        sessionId="abc"
        store={store}
        scheduleTimeout={fakeClock().scheduleTimeout}
        lookupRosterMemory={noPrefill}
      />,
    );
    await user.type(screen.getByLabelText(/callsign/i), "W1AW");
    await user.tab();

    await waitFor(() => expect(warning()).toHaveAttribute("data-duplicate-state", "worked"));
  });

  it("detects a duplicate typed with a portable designator and without shift", async () => {
    const user = userEvent.setup();
    const store = seededStore({ roster: [station(1, "W1AW", false)] });
    vi.stubGlobal("fetch", vi.fn());

    render(
      <QuickAddRow
        sessionId="abc"
        store={store}
        scheduleTimeout={fakeClock().scheduleTimeout}
        lookupRosterMemory={noPrefill}
      />,
    );
    await user.type(screen.getByLabelText(/callsign/i), "dl/w1aw/p");
    await user.tab();

    await waitFor(() => expect(warning()).toHaveAttribute("data-duplicate-state", "unworked"));
  });

  it("does not block the commit while the warning is showing", async () => {
    const user = userEvent.setup();
    const store = seededStore({ roster: [station(1, "W1AW", false)] });
    const fetchMock = vi.fn().mockResolvedValue(jsonResponse(201, summary({ latestSeq: 2 })));
    vi.stubGlobal("fetch", fetchMock);

    render(
      <QuickAddRow
        sessionId="abc"
        store={store}
        scheduleTimeout={fakeClock().scheduleTimeout}
        mintClientEventId={() => uuid(0xc1)}
        lookupRosterMemory={noPrefill}
      />,
    );
    const input = screen.getByLabelText(/callsign/i);
    await user.type(input, "W1AW");
    await user.tab();
    await waitFor(() => expect(warning()).toHaveAttribute("data-duplicate-state", "unworked"));

    // No confirmation step stands between the warning and the send: the very
    // next commit goes straight through, and the field is never marked invalid.
    expect(input).not.toHaveAttribute("aria-invalid");
    input.focus();
    await user.keyboard("{Enter}");

    await waitFor(() => expect(fetchMock).toHaveBeenCalledTimes(1));
    const [url, init] = fetchMock.mock.calls[0] as [string, RequestInit];
    expect(url).toBe("/api/net-sessions/abc/check-ins");
    expect(JSON.parse(init.body as string)).toMatchObject({ callsign: "W1AW" });
  });

  it("warns without waiting on the prefill lookup and issues no request to detect", async () => {
    const user = userEvent.setup();
    const store = seededStore({ roster: [station(1, "W1AW", false)] });
    const fetchMock = vi.fn().mockResolvedValue(jsonResponse(201, summary({ latestSeq: 2 })));
    vi.stubGlobal("fetch", fetchMock);

    render(
      <QuickAddRow
        sessionId="abc"
        store={store}
        scheduleTimeout={fakeClock().scheduleTimeout}
        mintClientEventId={() => uuid(0xc1)}
        lookupRosterMemory={() => new Promise<null>(() => {})}
      />,
    );
    const input = screen.getByLabelText(/callsign/i);
    await user.type(input, "W1AW");
    await user.tab();

    await waitFor(() => expect(warning()).toHaveAttribute("data-duplicate-state", "unworked"));
    // Nothing went to the network to work that out.
    expect(fetchMock).not.toHaveBeenCalled();

    input.focus();
    await user.keyboard("{Enter}");
    await waitFor(() => expect(fetchMock).toHaveBeenCalledTimes(1));
  });

  it("clears the warning as soon as the operator edits the callsign", async () => {
    const user = userEvent.setup();
    const store = seededStore({ roster: [station(1, "W1AW", false)] });
    vi.stubGlobal("fetch", vi.fn());

    render(
      <QuickAddRow
        sessionId="abc"
        store={store}
        scheduleTimeout={fakeClock().scheduleTimeout}
        lookupRosterMemory={noPrefill}
      />,
    );
    const input = screen.getByLabelText(/callsign/i);
    await user.type(input, "W1AW");
    await user.tab();
    await waitFor(() => expect(warning()).toHaveAttribute("data-duplicate-state", "unworked"));

    input.focus();
    await user.type(input, "X");
    expect(warning()).toHaveAttribute("data-duplicate-state", "none");
  });

  it("a warning left standing by a commit names its station, and the next keystroke clears it — it never becomes a verdict on the next callsign", async () => {
    // This test used to pin "clears the warning
    // after a successful commit, so it cannot outlive its subject".
    // The intent stands — a stale warning must never read as a verdict on the
    // NEXT callsign — but clearing on commit made the Enter-path warning
    // observable only for the length of the round-trip. The intent is now
    // proved by the subject the region carries plus the keystroke clear.
    const user = userEvent.setup();
    const store = seededStore({ roster: [station(1, "W1AW", false)] });
    const fetchMock = vi.fn().mockResolvedValue(jsonResponse(201, summary({ latestSeq: 2 })));
    vi.stubGlobal("fetch", fetchMock);

    render(
      <QuickAddRow
        sessionId="abc"
        store={store}
        scheduleTimeout={fakeClock().scheduleTimeout}
        mintClientEventId={() => uuid(0xc1)}
        lookupRosterMemory={noPrefill}
      />,
    );
    const input = screen.getByLabelText(/callsign/i);
    await user.type(input, "W1AW");
    await user.tab();
    await waitFor(() => expect(warning()).toHaveAttribute("data-duplicate-state", "unworked"));

    input.focus();
    await user.keyboard("{Enter}");

    await waitFor(() => expect(input).toHaveValue(""));
    // The field is empty and the warning still stands — about W1AW.
    expect(warning()).toHaveAttribute("data-duplicate-state", "unworked");
    expect(warning()).toHaveAttribute("data-duplicate-callsign", "W1AW");

    // The first character of the next callsign clears state AND subject,
    // so nothing about W1AW can be read as a verdict on the station now typed.
    await user.type(input, "K");
    expect(warning()).toHaveAttribute("data-duplicate-state", "none");
    expect(warning()).not.toHaveAttribute("data-duplicate-callsign");
  });

  it("detects a still-pending optimistic row — the double-Enter on one station", async () => {
    const user = userEvent.setup();
    const store = seededStore();
    const fetchMock = vi.fn().mockResolvedValue(jsonResponse(201, summary({ latestSeq: 2 })));
    vi.stubGlobal("fetch", fetchMock);

    render(
      <QuickAddRow
        sessionId="abc"
        store={store}
        scheduleTimeout={fakeClock().scheduleTimeout}
        mintClientEventId={() => uuid(0xc1)}
        lookupRosterMemory={noPrefill}
      />,
    );
    const input = screen.getByLabelText(/callsign/i);
    await user.type(input, "K2ABC");
    await user.keyboard("{Enter}");
    // The echo has not landed, so the only evidence of this station is the
    // optimistic pending row — which is exactly the duplicate to catch.
    await waitFor(() => expect(store.getState().pending.size).toBe(1));

    await user.type(input, "K2ABC");
    await user.tab();
    await waitFor(() => expect(warning()).toHaveAttribute("data-duplicate-state", "unworked"));
  });

  it("detects a still-pending optimistic row typed with a designator — the portable double-Enter", async () => {
    const user = userEvent.setup();
    const store = seededStore();
    const fetchMock = vi.fn().mockResolvedValue(jsonResponse(201, summary({ latestSeq: 2 })));
    vi.stubGlobal("fetch", fetchMock);

    render(
      <QuickAddRow
        sessionId="abc"
        store={store}
        scheduleTimeout={fakeClock().scheduleTimeout}
        mintClientEventId={() => uuid(0xc1)}
        lookupRosterMemory={noPrefill}
      />,
    );
    const input = screen.getByLabelText(/callsign/i);
    await user.type(input, "W1AW/P");
    await user.keyboard("{Enter}");
    // The pending row holds the RAW text `W1AW/P` — the server has not echoed,
    // so nothing has normalized it. The server will store BOTH adds as `W1AW`.
    await waitFor(() => expect(store.getState().pending.size).toBe(1));
    expect([...store.getState().pending.values()][0]?.callsign).toBe("W1AW/P");

    await user.type(input, "W1AW");
    await user.tab();
    await waitFor(() => expect(warning()).toHaveAttribute("data-duplicate-state", "unworked"));
  });

  it("detects a byte-identical repeat against a still-pending designator row", async () => {
    const user = userEvent.setup();
    const store = seededStore();
    const fetchMock = vi.fn().mockResolvedValue(jsonResponse(201, summary({ latestSeq: 2 })));
    vi.stubGlobal("fetch", fetchMock);

    render(
      <QuickAddRow
        sessionId="abc"
        store={store}
        scheduleTimeout={fakeClock().scheduleTimeout}
        mintClientEventId={() => uuid(0xc1)}
        lookupRosterMemory={noPrefill}
      />,
    );
    const input = screen.getByLabelText(/callsign/i);
    await user.type(input, "W1AW/P");
    await user.keyboard("{Enter}");
    await waitFor(() => expect(store.getState().pending.size).toBe(1));

    await user.type(input, "W1AW/P");
    await user.tab();
    await waitFor(() => expect(warning()).toHaveAttribute("data-duplicate-state", "unworked"));
  });

  // The check reaches the COMMIT path. It once ran only at blur, and the
  // documented fast path (type, Enter, type, Enter) never blurs the field — so
  // the feature never fired where it was needed.

  it("warns on the Enter path without the field ever losing focus, and names the station", async () => {
    const user = userEvent.setup();
    const store = seededStore({ roster: [station(1, "W1AW", false)] });
    const fetchMock = vi.fn().mockResolvedValue(jsonResponse(201, summary({ latestSeq: 2 })));
    vi.stubGlobal("fetch", fetchMock);

    render(
      <QuickAddRow
        sessionId="abc"
        store={store}
        scheduleTimeout={fakeClock().scheduleTimeout}
        mintClientEventId={() => uuid(0xc1)}
        lookupRosterMemory={noPrefill}
      />,
    );
    const input = screen.getByLabelText(/callsign/i);
    await user.type(input, "W1AW");
    // No tab: Enter commits straight from the callsign field.
    await user.keyboard("{Enter}");

    await waitFor(() => expect(input).toHaveValue(""));
    // The same state a tab-out produces: the warning survives the commit that
    // cleared the field, so it must say WHICH station it is about.
    expect(warning()).toHaveAttribute("data-duplicate-state", "unworked");
    expect(warning()).toHaveAttribute("data-duplicate-callsign", "W1AW");
    // Still advisory — the POST went, the input was never marked invalid,
    // and the region is a polite status, not an alert.
    expect(fetchMock).toHaveBeenCalledTimes(1);
    expect(input).not.toHaveAttribute("aria-invalid");
    expect(warning()).toHaveAttribute("role", "status");
  });

  it("reports none on the Enter path for a callsign that is not on the roster (the self-match fence)", async () => {
    // `selectRoster` folds in this client's OWN un-echoed pending rows. A check
    // computed after `addPending` therefore matches the station against the row
    // commit just created and answers `unworked` for EVERY callsign — and
    // every shaped test stays green. This is the only test that refuses that.
    const user = userEvent.setup();
    const store = seededStore({ roster: [station(1, "W1AW", false)] });
    const fetchMock = vi.fn().mockResolvedValue(jsonResponse(201, summary({ latestSeq: 2 })));
    vi.stubGlobal("fetch", fetchMock);

    render(
      <QuickAddRow
        sessionId="abc"
        store={store}
        scheduleTimeout={fakeClock().scheduleTimeout}
        mintClientEventId={() => uuid(0xc1)}
        lookupRosterMemory={noPrefill}
      />,
    );
    const input = screen.getByLabelText(/callsign/i);
    await user.type(input, "K2ABC");
    await user.keyboard("{Enter}");

    await waitFor(() => expect(input).toHaveValue(""));
    // The pending K2ABC row now exists in the store — and must not have been
    // what the check compared K2ABC against.
    expect(store.getState().pending.size).toBe(1);
    expect(warning()).toHaveAttribute("data-duplicate-state", "none");
    expect(warning()).not.toHaveAttribute("data-duplicate-callsign");
  });

  it("warns on Save & close too — the check lives in commit, not on the Enter key", async () => {
    const user = userEvent.setup();
    const store = seededStore({ roster: [station(1, "W1AW", true)] });
    const fetchMock = vi.fn().mockResolvedValue(jsonResponse(201, summary({ latestSeq: 2 })));
    vi.stubGlobal("fetch", fetchMock);

    render(
      <QuickAddRow
        sessionId="abc"
        store={store}
        scheduleTimeout={fakeClock().scheduleTimeout}
        mintClientEventId={() => uuid(0xc1)}
        lookupRosterMemory={noPrefill}
      />,
    );
    const input = screen.getByLabelText(/callsign/i);
    await user.type(input, "W1AW");
    // The click moves focus to the button, so the blur seam fires first — the
    // assertion below is on what stands AFTER the commit has settled, which
    // only the commit path's own computation (and its survival) can supply.
    await user.click(screen.getByRole("button", { name: /save & close/i }));

    await waitFor(() => expect(input).toHaveValue(""));
    expect(warning()).toHaveAttribute("data-duplicate-state", "worked");
    expect(warning()).toHaveAttribute("data-duplicate-callsign", "W1AW");
  });

  it("does not warn when a pending row collides only on a designator segment", async () => {
    const user = userEvent.setup();
    const store = seededStore();
    const fetchMock = vi.fn().mockResolvedValue(jsonResponse(201, summary({ latestSeq: 2 })));
    vi.stubGlobal("fetch", fetchMock);

    render(
      <QuickAddRow
        sessionId="abc"
        store={store}
        scheduleTimeout={fakeClock().scheduleTimeout}
        mintClientEventId={() => uuid(0xc1)}
        lookupRosterMemory={noPrefill}
      />,
    );
    const input = screen.getByLabelText(/callsign/i);
    await user.type(input, "K2ABC/P");
    await user.keyboard("{Enter}");
    await waitFor(() => expect(store.getState().pending.size).toBe(1));

    // A DIFFERENT portable station. The only shared segment is the `/P`
    // designator, which identifies nobody.
    await user.type(input, "W1AW/P");
    await user.tab();
    expect(warning()).toHaveAttribute("data-duplicate-state", "none");
    // The row it could have collided with is still there — this is a real
    // negative, not a negative won by an empty roster.
    expect(store.getState().pending.size).toBe(1);
  });

  // The warning is derived from the roster as it stands NOW, not from a
  // snapshot taken when the check last ran. The four tests below each move the
  // roster under a standing warning and hold the warning to the new truth.

  it("drops the warning when the station it names is removed over the websocket while it stands", async () => {
    const user = userEvent.setup();
    const store = seededStore({ latestSeq: 1, roster: [station(1, "W1AW", false)] });
    vi.stubGlobal("fetch", vi.fn());

    render(
      <QuickAddRow
        sessionId="abc"
        store={store}
        scheduleTimeout={fakeClock().scheduleTimeout}
        lookupRosterMemory={noPrefill}
      />,
    );
    await user.type(screen.getByLabelText(/callsign/i), "W1AW");
    await user.tab();
    await waitFor(() => expect(warning()).toHaveAttribute("data-duplicate-state", "unworked"));

    // Another operator removes the W1AW entry the warning was about.
    act(() => {
      store.getState().applyEvent({
        seq: 2,
        kind: "checkin.removed",
        actorId: uuid(201),
        at: "2026-07-16T00:00:06Z",
        payload: { checkInId: uuid(501) },
      });
    });

    expect(warning()).toHaveAttribute("data-duplicate-state", "none");
    expect(warning()).not.toHaveAttribute("data-duplicate-callsign");
  });

  it("raises the warning when the station arrives over the websocket after the check ran", async () => {
    const user = userEvent.setup();
    const store = seededStore({ latestSeq: 1 });
    vi.stubGlobal("fetch", vi.fn());

    render(
      <QuickAddRow
        sessionId="abc"
        store={store}
        scheduleTimeout={fakeClock().scheduleTimeout}
        lookupRosterMemory={noPrefill}
      />,
    );
    await user.type(screen.getByLabelText(/callsign/i), "W1AW");
    await user.tab();
    expect(warning()).toHaveAttribute("data-duplicate-state", "none");

    // A co-NCS logs W1AW while the operator's cursor sits in the Name field.
    act(() => {
      store.getState().applyEvent({
        seq: 2,
        kind: "checkin.added",
        actorId: uuid(201),
        at: "2026-07-16T00:00:06Z",
        payload: { checkInId: uuid(600), callsign: "W1AW" },
      });
    });

    expect(warning()).toHaveAttribute("data-duplicate-state", "unworked");
    expect(warning()).toHaveAttribute("data-duplicate-callsign", "W1AW");
  });

  it("never reads the echo of its own commit as a duplicate of the station just added", async () => {
    // Not a staleness test: this pins the self-match fence across the echo. A live derivation that only excluded the pending
    // row would flip to `unworked` the moment the authoritative row replaced it
    // — on every fresh commit, one round-trip late.
    const user = userEvent.setup();
    const store = seededStore({ latestSeq: 1 });
    const fetchMock = vi.fn().mockResolvedValue(jsonResponse(201, summary({ latestSeq: 2 })));
    vi.stubGlobal("fetch", fetchMock);

    render(
      <QuickAddRow
        sessionId="abc"
        store={store}
        scheduleTimeout={fakeClock().scheduleTimeout}
        mintClientEventId={() => uuid(0xc1)}
        lookupRosterMemory={noPrefill}
      />,
    );
    const input = screen.getByLabelText(/callsign/i);
    await user.type(input, "K2ABC");
    await user.keyboard("{Enter}");
    await waitFor(() => expect(input).toHaveValue(""));
    expect(warning()).toHaveAttribute("data-duplicate-state", "none");

    act(() => {
      store.getState().applyEvent({
        seq: 2,
        kind: "checkin.added",
        actorId: uuid(200),
        at: "2026-07-16T00:00:06Z",
        payload: { checkInId: uuid(600), callsign: "K2ABC", clientEventId: uuid(0xc1) },
      });
    });

    expect(store.getState().pending.size).toBe(0);
    expect(selectRoster(store.getState())).toHaveLength(1);
    expect(warning()).toHaveAttribute("data-duplicate-state", "none");
  });

  it("keeps a post-commit warning honest after the echo: it names the OTHER entry, and drops when that entry goes", async () => {
    const user = userEvent.setup();
    const store = seededStore({ latestSeq: 1, roster: [station(1, "W1AW", false)] });
    const fetchMock = vi.fn().mockResolvedValue(jsonResponse(201, summary({ latestSeq: 2 })));
    vi.stubGlobal("fetch", fetchMock);

    render(
      <QuickAddRow
        sessionId="abc"
        store={store}
        scheduleTimeout={fakeClock().scheduleTimeout}
        mintClientEventId={() => uuid(0xc1)}
        lookupRosterMemory={noPrefill}
      />,
    );
    const input = screen.getByLabelText(/callsign/i);
    await user.type(input, "W1AW");
    await user.keyboard("{Enter}");
    await waitFor(() => expect(input).toHaveValue(""));
    expect(warning()).toHaveAttribute("data-duplicate-state", "unworked");

    // The echo lands: W1AW is now on the roster twice, authoritatively.
    act(() => {
      store.getState().applyEvent({
        seq: 2,
        kind: "checkin.added",
        actorId: uuid(200),
        at: "2026-07-16T00:00:06Z",
        payload: { checkInId: uuid(600), callsign: "W1AW", clientEventId: uuid(0xc1) },
      });
    });
    expect(selectRoster(store.getState())).toHaveLength(2);
    expect(warning()).toHaveAttribute("data-duplicate-state", "unworked");

    // Another operator resolves the collision by removing the OLDER entry. The
    // only W1AW left is the one this commit added — no longer a duplicate.
    act(() => {
      store.getState().applyEvent({
        seq: 3,
        kind: "checkin.removed",
        actorId: uuid(201),
        at: "2026-07-16T00:00:07Z",
        payload: { checkInId: uuid(501) },
      });
    });

    expect(selectRoster(store.getState())).toHaveLength(1);
    expect(warning()).toHaveAttribute("data-duplicate-state", "none");
  });

  it("drops a warning that pointed at this client's own pending row once the failed send rolls it back", async () => {
    const user = userEvent.setup();
    const store = seededStore({ latestSeq: 1 });
    const held = deferred<Response>();
    vi.stubGlobal("fetch", vi.fn().mockReturnValue(held.promise));

    render(
      <QuickAddRow
        sessionId="abc"
        store={store}
        scheduleTimeout={fakeClock().scheduleTimeout}
        mintClientEventId={() => uuid(0xc1)}
        lookupRosterMemory={noPrefill}
      />,
    );
    const input = screen.getByLabelText(/callsign/i);
    await user.type(input, "K2ABC");
    await user.keyboard("{Enter}");
    await waitFor(() => expect(store.getState().pending.size).toBe(1));

    // The send is still in flight, so the field still reads K2ABC. Tabbing out
    // checks it against the roster — whose only K2ABC is the pending row.
    await user.tab();
    await waitFor(() => expect(warning()).toHaveAttribute("data-duplicate-state", "unworked"));

    held.resolve(jsonResponse(400, { type: "/errors/callsign-invalid", status: 400 }));
    await waitFor(() => expect(store.getState().pending.size).toBe(0));

    // The row the warning was about no longer exists, and the field kept its
    // value — the warning must not stand over it.
    expect(input).toHaveValue("K2ABC");
    expect(warning()).toHaveAttribute("data-duplicate-state", "none");
  });

  it("has no accessibility violations while a duplicate warning is showing", async () => {
    const user = userEvent.setup();
    const store = seededStore({ roster: [station(1, "W1AW", false)] });
    vi.stubGlobal("fetch", vi.fn());

    const { container } = render(
      <QuickAddRow
        sessionId="abc"
        store={store}
        scheduleTimeout={fakeClock().scheduleTimeout}
        lookupRosterMemory={noPrefill}
      />,
    );
    await user.type(screen.getByLabelText(/callsign/i), "W1AW");
    await user.tab();
    await waitFor(() => expect(warning()).toHaveAttribute("data-duplicate-state", "unworked"));

    await expectNoAxeViolations(container);
  });
});

describe("QuickAddRow — the way in it records", () => {
  /** A second connection so the override has somewhere to go, and so the
   * quick-add is exercised on a net that is genuinely cross-mode. */
  function echolinkWay() {
    return {
      ...hfWay(),
      id: "00000000-0000-0000-0000-000000001606",
      position: 1,
      kind: "echolink",
      plannedFrequencyHz: null,
      band: null,
      mode: null,
      node: "N1CCK-R",
    };
  }

  const crossMode = { connections: [hfWay(), echolinkWay()] };

  /** Commits `callsign` and returns the parsed POST body. */
  async function commitAndReadBody(
    fetchMock: ReturnType<typeof vi.fn>,
    user: ReturnType<typeof userEvent.setup>,
    callsign = "N1CCK",
  ): Promise<Record<string, unknown>> {
    await user.type(screen.getByLabelText(/callsign/i), callsign);
    await user.keyboard("{Enter}");
    await waitFor(() => expect(fetchMock).toHaveBeenCalled());
    const [, init] = fetchMock.mock.calls[0] as [string, RequestInit];
    return JSON.parse(init.body as string) as Record<string, unknown>;
  }

  it("POSTs the stamp as `via` with the operator touching nothing on the hot path", async () => {
    const user = userEvent.setup();
    const store = seededStore(crossMode);
    store.getState().setViaStamp({ kind: "connection", connectionId: echolinkWay().id });
    const fetchMock = vi.fn().mockResolvedValue(jsonResponse(201, summary(crossMode)));
    vi.stubGlobal("fetch", fetchMock);

    render(<QuickAddRow sessionId="abc" store={store} />);

    expect(await commitAndReadBody(fetchMock, user)).toMatchObject({
      callsign: "N1CCK",
      via: { kind: "connection", connectionId: echolinkWay().id },
    });
  });

  it("omits the `via` KEY ENTIRELY when no stamp was ever set", async () => {
    // Including on the ONE-connection net, where defaulting to the only way in
    // looks unarguable and is not: "recorded on HF" and "not recorded, on a net
    // that only has HF" are different facts, and only the second stays true
    // when the owner later adds an EchoLink connection.
    const user = userEvent.setup();
    const store = seededStore({ connections: [hfWay()] });
    const fetchMock = vi.fn().mockResolvedValue(jsonResponse(201, summary()));
    vi.stubGlobal("fetch", fetchMock);

    render(<QuickAddRow sessionId="abc" store={store} />);

    const body = await commitAndReadBody(fetchMock, user);
    expect(Object.hasOwn(body, "via")).toBe(false);
  });

  it("preselects the stamp, so what is being recorded is legible at the point of use", () => {
    const store = seededStore(crossMode);
    store.getState().setViaStamp({ kind: "connection", connectionId: echolinkWay().id });
    vi.stubGlobal("fetch", vi.fn());

    render(<QuickAddRow sessionId="abc" store={store} />);

    const group = screen.getByRole("radiogroup", { name: /way in/i });
    const checked = within(group)
      .getAllByRole("radio")
      .filter((radio) => radio.getAttribute("aria-checked") === "true")
      .map((radio) => radio.textContent);
    expect(checked).toEqual(["EchoLink — N1CCK-R"]);
  });

  it("overrides for ONE station in one tap, and returns to the stamp on commit", async () => {
    // The interleaved half the stamp cannot carry: a station arriving on a
    // different connection mid-block costs one tap and nothing afterwards.
    const user = userEvent.setup();
    const store = seededStore(crossMode);
    store.getState().setViaStamp({ kind: "connection", connectionId: echolinkWay().id });
    const fetchMock = vi.fn().mockResolvedValue(jsonResponse(201, summary(crossMode)));
    vi.stubGlobal("fetch", fetchMock);

    render(<QuickAddRow sessionId="abc" store={store} />);

    await user.click(screen.getByRole("radio", { name: "HF — 14.230 MHz" }));
    const first = await commitAndReadBody(fetchMock, user, "W1AW");
    expect(first).toMatchObject({
      via: { kind: "connection", connectionId: hfWay().id },
    });

    // The next station is back on the block's connection with no second action,
    // and the STORE's stamp was never touched by the override.
    expect(store.getState().viaStamp).toEqual({
      kind: "connection",
      connectionId: echolinkWay().id,
    });
    await user.type(screen.getByLabelText(/callsign/i), "K2ABC");
    await user.keyboard("{Enter}");
    await waitFor(() => expect(fetchMock).toHaveBeenCalledTimes(2));
    const [, second] = fetchMock.mock.calls[1] as [string, RequestInit];
    expect(JSON.parse(second.body as string)).toMatchObject({
      callsign: "K2ABC",
      via: { kind: "connection", connectionId: echolinkWay().id },
    });
  });

  it("records a station on a way in the owner never listed, and creates nothing", async () => {
    const user = userEvent.setup();
    const store = seededStore(crossMode);
    const fetchMock = vi.fn().mockResolvedValue(jsonResponse(201, summary(crossMode)));
    vi.stubGlobal("fetch", fetchMock);

    render(<QuickAddRow sessionId="abc" store={store} />);

    await user.click(screen.getByRole("radio", { name: /other/i }));
    await user.type(screen.getByRole("textbox", { name: /way in/i }), "Bob's hotspot");

    expect(await commitAndReadBody(fetchMock, user)).toMatchObject({
      via: { kind: "unlisted", text: "Bob's hotspot" },
    });
    // Nothing was written back to the session's frozen snapshot — the net's
    // connection list stays the owner's.
    expect(store.getState().session.connections).toHaveLength(2);
  });

  it("cannot POST more than MAX_VIA_CHARS of free text", async () => {
    const user = userEvent.setup();
    const store = seededStore(crossMode);
    const fetchMock = vi.fn().mockResolvedValue(jsonResponse(201, summary(crossMode)));
    vi.stubGlobal("fetch", fetchMock);

    render(<QuickAddRow sessionId="abc" store={store} />);

    await user.click(screen.getByRole("radio", { name: /other/i }));
    await user.type(screen.getByRole("textbox", { name: /way in/i }), "x".repeat(80));

    const body = await commitAndReadBody(fetchMock, user);
    expect(body.via).toEqual({ kind: "unlisted", text: "x".repeat(MAX_VIA_CHARS) });
  });

  it("ships NO `via` key for a blank-after-trim free text, rather than losing the station", async () => {
    // `parse_via_text` treats blank-after-trim as an ERROR. `via` rides the
    // same POST as the callsign, so a 400 takes the station with it — at 20:07,
    // with someone waiting. The client makes that unreachable by construction.
    const user = userEvent.setup();
    const store = seededStore(crossMode);
    const fetchMock = vi.fn().mockResolvedValue(jsonResponse(201, summary(crossMode)));
    vi.stubGlobal("fetch", fetchMock);

    render(<QuickAddRow sessionId="abc" store={store} />);

    await user.click(screen.getByRole("radio", { name: /other/i }));
    await user.type(screen.getByRole("textbox", { name: /way in/i }), "   ");

    const body = await commitAndReadBody(fetchMock, user);
    expect(Object.hasOwn(body, "via")).toBe(false);
  });

  it("does not POST a stamp the session's connection set no longer resolves", async () => {
    const user = userEvent.setup();
    const store = seededStore(crossMode);
    store.getState().setViaStamp({
      kind: "connection",
      connectionId: "00000000-0000-0000-0000-0000000000ff",
    });
    const fetchMock = vi.fn().mockResolvedValue(jsonResponse(201, summary(crossMode)));
    vi.stubGlobal("fetch", fetchMock);

    render(<QuickAddRow sessionId="abc" store={store} />);

    const body = await commitAndReadBody(fetchMock, user);
    expect(Object.hasOwn(body, "via")).toBe(false);
  });

  it("surfaces the server's own refusal of a `via` through the existing alert line", async () => {
    // Client validation is UX-only and never authoritative — the
    // server's 400 still has to render.
    const user = userEvent.setup();
    const store = seededStore(crossMode);
    const fetchMock = vi
      .fn()
      .mockResolvedValue(jsonResponse(400, { type: "/errors/via-invalid", status: 400 }));
    vi.stubGlobal("fetch", fetchMock);

    render(<QuickAddRow sessionId="abc" store={store} />);

    await user.type(screen.getByLabelText(/callsign/i), "N1CCK");
    await user.keyboard("{Enter}");

    const alert = await screen.findByRole("alert");
    expect(alert).toHaveTextContent(messageForProblemType("/errors/via-invalid"));
    // The optimistic row was rolled back — the station is not left dimmed.
    expect(store.getState().pending.size).toBe(0);
  });

  it("keeps the override reachable by keyboard, as ONE tab stop after the Save buttons", async () => {
    // The reach is deliberately adequate rather than optimal: it is the
    // exception path, "one TAP" is the criterion's own word, and a shorter
    // reach costs either the pinned tab order or a third hotkey, and there are
    // exactly two.
    const user = userEvent.setup();
    const store = seededStore(crossMode);
    vi.stubGlobal("fetch", vi.fn());

    render(<QuickAddRow sessionId="abc" store={store} />);

    const group = screen.getByRole("radiogroup", { name: /way in/i });
    const reachable = within(group)
      .getAllByRole("radio")
      .filter((radio) => radio.tabIndex === 0);
    expect(reachable).toHaveLength(1);

    screen.getByRole("button", { name: /save & close/i }).focus();
    for (let i = 0; i < 6; i += 1) {
      if (document.activeElement === reachable[0]) {
        break;
      }
      await user.tab();
    }
    expect(document.activeElement).toBe(reachable[0]);
  });

  it("paints the optimistic row carrying the way in, BEFORE the echo", async () => {
    // The commit is held mid-flight, so nothing authoritative has arrived: the
    // dimmed row is showing the operator their own choice, which is the whole
    // point of the surface.
    const user = userEvent.setup();
    const store = seededStore(crossMode);
    store.getState().setViaStamp({ kind: "connection", connectionId: echolinkWay().id });
    const held = deferred<Response>();
    vi.stubGlobal("fetch", vi.fn().mockReturnValue(held.promise));

    render(<QuickAddRow sessionId="abc" store={store} />);

    await user.type(screen.getByLabelText(/callsign/i), "N1CCK");
    await user.keyboard("{Enter}");

    await waitFor(() => expect(store.getState().pending.size).toBe(1));
    const [row] = selectRoster(store.getState()).filter((entry) => entry.optimistic);
    expect(row.viaLabel).toBe("EchoLink — N1CCK-R");
  });

  it("adds no third hotkey — there are exactly two, `n` and `w`", async () => {
    const user = userEvent.setup();
    const store = seededStore(crossMode);
    store.getState().setViaStamp({ kind: "connection", connectionId: echolinkWay().id });
    vi.stubGlobal("fetch", vi.fn());

    render(<QuickAddRow sessionId="abc" store={store} />);
    document.body.focus();

    await user.keyboard("v");

    // `v` changes nothing: not the stamp, not the override, not focus.
    expect(store.getState().viaStamp).toEqual({
      kind: "connection",
      connectionId: echolinkWay().id,
    });
    const group = screen.getByRole("radiogroup", { name: /way in/i });
    expect(within(group).getAllByRole("radio").some((r) => r === document.activeElement)).toBe(
      false,
    );
  });
});

describe("QuickAddRow — the relaying station", () => {
  /** Types a callsign plus whatever `fill` adds, commits, and returns the store
   * and the parsed POST body. */
  async function commitWith(
    fill: (user: ReturnType<typeof userEvent.setup>) => Promise<void>,
  ): Promise<{
    store: ReturnType<typeof createSessionStore>;
    body: Record<string, unknown>;
  }> {
    const user = userEvent.setup();
    const store = createSessionStore();
    store.getState().seedFromSnapshot(summary());
    // A FRESH Response per call: a `Response` body can only be read once, and
    // the quick-add also fetches the roster-memory prefill.
    const fetchMock = vi.fn().mockImplementation(() =>
      Promise.resolve(
        new Response(JSON.stringify(summary()), {
          status: 201,
          headers: { "content-type": "application/json" },
        }),
      ),
    );
    vi.stubGlobal("fetch", fetchMock);
    render(<QuickAddRow sessionId="abc" store={store} />);
    await user.type(screen.getByLabelText(/callsign/i), "K9XYZ");
    await fill(user);
    await user.click(screen.getByRole("button", { name: /save & add new/i }));
    // The quick-add also GETs the roster-memory prefill, so the POST is not
    // necessarily call zero — find it by method rather than by position.
    await waitFor(() =>
      expect(
        fetchMock.mock.calls.some(([, init]) => (init as RequestInit | undefined)?.method === "POST"),
      ).toBe(true),
    );
    const post = fetchMock.mock.calls.find(
      ([, init]) => (init as RequestInit | undefined)?.method === "POST",
    ) as [string, RequestInit];
    return { store, body: JSON.parse(post[1].body as string) as Record<string, unknown> };
  }

  it("POSTs the operator's typed relaying station as `relayedBy`", async () => {
    const { store, body } = await commitWith(async (user) => {
      await user.type(screen.getByLabelText(/relayed by/i), "w3rel");
    });
    expect(body.relayedBy).toBe("w3rel");
    // The optimistic row carries it too, so the dimmed row shows what the
    // operator typed instead of waiting for the echo to confirm it.
    const [pending] = selectRoster(store.getState()).filter((entry) => entry.optimistic);
    expect(pending?.relayedBy).toBe("W3REL");
  });

  it("omits the `relayedBy` KEY ENTIRELY when the operator typed nothing", async () => {
    const { body } = await commitWith(async () => {});
    expect(Object.hasOwn(body, "relayedBy")).toBe(false);
  });

  it("ships NO `relayedBy` key for an implausible callsign rather than losing the station", async () => {
    // sharpest finding: `relayedBy` rides the SAME POST as the
    // callsign, so a server 400 takes the whole check-in down with it. The
    // client refuses to send what the shipped guard cannot pass.
    const { body } = await commitWith(async (user) => {
      await user.type(screen.getByLabelText(/relayed by/i), "not a callsign");
    });
    expect(Object.hasOwn(body, "relayedBy")).toBe(false);
    expect(body.callsign).toBe("K9XYZ");
  });

  it("clears the relaying station after a commit — the next station is a different one", async () => {
    // A sticky relay stamp would put a confidently-wrong third-party callsign on
    // every subsequent entry, which is strictly worse than a blank. Relay has no
    // BLOCK case the way `via` does.
    await commitWith(async (user) => {
      await user.type(screen.getByLabelText(/relayed by/i), "W3REL");
    });
    await waitFor(() =>
      expect((screen.getByLabelText(/relayed by/i) as HTMLInputElement).value).toBe(""),
    );
  });
});
