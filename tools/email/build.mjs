// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
// Compiles NetRoll's transactional email MJML sources to the checked-in HTML
// the Rust mail templates embed: MJML at BUILD time, output committed,
// interpolated at runtime by askama, so no Node reaches the runtime image.
//
// Two invariants this script exists to hold:
//
//  1. Regeneration is a NO-OP. Output is deterministic: LF endings, one trailing
//     newline, no minification, no timestamps. CI runs this and then
//     `git diff --exit-code` (ci.yml, `frontend` job) — a byte of drift fails
//     the merge. See tools/email/verify-regeneration.sh, which CI invokes
//     verbatim so the local and CI commands cannot diverge. That script learns
//     WHICH paths to compare from this script's own `artifact <path>` output
//     rather than restating them, so a rename cannot leave it diffing nothing.
//
//  2. The palette is NOT a copy. Every colour in the sources is a
//     `$NR_*$` placeholder resolved from frontend/src/ui/tokens/tokens.css's
//     dark theme block at build time, so the email palette's values live in
//     exactly one place in the repo — tokens.css itself.
//     What each template actually ENDED UP carrying is recorded per template in
//     palette.lock.json, which the drift test
//     (frontend/src/ui/tokens/palette-projections.test.ts) asserts against both
//     tokens.css and the generated HTML. Per template rather than globally,
//     because a value only reaches a template's output if that template uses
//     the component it styles — the notice mail has no button, so it carries no
//     `--accent-deep`.

import { readFileSync, writeFileSync, readdirSync } from 'node:fs';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

import mjml2html from 'mjml';

const HERE = dirname(fileURLToPath(import.meta.url));
const REPO = resolve(HERE, '..', '..');
const SRC = join(HERE, 'src');

const TOKENS_CSS = join(REPO, 'frontend', 'src', 'ui', 'tokens', 'tokens.css');

/** The lock file the drift test reads, repo-relative. */
const LOCK = 'tools/email/palette.lock.json';

// Email has no theme toggle: it renders the product's default (dark) palette.
const THEME_SELECTOR = ":root[data-theme='dark']";

/**
 * Each source and the crate template directory whose askama templates embed it.
 * The summary lives in netroll-app because its HTML body is rendered next to the
 * text body, from the SAME roster loop.
 *
 * verify-regeneration.sh derives the paths it byte-compares from THIS list, by
 * parsing the `artifact <path>` lines this script prints — never from a second
 * copy of the paths. See the note beside those lines.
 */
const TARGETS = [
  ['magic-link.mjml', 'backend/crates/netroll-adapters/templates/magic_link.html'],
  ['email-change.mjml', 'backend/crates/netroll-adapters/templates/email_change.html'],
  ['email-change-notice.mjml', 'backend/crates/netroll-adapters/templates/email_change_notice.html'],
  ['net-summary.mjml', 'backend/crates/netroll-app/templates/net_summary.html'],
];

/** The `--name: value;` pairs declared in tokens.css's dark theme block. */
function darkThemeVariables() {
  const source = readFileSync(TOKENS_CSS, 'utf8');
  const at = source.indexOf(THEME_SELECTOR);
  if (at < 0) {
    throw new Error(`${THEME_SELECTOR} not found in ${TOKENS_CSS}`);
  }
  const open = source.indexOf('{', at);
  const close = source.indexOf('}', open);
  const block = source.slice(open + 1, close);
  if (block.includes('{')) {
    throw new Error(`${THEME_SELECTOR} block contains a nested rule — refusing to guess`);
  }
  const variables = new Map();
  for (const [, name, value] of block.matchAll(/(--[a-z0-9-]+)\s*:\s*([^;]+);/g)) {
    variables.set(name, value.trim());
  }
  return variables;
}

/** `$NR_TEXT_MUTED$` -> `--text-muted`. */
function placeholderToVariable(placeholder) {
  return `--${placeholder.slice(4, -1).toLowerCase().replaceAll('_', '-')}`;
}

/**
 * Substitutes every `$NR_*$` placeholder from tokens.css. An unresolvable
 * placeholder is a hard failure: silently leaving `$NR_ACCENT$` in the HTML
 * would ship a broken colour that the drift test could not attribute.
 */
