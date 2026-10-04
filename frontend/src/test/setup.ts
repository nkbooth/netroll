// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
// Vitest setup: registers @testing-library/jest-dom matchers on vitest's
// expect for every suite that runs in the jsdom environment, and RTL cleanup
// (auto-cleanup needs vitest globals, which this project keeps disabled).
import "@testing-library/jest-dom/vitest";

import { cleanup } from "@testing-library/react";
import { afterEach } from "vitest";

// Node 22+ defines its own `localStorage`/`sessionStorage` globals, left
// `undefined` unless the process is started with --localstorage-file. Vitest's
// jsdom environment copies window properties onto the global ONLY for names not
// already present there (plus its own allowlist, which omits web storage), so
// Node's undefined globals shadow jsdom's working ones and every suite touching
// theme persistence dies in beforeEach. Re-point them at the jsdom window
// vitest parks on `globalThis.jsdom`. src/test/dom.test.ts guards this.
const jsdomWindow = (globalThis as { jsdom?: { window: Window } }).jsdom?.window;
if (jsdomWindow) {
  for (const area of ["localStorage", "sessionStorage"] as const) {
    if (globalThis[area] === undefined) {
      Object.defineProperty(globalThis, area, {
        value: jsdomWindow[area],
        configurable: true,
        writable: true,
      });
    }
  }
}

// jsdom ships no matchMedia; primitives that read it (reduced-motion via
// motion.ts, the responsive breakpoint hook) need a default. Individual
// suites override this via vi.stubGlobal to drive a specific query result.
if (typeof window !== "undefined" && !window.matchMedia) {
  window.matchMedia = (query: string): MediaQueryList =>
    ({
      matches: false,
      media: query,
      onchange: null,
      addEventListener: () => {},
      removeEventListener: () => {},
      addListener: () => {},
      removeListener: () => {},
      dispatchEvent: () => false,
    }) as unknown as MediaQueryList;
}

afterEach(() => {
  cleanup();
});
