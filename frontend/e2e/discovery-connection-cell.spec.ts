// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
import { expect, test } from "@playwright/test";
import type { Route } from "@playwright/test";

/**
 * The LAYOUT proof for the discovery connection cell — the one property the
 * gated suite cannot see. `npm test` is vitest + jsdom, and jsdom implements
 * no layout engine: every `scrollWidth`, `clientWidth` and
 * `getBoundingClientRect()` there reads zero. The jsdom tests in
 * `DiscoveryPage.test.tsx` assert the MECHANISM
 * (`nowrap` is off, the lines are separate elements, the cap holds, the
 * affordance points at the net); this spec asserts the OUTCOME.
 *
 * ⚠️ `boundingBox().width` IS NOT THE ASSERTION, and that is measured rather
 * than assumed: it reads 150px on the broken code and 150px on the fixed code,
 * because the pill is a grid item whose BORDER BOX is sized to the track while
 * its CONTENT overflows. A spec built on it passes on the very bug it exists to
 * catch. The two that discriminate are content overflow
 * (`scrollWidth > clientWidth`) and the widest painted descendant's `right`
 * against the cell's own — and against the next column's `left`, which is the
 * claim that matters.
 *
 * Route-mocked at the browser boundary the way `reconnect.spec.ts` mocks its
 * transport: no Rust backend, no Postgres, no auth.
 */

const connection = (overrides: Record<string, unknown>) => ({
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
});

// Internet-only: no `hf`, no `repeater`, so the summary takes the unbounded
// branch. An RF net is one line before AND after the fix and proves nothing.
const FOUR_WAYS = [
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

const upcomingNet = {
  id: "def-e2e",
  definitionVersion: 1,
  occurrenceId: "occ-e2e",
  scheduledStartAt: "2099-01-01T20:00:00+00:00",
  title: "Internet Only Net",
  description: null,
  country: null,
  state: null,
  grid: null,
  netCategory: "traffic",
  netType: "open",
  expectedDurationMinutes: null,
  linkToken: "tok-internet",
  matchedConnectionId: null,
  connections: FOUR_WAYS,
};

test.use({ viewport: { width: 1280, height: 900 } });

test("the connection cell stays inside its column on a multi-connection net", async ({
  page,
}) => {
  await page.route(/\/api\/discovery(\?.*)?$/, (route: Route) =>
    route.fulfill({
      status: 200,
      contentType: "application/json",
      body: JSON.stringify({
        activeNow: [],
        upcoming: [upcomingNet],
        applied: { sort: "time" },
      }),
    }),
  );

  await page.goto("/");
  const row = page.getByRole("listitem").filter({ hasText: "Internet Only Net" });
  const cell = row.getByTestId("connection-cell");
  await expect(cell).toBeVisible();

  // 1. The content fits the box it is painted in. `scrollWidth > clientWidth`
  //    is the direct statement of "this element's content overflows"; it read
  //    568 > 148 on the pre-fix configuration.
  const overflow = await cell.evaluate((el) => ({
    scrollWidth: el.scrollWidth,
    clientWidth: el.clientWidth,
  }));
  expect(overflow.scrollWidth).toBeLessThanOrEqual(overflow.clientWidth);

  // 2. No painted descendant reaches past the cell's own right edge. This is
  //    the assertion `boundingBox().width` cannot make: the border box is
  //    sized to the track either way, so only the DESCENDANTS say whether ink
  //    left the column. It read +419.3px on the pre-fix configuration.
  const ink = await cell.evaluate((el) => {
    const own = el.getBoundingClientRect();
    const widest = Array.from(el.querySelectorAll("*")).reduce(
      (max, child) => Math.max(max, child.getBoundingClientRect().right),
      own.left,
    );
    return { widest, cellRight: own.right };
  });
  expect(ink.widest).toBeLessThanOrEqual(ink.cellRight + 1);

  // 3. And the claim that matters: the ways-in cell does not
  //    run through its neighbour. The pre-fix ink ran 407.3px into "Starts".
  const timeLeft = await row
    .locator("time")
    .evaluate((el) => el.getBoundingClientRect().left);
  expect(ink.widest).toBeLessThanOrEqual(timeLeft);

  // 4. The row GREW to hold the wrapped lines rather than being clipped — the
  //    row already grows, in a real layout engine. A clamp that
  //    hid the overflow would satisfy 1-3 and fail here.
  const rowHeight = await row.evaluate((el) => el.getBoundingClientRect().height);
  expect(rowHeight).toBeGreaterThan(60);
});
