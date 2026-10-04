// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
/// <reference types="vitest/config" />
import { defineConfig } from 'vite'
import react from '@vitejs/plugin-react'

// https://vite.dev/config/
export default defineConfig({
  plugins: [react()],
  server: {
    // Dev-only: same-origin /api calls flow to the Rust app, so the session
    // cookie works without CORS. `ws: true` upgrades the live-session
    // WebSocket (`/api/net-sessions/{id}/ws`) through the proxy;
    // the string shorthand does NOT proxy WS.
    //
    // Dev Origin note: the backend refuses a browser WS upgrade whose
    // `Origin` header does not case-insensitively equal `PUBLIC_BASE_URL`
    // (ws/mod.rs origin check). The proxy forwards the browser's Origin (the
    // Vite dev origin) unchanged, so in dev run the backend with
    // `PUBLIC_BASE_URL` set to the Vite dev origin (default
    // `http://localhost:5173`). Do NOT disable the check. In production and
    // in the E2E harness the Rust binary serves the built SPA on ONE origin
    // so Origin == PUBLIC_BASE_URL holds naturally and no proxy is in play.
    proxy: {
      '/api': { target: 'http://localhost:3000', ws: true },
      // Uploaded avatars are served by the Rust binary from AVATAR_DIR. Without
      // this the dev server answers `/avatars/*` with the SPA shell (its own
      // history fallback), so every uploaded avatar renders as a broken image
      // in dev while working fine in production — where one origin serves both.
      '/avatars': { target: 'http://localhost:3000' },
    },
  },
  test: {
    environment: 'jsdom',
    setupFiles: ['./src/test/setup.ts'],
    // Keep the Playwright E2E lane (frontend/e2e/*.spec.ts) out of the Vitest
    // glob so `npm test` and `npx playwright test` never collide.
    exclude: ['**/node_modules/**', '**/dist/**', 'e2e/**'],
  },
})
