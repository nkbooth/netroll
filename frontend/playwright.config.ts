// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
import { defineConfig, devices } from "@playwright/test";

/**
 * Playwright config for NetRoll's end-to-end lane. E2E
 * specs live in `frontend/e2e/` — kept out of the Vitest glob (see
 * `vite.config.ts` `test.exclude`) so `npm test` and `npx playwright test` never
 * collide.
 *
 * The suite drives the REAL browser client (real chromium, real WebSocket API,
 * the real fold/store/reconnect controller, real DOM) against the built SPA
 * served by `vite preview`. The transport is driven at the browser boundary via
 * Playwright's WebSocket + HTTP route mocking so the reconnect choreography is
 * exercised deterministically. Standing the full Rust-backend + Postgres +
 * magic-link-auth stack behind it (the server's own authoritative fold) is the
 * documented follow-up.
 */
export default defineConfig({
  testDir: "./e2e",
  fullyParallel: true,
  forbidOnly: !!process.env.CI,
  retries: process.env.CI ? 1 : 0,
  reporter: process.env.CI ? "list" : "line",
  timeout: 30_000,
  use: {
    baseURL: "http://localhost:4173",
    trace: "on-first-retry",
  },
  projects: [{ name: "chromium", use: { ...devices["Desktop Chrome"] } }],
  webServer: {
    command: "npm run build && npm run preview -- --port 4173 --strictPort",
    url: "http://localhost:4173",
    reuseExistingServer: !process.env.CI,
    timeout: 120_000,
  },
});
