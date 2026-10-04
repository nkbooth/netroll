// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
import { readFileSync } from "node:fs";
import { dirname, resolve } from "node:path";

import { describe, expect, it } from "vitest";

import { REDUCED_MOTION_DURATION } from "./motion";
import { cssVariables } from "./tokens";

// jsdom cannot cascade custom properties through getComputedStyle, so the
// stylesheet is welded to the TS source of truth by raw-text assertion:
// every variable cssVariables() emits must appear verbatim in the matching
// theme block. CSS and TS cannot drift. (Read via node:fs — vitest stubs CSS
// imports to empty modules even with a ?raw query, and import.meta.url is not
// a file: URL under the jsdom environment — so anchor on the test file's own
// path via expect.getState(); a bare relative path would break whenever
// vitest is launched from any CWD other than frontend/.)
let cachedStylesheet: string | undefined;
const stylesheet = (): string => {
  if (cachedStylesheet === undefined) {
    const testPath = expect.getState().testPath;
    if (!testPath) {
      throw new Error("vitest testPath unavailable — cannot locate tokens.css");
    }
    cachedStylesheet = readFileSync(resolve(dirname(testPath), "tokens.css"), "utf8");
  }
  return cachedStylesheet;
};

const themeBlock = (theme: "dark" | "light"): string => {
  const source = stylesheet();
  const selector = `:root[data-theme='${theme}']`;
  const start = source.indexOf(selector);

  expect(start, `selector ${selector} present`).toBeGreaterThanOrEqual(0);

  const open = source.indexOf("{", start);
  const close = source.indexOf("}", open);
  const block = source.slice(open + 1, close);

  // A nested rule would make the first "}" close the inner rule and silently
  // truncate the block, weakening every assertion below — fail loudly instead.
  expect(block, `${selector} block must not contain nested rules`).not.toContain(
    "{",
  );

  return block;
};

describe("tokens.css mirrors tokens.ts", () => {
  it("declares every dark variable in the dark theme block", () => {
    const block = themeBlock("dark");

    for (const [name, value] of Object.entries(cssVariables("dark"))) {
      expect(block, `${name} in dark block`).toContain(`${name}: ${value};`);
    }
  });

  it("declares every light variable in the light theme block", () => {
    const block = themeBlock("light");

    for (const [name, value] of Object.entries(cssVariables("light"))) {
      expect(block, `${name} in light block`).toContain(`${name}: ${value};`);
    }
  });

  it("lets native form controls follow the theme via color-scheme", () => {
    expect(themeBlock("dark")).toContain("color-scheme: dark;");
    expect(themeBlock("light")).toContain("color-scheme: light;");
  });

  it("falls back to the dark tokens when data-theme is missing or garbage", () => {
    // Bare :root must carry the dark (default) block: if data-theme is ever
    // absent or unrecognized, neither attribute selector matches and every
    // var() consumer would otherwise resolve to unset — a fully unstyled app.
    expect(stylesheet()).toMatch(/:root,\s*:root\[data-theme='dark'\]\s*\{/);
  });

  it("zeroes the motion budget under prefers-reduced-motion", () => {
    const source = stylesheet();
    const media = "@media (prefers-reduced-motion: reduce)";
    const start = source.indexOf(media);

    expect(start, `${media} present`).toBeGreaterThanOrEqual(0);

    const override = source.slice(start);

    // Welded to motion.ts: the CSS zeros and motionDuration()'s reduced-motion
    // return value must stay the same representation, not 0ms-vs-0s cousins.
    expect(override).toContain(`--motion-fast: ${REDUCED_MOTION_DURATION};`);
    expect(override).toContain(`--motion-live-pulse: ${REDUCED_MOTION_DURATION};`);
    expect(override).toContain(`--motion-cursor-wash: ${REDUCED_MOTION_DURATION};`);
  });
});
