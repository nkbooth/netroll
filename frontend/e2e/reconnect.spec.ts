// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
import { expect, test } from "@playwright/test";
import type { Route } from "@playwright/test";

/**
 * Browser reconnect/real-time E2E. Drives the REAL client in a real chromium:
 * it loads the live-session page, confirms `live`,
 * severs the socket, appends events during the gap, and asserts the browser's
 * recovered roster equals the authoritative fold with `ConnectionStatus`
 * transitioning live → catching-up → live and NO manual refresh.
 *
 * The transport is mocked at the browser boundary (Playwright WebSocket + HTTP
 * route interception) so the choreography is deterministic. The full
 * Rust-backend + Postgres + auth stack behind the same spec is the documented
 * follow-up.
 */

const SESSION_ID = "00000000-0000-0000-0000-0000000000e2";

const definition = {
  title: "Sunday Traffic Net",
  plannedFrequencyHz: 14_230_000,
  band: "20m",
  mode: "ssb",
  netCategory: "traffic",
  netType: "open",
};

function summary(latestSeq: number, roster: unknown[]) {
  return {
    id: SESSION_ID,
    definitionId: "00000000-0000-0000-0000-000000000007",
    definitionVersion: 3,
    lifecycle: "live",
    operatingFrequencyHz: 14_250_000,
    startedAt: "2026-07-16T00:00:00Z",
    closedAt: null,
    durationSeconds: null,
    latestSeq,
    participantCount: roster.length,
    roster,
    definition,
  };
}

function checkinFrame(seq: number, id: string, callsign: string) {
  return {
    type: "event",
    seq,
    kind: "checkin.added",
    at: "2026-07-16T00:00:05Z",
    payload: { checkInId: id, callsign },
  };
}

const W1AW = "00000000-0000-0000-0000-00000000002a";
const N1CCK = "00000000-0000-0000-0000-00000000002b";

test("recovers roster and connection state across a dropped socket, no refresh", async ({
  page,
}) => {
  // The HTTP self-gating snapshot: a live session with an empty roster.
  await page.route(/\/api\/net-sessions\/[^/]+$/, (route: Route) =>
    route.fulfill({
      status: 200,
      contentType: "application/json",
      body: JSON.stringify(summary(1, [])),
    }),
  );

  // The HTTP catch-up gap: the two check-ins that landed during the outage —
  // this is the server's authoritative fold the recovered client must match.
  await page.route(/\/api\/net-sessions\/[^/]+\/events/, (route: Route) =>
    route.fulfill({
      status: 200,
      contentType: "application/json",
      body: JSON.stringify([
        checkinFrame(2, W1AW, "W1AW"),
        checkinFrame(3, N1CCK, "N1CCK"),
      ]),
    }),
  );

  // The mocked WS transport. The fresh socket seeds via a snapshot then goes
  // live; we keep a handle to sever it. The resume socket (?since=3) opens
  // already caught-up.
  const freshSockets: Array<{ close: () => void }> = [];
  await page.routeWebSocket(/\/api\/net-sessions\/[^/]+\/ws/, (ws) => {
    const url = ws.url();
    if (url.includes("since=")) {
      // Resume socket: no snapshot; the client goes live on open.
      return;
    }
    // Fresh socket: seed with a snapshot; the client transitions to live.
    ws.send(JSON.stringify({ type: "snapshot", session: summary(1, []) }));
    freshSockets.push({ close: () => ws.close() });
  });

  await page.goto(`/net-sessions/${SESSION_ID}`);

  const status = page.getByTestId("connection-status");
  // The live stream confirmed via the WS snapshot.
  await expect(status).toHaveAttribute("data-state", "live");

  // Sever the socket mid-session.
  expect(freshSockets.length).toBeGreaterThan(0);
  freshSockets[0].close();

  // The connection status actually transitions THROUGH catching-up, not
  // just live-before/live-after — a regression that skipped the intermediate
  // state entirely must fail this.
  await expect(status).toHaveAttribute("data-state", "catching-up");

  // The client recovers on its own: HTTP catch-up → resume WS → live. The
  // recovered roster equals the authoritative fold (both check-ins, in order).
  await expect(status).toHaveAttribute("data-state", "live");
  // Each row also carries the operator console's source badge/staying/
  // precedence chips plus a report cell that renders an em-dash
  // when unreported, all in the same mono face — so assert on the callsign
  // cell by name rather than on `.mono` or the row's full text
  // (LiveSessionPage.test.tsx does the same).
  const callsigns = page.getByRole("listitem").locator("[data-callsign]");
  await expect(callsigns).toHaveText(["W1AW", "N1CCK"]);

  // Never a manual-refresh prompt in any connection state.
  await expect(page.getByText(/refresh/i)).toHaveCount(0);
});
