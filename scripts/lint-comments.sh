#!/usr/bin/env bash
# SPDX-License-Identifier: RPL-1.5
# Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
# The tracked tree must not cite internal planning artifacts, and comments must
# not crowd the code.
#
# WHY: the repository is public and its planning records are not, so a citation
# is a pointer at something no reader can open. Four rules, every hit printed as
# `<file>:<line>: <rule>: <detail>` before exiting:
#
#   citation       a planning identifier on ANY line (epic, story, acceptance
#                  criterion, requirement and decision-record numbers), any case
#   decision-id    `D` plus one or two digits on a comment line of a .rs, .ts,
#                  .tsx, .js or .css file (in prose or code it is too common)
#   planning-file  the name of a planning document on any line
#   density        a scope's comment lines exceed its ratchet ceiling (below);
#                  the license notice at a file's head is neither comment nor code
#
# It does not catch narration that cites nothing ("as decided earlier"); that is
# review's job. A rule that misfires twice is deleted, never excepted.
#
#   scripts/lint-comments.sh     exit 0 = clean, 1 = at least one hit, 2 = usage or tool error
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

[[ $# -eq 0 ]] || { echo "usage: $0" >&2; exit 2; }

EXCLUDES=(
    ':!CHANGELOG.md'       # rewritten wholesale by the release tooling
    ':!*.lock'             # generated
    ':!*package-lock.json' # generated
    ':!*.min.js'           # vendored bundles; mermaid's alone carries D-number tokens
)

CITATION='\b(epics?|stor(y|ies))[[:space:]]+[0-9]+[a-z]?(\.[0-9]+)?\b|\bAC[[:space:]]?[0-9]+\b|\b(FR|NFR|AR|UX-DR|OQ)[[:space:]-]?[0-9]+\b'
PLANNING_FILE='deferred[- ]work|sprint-status\.yaml|\bepics\.md\b|project-context\.md'

# Ratchet ceilings, in percent of a scope's lines that are comment lines. The
# direction is 12 for each backend crate and 10 for frontend/src; until a scope
# gets there its ceiling is its own measured figure rounded up to the next half
# percent. A rise fails. After a fall, lower the ceiling here by hand.
SCOPES=(
    'backend/crates/netroll-domain 24.0'
    'backend/crates/netroll-adapters 15.5'
    'backend/crates/netroll-app 16.5'
    'frontend/src 17.0'
)

hits=0

# git grep exits 1 for "no match" and 2+ for a real error; only the latter may stop the run.
grep_rule() {
    local rule="$1" flags="$2" pattern="$3" out status=0
    out="$(git grep "$flags" -e "$pattern" -- . "${EXCLUDES[@]}")" || status=$?
    [[ "$status" -le 1 ]] || { echo "error: git grep failed for $rule" >&2; exit 2; }
    [[ -n "$out" ]] || return 0
    sed -E "s/^([^:]*:[0-9]+):(.*)\$/\\1: $rule: \\2/" <<<"$out"
    hits=$((hits + $(grep -c . <<<"$out")))
}

# A comment line is one that starts with `//` or `/*` (or JSX's `{/*`), or sits
# inside an open block comment; a trailing comment after code is not one.
# Plain ERE with no interval braces, so the awk on a CI runner (mawk) reads it
# the same as gawk does.
decision_ids() {
    local out
    out="$(git ls-files -z -- '*.rs' '*.ts' '*.tsx' '*.js' '*.css' "${EXCLUDES[@]}" \
        | xargs -0 -r awk '
            FNR == 1 { inblk = 0 }
            {
                line = $0; sub(/^[ \t]+/, "", line); is_comment = 0
                if (inblk) { is_comment = 1; if (line ~ /\*\//) inblk = 0 }
                else if (line ~ /^(\{)?\/\*/) { is_comment = 1; if (line !~ /\*\//) inblk = 1 }
                else if (line ~ /^\/\//) is_comment = 1
                if (is_comment && $0 ~ /(^|[^A-Za-z0-9_])D[0-9][0-9]?([^A-Za-z0-9_]|$)/)
                    print FILENAME ":" FNR ": decision-id: " line
            }')"
    [[ -n "$out" ]] || return 0
    printf '%s\n' "$out"
    hits=$((hits + $(grep -c . <<<"$out")))
}

# xargs may split a long file list into several awk runs, each printing its own
# totals, so each run prints raw counts and a second awk sums them. Feeding one
# awk a concatenated stream instead would lose FNR and with it the per-file reset.
density() {
    local scope ceiling counts comment total pct
    for entry in "${SCOPES[@]}"; do
        read -r scope ceiling <<<"$entry"
        counts="$(git ls-files -z -- "$scope/*.rs" "$scope/*.ts" "$scope/*.tsx" \
            | xargs -0 -r awk '
                FNR == 1 { inblk = 0 }
                FNR <= 5 && /^\/\/ (SPDX-License-Identifier: |Copyright [(]C[)] )/ { next }
                { total++; line = $0; sub(/^[ \t]+/, "", line)
                  if (inblk) { comment++; if (line ~ /\*\//) inblk = 0; next }
                  if (line ~ /^(\{)?\/\*/) { comment++; if (line !~ /\*\//) inblk = 1; next }
                  if (line ~ /^\/\//) { comment++; next } }
                END { print comment + 0, total + 0 }' \
            | awk '{ c += $1; t += $2 } END { print c + 0, t + 0 }')"
        read -r comment total <<<"$counts"
        if [[ "$total" -eq 0 ]]; then
            echo "error: density scope $scope has no source lines" >&2
            exit 2
        fi
        pct="$(awk -v c="$comment" -v t="$total" 'BEGIN { printf "%.2f", 100 * c / t }')"
        echo "density $scope comment=$comment total=$total pct=$pct ceiling=$ceiling"
        if awk -v p="$pct" -v c="$ceiling" 'BEGIN { exit !(p > c) }'; then
            echo "$scope: density: $pct% is over the $ceiling% ceiling"
            hits=$((hits + 1))
        fi
    done
}

grep_rule citation -nIiEo "$CITATION"
decision_ids
grep_rule planning-file -nIiEo "$PLANNING_FILE"
density

if [[ "$hits" -ne 0 ]]; then
    echo "lint-comments: $hits hit(s)"
    exit 1
fi
echo "lint-comments: clean"
