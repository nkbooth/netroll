// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
// CI-side commitlint config (wagoid/commitlint-github-action auto-discovers
// it at repo root). `.cjs` extension is load-bearing, not stylistic: Node's
// nearest-package.json walk from /github/workspace finds none in this repo
// and falls through to the action image's own /package.json ("type":
// "module"), which then treats a plain `.js` file as ESM and chokes on
// `module.exports`. `.cjs` sidesteps that lookup entirely.

// The message is the contract sentence in CONTRIBUTING.md, verbatim.
const NO_STORY_REFS =
    '`no-story-refs`: the subject and body must not reference internal planning identifiers ' +
    '(epic, story, AC, FR or NFR numbers); describe the user-visible change instead. ' +
    'GitHub issue references such as `Refs: #123` are fine.';

// Case-insensitive, with plurals, an optional space or hyphen and lettered
// keys, because each of those spellings has occurred; it matches
// scripts/lint-comments.sh's citation pattern. A bare `AC` with no number (as in
// AC power) passes.
const PLANNING_REF =
    /\b(epics?|stor(y|ies))\s+\d+[a-z]?(\.\d+)?\b|\bAC\s?\d+\b|\b(FR|NFR|AR|UX-DR|OQ)[\s-]?\d+\b/i;

// commitlint's default ignores skip every rule for reverts, fixup!/squash!/amend!
// and loosely matched merges, which would let a citation through any of them.
// Only subjects shaped exactly as GitHub or `git merge` writes them are
// skipped: their text is generated, not ours. Prose after the names is not.
const NAMES = "'[^']+'(, '[^']+')*( and '[^']+')?";
const GENERATED_MERGE = new RegExp(
    '^Merge (pull request #\\d+ from \\S+' +
        `|(remote-tracking )?branch(es)? ${NAMES}( of \\S+)?` +
        `|(tag|commit) ${NAMES}` +
        // A bare ref is a remote one or a SHA, so `Merge <citation>` is not skipped.
        '|\\S+/\\S+|[0-9a-f]{7,40})( into \\S+)?$',
);

module.exports = {
    extends: ['@commitlint/config-conventional'],
    defaultIgnores: false,
    ignores: [(message) => GENERATED_MERGE.test(message.split('\n')[0])],
    plugins: [
        {
            rules: {
                // Header, body AND footer: commitlint parses `Closes #12` and
                // anything after it as footer, so a subject-and-body rule would
                // let an identifier through below the first trailer. The header
                // also covers the type and scope.
                'no-story-refs': (parsed) => {
                    const text = [parsed.header, parsed.body, parsed.footer].filter(Boolean).join('\n');
                    const hit = text.match(PLANNING_REF);
                    return [!hit, NO_STORY_REFS + (hit ? ` Found: "${hit[0]}"` : '')];
                },
            },
        },
    ],
    rules: { 'no-story-refs': [2, 'always'] },
};
