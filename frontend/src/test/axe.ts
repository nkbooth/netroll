// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
import type { AxeResults, Result } from "axe-core";
import { axe } from "vitest-axe";
import { expect } from "vitest";

/**
 * Runs axe against a rendered subtree and asserts zero WCAG 2.1 AA
 * violations. Scoped to the passed container (not the whole document) and
 * pinned to the WCAG A/AA tag set — the build-gated accessibility floor.
 * Do not silence load-bearing rules to pass; fix the surface.
 *
 * Asserts on `results.violations` directly rather than via vitest-axe's
 * custom matcher: that matcher's 0.1.0 typings don't resolve as values under
 * this repo's `verbatimModuleSyntax`. The assertion below is equivalent and
 * ships a readable per-rule failure message.
 *
 * `resultTypes: ["violations"]` is a
 * reporting-side narrowing, NOT a coverage one: every rule still runs and every
 * violation is still processed in full — axe only stops materialising the
 * per-node detail for the `passes`/`incomplete` buckets, which this assertion
 * never reads. **36 test files call this helper**, and that cost was the direct
 * cause of the full-suite timeout flake. (Measured alternative,
 * rejected: disabling `color-contrast` — which reports zero violations, zero
 * incomplete AND zero passes under jsdom — saved nothing, so the rule stays
 * enabled.)
 *
 * The size of the win is roughly **an order of magnitude (~9×)** on the largest
 * surface in the app; treat that ratio as the claim and any absolute figure as a
 * host-dependent snapshot. Re-measured 2026-08-26 on an idle host with
 * `npx vitest run --reporter=verbose … -t "has no WCAG 2.1 AA violations on the
 * edit-mode form"`: **17 475 ms without this option, 1 888 ms with it.** Earlier
 * records of the same change quote 1.7 s and 1.1 s. The 1.1 s figure could not
 * be reproduced here and its measurement conditions are not recorded, so it is
 * reported as a disagreement rather than reconciled. Nothing about behaviour
 * differs between those runs — re-measure rather than trusting any of the
 * numbers.
 */
export async function expectNoAxeViolations(container: Element): Promise<void> {
  const results: AxeResults = await axe(container, {
    runOnly: {
      type: "tag",
      values: ["wcag2a", "wcag2aa", "wcag21a", "wcag21aa"],
    },
    resultTypes: ["violations"],
  });

  expect(results.violations, formatViolations(results.violations)).toHaveLength(
    0,
  );
}

function formatViolations(violations: readonly Result[]): string {
  if (violations.length === 0) {
    return "No accessibility violations";
  }
  const lines = violations.map(
    (violation) =>
      `- ${violation.id}: ${violation.help} (${violation.nodes.length} node(s)) ${violation.helpUrl}`,
  );
  return `Expected no WCAG 2.1 AA violations, found ${violations.length}:\n${lines.join("\n")}`;
}
