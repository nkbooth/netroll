// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
import { readFileSync } from "node:fs";
import { dirname, resolve } from "node:path";

import { describe, expect, it } from "vitest";

// The two palette projections of tokens.css that live OUTSIDE the SPA, welded
// to their source the way tokens-css.test.ts welds tokens.css to tokens.ts.
//
// 1. docs/assets/netroll.css — a HAND-MAINTAINED stylesheet. Its only
// consumer is `extra_css` in mkdocs.yml. Before this test it had a prose
// comment and nothing else, and all 21 of its letter-bearing hexes had
// drifted to lowercase. This test is the ONLY guard on that file.
//
// 2. The MJML-generated email HTML. Its palette is not a copy at all —
// tools/email/build.mjs resolves every `$NR_*$` placeholder out of
// tokens.css at build time and records what it used in palette.lock.json.
// This test is a SECOND net under the byte-exact regeneration check in CI
// (ci.yml, `frontend` job): it catches a hand-edit to the shipped HTML and
// a stale palette.lock.json, both of which the regeneration check also
// catches, and it names the palette when it fails rather than pointing at
// a diff. It asserts the OCCURRENCE COUNT of each value, not merely its
// presence, so repainting one element by hand reds here.
//
// WHAT THIS TEST DOES NOT CATCH, stated plainly so nobody plans against more:
//
// - A hex that MATCHES but is used in the WRONG PLACE. Painting the email's
// body background with `--text` is invisible here: the value is a real
// tokens.css value and it does appear in the artifact. Nothing in this
// repo checks role-correctness; only a human looking at the mail does.
// - A tokens.css variable the email or the docs SHOULD use but does not.
// Coverage is driven by what each consumer already declares.
// - Anything about the LIGHT email palette. Email renders the product's
// default dark theme and has no toggle, so only the dark block is bound.
// - Colours MJML emits on its own (`#000000`, `#ffffff` in its resets). They
// are not palette values and are not asserted.
// - The `--nr-code-bg -> --surface-2` alias below, which is hand-maintained
// here. A renamed alias is a test edit, not a red.

const repoRoot = (): string => {
  const testPath = expect.getState().testPath;
  if (!testPath) {
    throw new Error("vitest testPath unavailable — cannot locate the repo root");
  }
  // <repo>/frontend/src/ui/tokens/<this file>
  return resolve(dirname(testPath), "..", "..", "..", "..");
};

const read = (...parts: string[]): string =>
  readFileSync(resolve(repoRoot(), ...parts), "utf8");

/** The `--name: value` pairs declared in one CSS rule block, by selector. */
const declarations = (
  source: string,
  selector: string,
  label: string,
): Map<string, string> => {
  const start = source.indexOf(selector);
  expect(start, `${label}: selector ${selector} present`).toBeGreaterThanOrEqual(0);

  const open = source.indexOf("{", start);
  const close = source.indexOf("}", open);
  const block = source.slice(open + 1, close);

  // Same guard tokens-css.test.ts uses: a nested rule would make the first "}"
  // truncate the block and silently weaken every assertion below.
  expect(block, `${label}: ${selector} must not contain nested rules`).not.toContain("{");

  const pairs = new Map<string, string>();
  for (const [, name, value] of block.matchAll(/(--[a-z0-9-]+)\s*:\s*([^;]+);/g)) {
    pairs.set(name, value.trim());
  }
  expect(pairs.size, `${label}: ${selector} declares variables`).toBeGreaterThan(0);
  return pairs;
};

const tokens = (theme: "dark" | "light"): Map<string, string> =>
  declarations(
    read("frontend", "src", "ui", "tokens", "tokens.css"),
    `:root[data-theme='${theme}']`,
    "tokens.css",
  );

describe("docs/assets/netroll.css is a projection of tokens.css", () => {
  // netroll.css strips the `--nr-` prefix off tokens.css's names, except for
  // this one alias.
  const ALIASES: Record<string, string> = { "code-bg": "surface-2" };

  const cases: Array<[string, "dark" | "light", string]> = [
    ["dark", "dark", "[data-md-color-scheme='slate']"],
    ["light", "light", "[data-md-color-scheme='default']"],
  ];

  for (const [label, theme, selector] of cases) {
    it(`declares every ${label} value verbatim from tokens.css`, () => {
      const source = tokens(theme);
      const projection = declarations(
        read("docs", "assets", "netroll.css"),
        selector,
        "netroll.css",
      );

      for (const [name, value] of projection) {
        const bare = name.replace(/^--nr-/, "");
        const variable = `--${ALIASES[bare] ?? bare}`;
        const expected = source.get(variable);

        expect(
          expected,
          `${name} maps to ${variable}, which tokens.css's ${theme} block must declare`,
        ).toBeDefined();
        // Verbatim, not case-insensitively: "same colour, different bytes" is
        // precisely the drift this file had, and normalising it away here would
        // reintroduce it.
        expect(value, `${name} must equal tokens.css ${variable}`).toBe(expected);
      }
    });
  }
});