function resolvePalette(source, name, used) {
  return source.replaceAll(/\$NR_[A-Z0-9_]+\$/g, (placeholder) => {
    const variable = placeholderToVariable(placeholder);
    const value = variables.get(variable);
    if (value === undefined) {
      throw new Error(`${name}: ${placeholder} -> ${variable} is not declared in tokens.css`);
    }
    used.set(variable, value);
    return value;
  });
}

/**
 * Fails on any `$NR` the substitution pass did not consume.
 *
 * `resolvePalette`'s regex only matches the WELL-FORMED shape, so a typo —
 * `$NR_bg$`, `$NR_TEXT`, `$NRBG$` — is not a placeholder to it and survives
 * untouched into the shipped template. That is only latent today because every
 * current placeholder sits in a colour-typed MJML attribute, where
 * `validationLevel: 'strict'` happens to catch it; in `<mj-text>` content it
 * built clean and shipped verbatim. Scanning the RESOLVED SOURCE rather than the
 * compiled output is the stronger of the two: a stray placeholder in an
 * `<mj-attributes>` default for a component this template never uses would not
 * reach the output, and is still a bug.
 *
 * The `NR` sentinel is matched CASE-INSENSITIVELY. `$nr_bg$` is the plausible
 * typo — the CSS variable it names is lowercase — and an `NR`-only-uppercase
 * sentinel matched neither the resolver nor this scan, so the literal `$nr_bg$`
 * built clean and shipped into the template.
 */
function rejectResidualPlaceholders(source, name) {
  const residual = source.match(/\$[Nn][Rr][A-Za-z0-9_]*\$?/g);
  if (residual) {
    throw new Error(
      `${name}: unresolved placeholder(s) ${[...new Set(residual)].join(', ')} — ` +
        'the $NR_NAME$ form (uppercase) is the only one substituted; fix the spelling',
    );
  }
}

/**
 * Fails on any literal colour value in a source.
 *
 * The email palette lives in exactly ONE place — `tokens.css` — so every
 * colour must arrive through a `$NR_*$` placeholder. A
 * hard-coded hex in a `.mjml` source does not merely bypass that: it fails the
 * palette drift test OPEN. The lock records how many times each resolved value
 * survived into the output, and the lock is itself a build output, so replacing
 * one occurrence of a variable that is still used elsewhere re-baselines the
 * count and the drift test compares the new lock against the new HTML and
 * agrees with itself. Proven: `color="#FF00AA"` on net-summary.mjml:9 took
 * `--text` from 3 occurrences to 2 and all five palette tests stayed green.
 * Rejecting the literal at its source is what makes that mutation fail instead
 * of re-baseline — the build throws before any file or the lock is written.
 *
 * Scanned BEFORE `resolvePalette`, because after substitution the source is
 * legitimately full of tokens.css's own hex values.
 *
 * RESIDUAL, recorded rather than closed: a CSS NAMED colour (`color="red"`) is
 * not caught. MJML's `validationLevel: 'strict'` accepts it, and enumerating
 * the 148 CSS names to catch a shape nothing in the repo uses is not worth the
 * list. Hex and the functional notations are every form the sources or
 * tokens.css have ever used.
 */
