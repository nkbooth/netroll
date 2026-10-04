// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
import { readFileSync } from "node:fs";
import { dirname, resolve } from "node:path";

import { describe, expect, it } from "vitest";

import { THEME_STORAGE_KEY } from "./theme";

// The FOUC guard in public/theme-init.js cannot import theme.ts (it must run
// before any module loads), so it duplicates the storage key, the accepted
// values, and the dark default as literals. These raw-text welds make a rename
// in either place fail the suite instead of silently desyncing first paint
// from the theme manager.
const repoFile = (relative: string): string => {
  const testPath = expect.getState().testPath;
  if (!testPath) {
    throw new Error(`vitest testPath unavailable — cannot locate ${relative}`);
  }
  return readFileSync(resolve(dirname(testPath), "../..", relative), "utf8");
};

const themeInit = (): string => repoFile("public/theme-init.js");
const indexHtml = (): string => repoFile("index.html");

describe("theme-init.js FOUC guard stays welded to theme.ts", () => {
  it("reads the same storage key as the theme manager", () => {
    expect(themeInit()).toContain(
      `localStorage.getItem('${THEME_STORAGE_KEY}')`,
    );
  });

  it("accepts exactly the two first-class themes", () => {
    expect(themeInit()).toContain("stored === 'light' || stored === 'dark'");
  });

  it("defaults to dark and stamps data-theme on <html>", () => {
    const script = themeInit();

    expect(script).toContain("var theme = 'dark';");
    expect(script).toContain("document.documentElement.dataset.theme = theme;");
  });
});

describe("index.html keeps the guard external and render-blocking", () => {
  it("loads the guard from its own file", () => {
    expect(indexHtml()).toContain('<script src="/theme-init.js"></script>');
  });

  it("carries no inline script at all", () => {
    // The CSP served for this origin is `script-src 'self'` — no hash, no
    // 'unsafe-inline'. An inline script reintroduced here would be BLOCKED in
    // production, and silently: the app still self-corrects the theme on boot,
    // so only the pre-paint flash regresses and nobody reports it.
    const inlineScripts = [
      ...indexHtml().matchAll(/<script(?![^>]*\bsrc=)[^>]*>/g),
    ];

    expect(inlineScripts).toHaveLength(0);
  });

  it("runs the guard before first paint, not deferred", () => {
    // `defer`/`async` would let the body paint first, which is the exact flash
    // this guard exists to prevent.
    const tag = /<script[^>]*src="\/theme-init\.js"[^>]*>/.exec(indexHtml());

    expect(tag).not.toBeNull();
    expect(tag?.[0]).not.toMatch(/\b(defer|async)\b/);
  });
});