describe("the email palette is a projection of tokens.css", () => {
  /** One tokens.css variable as it reached a generated template. */
  interface Binding {
    /** The literal the build substituted, e.g. `#0D1117`. */
    value: string;
    /** How many times that literal occurs in the shipped template. */
    occurrences: number;
  }

  // Written by tools/email/build.mjs: per generated template, the tokens.css
  // variables whose values actually reached that template's output, and how
  // many times each one did.
  const manifest = (): Record<string, Record<string, Binding>> =>
    JSON.parse(read("tools", "email", "palette.lock.json")) as Record<
      string,
      Record<string, Binding>
    >;

  // The four kinds and the exact palette each one is expected to carry.
  // Hard-coded — NOT read from the lock file — because the lock is a build
  // OUTPUT: a regression that stopped six of a template's seven values reaching
  // the HTML would rewrite the lock to match itself and stay green against any
  // "at least one" floor. A template dropped entirely reds here too.
  const EXPECTED_BINDINGS: Record<string, string[]> = {
    "backend/crates/netroll-adapters/templates/magic_link.html": [
      "--accent-deep",
      "--accent-ink",
      "--bg",
      "--border",
      "--on-accent",
      "--surface",
      "--text",
      "--text-muted",
    ],
    "backend/crates/netroll-adapters/templates/email_change.html": [
      "--accent-deep",
      "--accent-ink",
      "--bg",
      "--border",
      "--on-accent",
      "--surface",
      "--text",
      "--text-muted",
    ],
    // No button in the notice mail, so no button palette: `--accent-deep` and
    // `--on-accent` are substituted into the shared _head.mjml and never reach
    // this template's output.
    "backend/crates/netroll-adapters/templates/email_change_notice.html": [
      "--accent-ink",
      "--bg",
      "--border",
      "--surface",
      "--text",
      "--text-muted",
    ],
    "backend/crates/netroll-app/templates/net_summary.html": [
      "--accent-ink",
      "--bg",
      "--border",
      "--surface",
      "--text",
      "--text-muted",
    ],
  };

  it("covers every generated template with the palette that template should use", () => {
    const actual = Object.fromEntries(
      Object.entries(manifest()).map(([template, bindings]) => [
        template,
        Object.keys(bindings).sort(),
      ]),
    );
    const expected = Object.fromEntries(
      Object.entries(EXPECTED_BINDINGS).map(([template, variables]) => [
        template,
        [...variables].sort(),
      ]),
    );
    expect(actual).toStrictEqual(expected);
  });

  it("resolved every email colour from tokens.css's dark block", () => {
    const source = tokens("dark");

    for (const [template, bindings] of Object.entries(manifest())) {
      for (const [variable, binding] of Object.entries(bindings)) {
        expect(
          source.get(variable),
          `${template}: ${variable} must equal tokens.css's dark value — regenerate with tools/email/verify-regeneration.sh`,
        ).toBe(binding.value);
      }
    }
  });

  it("baked those exact bytes into the generated template that ships", () => {
    // Guards the ARTIFACT, not only the manifest beside it, and guards it by
    // OCCURRENCE COUNT rather than containment. Containment only reds when
    // EVERY occurrence of a hex is changed at once; the realistic hand-edit —
    // repainting one element — leaves the other occurrences behind and passes.
    // The count is recorded by the build, so a single edited occurrence reds
    // here and names the template and the variable.
    for (const [template, bindings] of Object.entries(manifest())) {
      const html = read(...template.split("/"));

      for (const [variable, binding] of Object.entries(bindings)) {
        expect(binding.occurrences, `${template}: ${variable} is recorded as used`).toBeGreaterThan(
          0,
        );
        expect(
          html.split(binding.value).length - 1,
          `${template} must carry ${variable} as ${binding.value} exactly ${binding.occurrences} time(s) — regenerate with tools/email/verify-regeneration.sh`,
        ).toBe(binding.occurrences);
      }
    }
  });
});