function rejectLiteralColours(source, name) {
  const literals = [
    ...(source.match(/#(?:[0-9A-Fa-f]{8}|[0-9A-Fa-f]{6}|[0-9A-Fa-f]{3,4})\b/g) ?? []),
    ...(source.match(/\b(?:rgba?|hsla?)\s*\(/g) ?? []),
  ];
  if (literals.length > 0) {
    throw new Error(
      `${name}: literal colour value(s) ${[...new Set(literals)].join(', ')} — ` +
        'every email colour must come through a $NR_NAME$ placeholder resolved from ' +
        'frontend/src/ui/tokens/tokens.css; a literal here silently ' +
        're-baselines tools/email/palette.lock.json instead of failing',
    );
  }
}

/** Inlines `<mj-include path="./x.mjml" />` before the palette pass. */
function inlineIncludes(source, name) {
  return source.replaceAll(/[ \t]*<mj-include\s+path="\.\/([A-Za-z0-9_.-]+)"\s*\/>[ \t]*\r?\n?/g, (_, file) => {
    const included = readFileSync(join(SRC, file), 'utf8');
    if (included.includes('<mj-include')) {
      throw new Error(`${name}: nested mj-include in ${file} is not supported`);
    }
    return included;
  });
}

/** How many times `needle` occurs in `haystack`. */
function occurrences(haystack, needle) {
  return haystack.split(needle).length - 1;
}

const variables = darkThemeVariables();

// Everything is compiled and held in memory BEFORE anything is written: a throw
// on the third target used to leave the first two rewritten, the lock stale and
// no message on stderr explaining the half-regenerated tree.
const pending = [];
const manifest = {};
for (const [source, destination] of TARGETS) {
  const raw = readFileSync(join(SRC, source), 'utf8');
  const used = new Map();
  const inlined = inlineIncludes(raw, source);
  rejectLiteralColours(inlined, source);
  const mjml = resolvePalette(inlined, source, used);
  rejectResidualPlaceholders(mjml, source);
  const result = await mjml2html(mjml, {
    validationLevel: 'strict',
    minify: false,
    // `fonts: {}` disables MJML's built-in Google Fonts map. Left on, MJML
    // emits a <link> to fonts.googleapis.com in every message, which beacons
    // the recipient's IP address to a third party the moment they open the mail
    // — and the notice mail in particular must link NOWHERE (pinned by
    // `neither_part_of_the_change_notice_links_back_to_this_product`). The
    // templates use system font stacks, so nothing is lost.
    fonts: {},
    // Source comments (the license notice heading every .mjml file) stay in
    // the source; left on, MJML copies them into every message sent.
    keepComments: false,
  });
  if (result.errors.length > 0) {
    throw new Error(`${source}: ${result.errors.map((e) => e.formattedMessage ?? e.message).join('\n')}`);
  }
  // Normalised bytes: LF only, exactly one trailing newline. CRLF from a
  // checkout on another platform would fail the byte diff.
  const html = `${result.html.replaceAll('\r\n', '\n').replace(/\s+$/, '')}\n`;
  pending.push([destination, html]);

  // Only what SURVIVED into the output, and HOW MANY TIMES: a placeholder can be
  // substituted into the MJML (every template includes the shared _head.mjml)
  // and still not reach the HTML, because MJML emits an attribute default only
  // where the component it belongs to is used. The count is what lets the drift
  // test red on a SINGLE hand-edited occurrence — containment alone only reds
  // when every occurrence of a hex is changed at once.
  const survived = [...used.entries()]
    .map(([variable, value]) => [variable, { value, occurrences: occurrences(html, value) }])
    .filter(([, binding]) => binding.occurrences > 0);
  if (survived.length === 0) {
    throw new Error(`${destination}: no palette value reached the output — the substitution broke`);
  }
  // Sorted so the manifest's bytes never depend on iteration order.
  manifest[destination] = Object.fromEntries(survived.sort(([a], [b]) => (a < b ? -1 : 1)));
}

const stray = readdirSync(SRC).filter((f) => f.endsWith('.mjml') && !f.startsWith('_'))
  .filter((f) => !TARGETS.some(([source]) => source === f));
if (stray.length > 0) {
  throw new Error(`unregistered .mjml sources (add them to TARGETS): ${stray.join(', ')}`);
}

const artifacts = [];
for (const [destination, html] of pending) {
  writeFileSync(join(REPO, destination), html, 'utf8');
  artifacts.push(destination);
}
writeFileSync(join(REPO, LOCK), `${JSON.stringify(manifest, null, 2)}\n`, 'utf8');
artifacts.push(LOCK);

// The machine-readable half of this script's output: verify-regeneration.sh
// byte-compares EXACTLY these paths, parsed from here, so its pathspecs cannot
// drift from TARGETS. A hard-coded second copy is how this gate could pass
// having diffed nothing — `git diff --exit-code` over a pathspec that matches no
// file exits 0, silently.
for (const artifact of artifacts) {
  console.log(`artifact ${artifact}`);
}

const total = Object.values(manifest).reduce((n, m) => n + Object.keys(m).length, 0);
console.log(`mjml -> ${pending.length} templates, ${total} palette bindings`);
