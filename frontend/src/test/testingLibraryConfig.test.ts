// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
import { getConfig } from "@testing-library/react";
import { expect, it } from "vitest";

/**
 * Containment probe for `configure()` calls made by individual test files.
 *
 * `NetDefinitionFormPage.test.tsx` raises `asyncUtilTimeout` to 5 000 ms for
 * itself. Unlike `vi.setConfig`, which vitest scopes and restores per file,
 * RTL's `configure` writes to module-global `@testing-library/dom` state — so
 * that raise is contained by ONE thing only: vitest giving every test file its
 * own module registry. `vite.config.ts` sets neither `isolate` nor `pool`, so
 * the default `forks` + `isolate: true` provides that today.
 *
 * The day someone sets `isolate: false` (or `--no-isolate`, or a shared-context
 * pool) for speed, a 5 s async budget silently applies to the whole suite and
 * every `waitFor`-shaped race elsewhere gets five times as long to look healthy.
 * That is a coverage loss no other test would notice, so this assertion exists
 * to turn it into a red test instead. Verified to fire: under
 * `npx vitest run --no-isolate --no-file-parallelism` alongside
 * `NetDefinitionFormPage.test.tsx` it fails with `expected 5000 to be 1000`.
 *
 * Deliberately NOT fixed with `afterAll(() => configure({ asyncUtilTimeout:
 * 1000 }))` in the raising file: that would contain the leak and thereby delete
 * the signal, leaving the suite quietly dependent on an unpinned default with
 * nothing left to say so.
 *
 * Honest limit: under `--no-isolate` this only fails if the raising file runs
 * FIRST. Vitest's default file order is by descending size and
 * `NetDefinitionFormPage.test.tsx` is one of the largest files in the suite
 * while this one is among the smallest, so it does in practice — but a
 * `sequence.shuffle` or an explicit order could hide it.
 */
it("leaves @testing-library/dom's asyncUtilTimeout at its 1000 ms default", () => {
  expect(getConfig().asyncUtilTimeout).toBe(1000);
});
