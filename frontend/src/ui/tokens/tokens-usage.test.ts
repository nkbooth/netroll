// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
import { readFileSync, readdirSync } from "node:fs";
import { dirname, join, resolve } from "node:path";

import { describe, expect, it } from "vitest";

/**
 * Guards against referencing a custom property nothing declares. A
 * `var(--fs-meta, 13px)` looks like a token read but silently resolves to its
 * fallback forever, so the "token" never tracks the theme and its value drifts
 * from the DESIGN.md ramp with nothing failing. No component declares custom
 * properties of its own (they all come from the stylesheets), so every name
 * used in source must be declared in CSS.
 */

const sourceRoot = (): string => {
  const testPath = expect.getState().testPath;
  if (!testPath) {
    throw new Error("vitest testPath unavailable — cannot locate src/");
  }
  // …/src/ui/tokens/this-file → …/src
  return resolve(dirname(testPath), "..", "..");
};

const walk = (dir: string): readonly string[] =>
  readdirSync(dir, { withFileTypes: true }).flatMap((entry) => {
    const path = join(dir, entry.name);
    return entry.isDirectory() ? walk(path) : [path];
  });

const isSource = (path: string): boolean =>
  /\.(tsx?|css)$/.test(path) && !/\.test\.tsx?$/.test(path);

const VAR_REFERENCE = /var\(\s*(--[a-zA-Z0-9-]+)/g;
const VAR_DECLARATION = /(--[a-zA-Z0-9-]+)\s*:/g;

const matchAll = (source: string, pattern: RegExp): readonly string[] =>
  [...source.matchAll(pattern)].map((match) => match[1]);

describe("custom-property usage", () => {
  const files = walk(sourceRoot()).filter(isSource);

  it("scans a non-trivial slice of the app", () => {
    // A broken walk would make every assertion below vacuously pass.
    expect(files.length).toBeGreaterThan(50);
  });

  it("declares every custom property the app reads", () => {
    const declared = new Set(
      files
        .filter((path) => path.endsWith(".css"))
        .flatMap((path) => matchAll(readFileSync(path, "utf8"), VAR_DECLARATION)),
    );

    const undeclared = new Map<string, string[]>();
    for (const path of files) {
      for (const name of matchAll(readFileSync(path, "utf8"), VAR_REFERENCE)) {
        if (!declared.has(name)) {
          undeclared.set(name, [
            ...(undeclared.get(name) ?? []),
            path.slice(sourceRoot().length + 1),
          ]);
        }
      }
    }

    expect(Object.fromEntries(undeclared)).toEqual({});
  });
});
