#!/usr/bin/env bash
# SPDX-License-Identifier: RPL-1.5
# Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
# The variable table in deploy/README.md is checked against the env templates,
# not trusted.
#
# WHY: a self-hoster copies the templates and reads the README; when the two
# disagree, the README is the one that is wrong, and nothing else notices. This
# compares the backticked names in the README's marked table with the union of
# variable names the two templates actually set, and fails on any difference.
#
#   scripts/check-env-docs.sh [path/to/README.md]
#
#   exit 0 = the table names exactly the union of template variables
#   exit 1 = at least one name is missing from the table or absent from every
#            template (each difference is printed)
#   exit 2 = bad usage, a file is missing, a table marker is absent, or either
#            side carries no variable names at all — none of these may ever
#            read as "nothing to check"
#
# The optional README path exists so a mutation proof can run against a
# scratch copy. The templates are always the tracked ones at the repo root and
# under deploy/. Same check-mode shape as scripts/github-settings.sh so a CI
# job can call it unchanged.
set -euo pipefail

# `comm` compares bytes, so `sort` must not collate by locale or the two orderings
# disagree for names differing only in underscore placement.
export LC_ALL=C

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
README="${1:-$ROOT/deploy/README.md}"
TEMPLATES=("$ROOT/.env.template" "$ROOT/deploy/.env.production.tpl")
START_MARKER='<!-- env-table:start -->'
END_MARKER='<!-- env-table:end -->'

usage() {
    echo "usage: $0 [path/to/deploy/README.md]" >&2
    exit 2
}

[[ $# -le 1 ]] || usage
[[ -f "$README" ]] || { echo "error: README not found: $README" >&2; exit 2; }
for template in "${TEMPLATES[@]}"; do
    [[ -f "$template" ]] || { echo "error: template not found: $template" >&2; exit 2; }
done

for marker in "$START_MARKER" "$END_MARKER"; do
    count="$(grep -cF -- "$marker" "$README" || true)"
    if [[ "$count" -ne 1 ]]; then
        echo "error: expected exactly one '$marker' in $README, found $count" >&2
        exit 2
    fi
done

# Only the backticked first column of table rows between the markers counts as
# a documented variable; prose that happens to mention a name does not.
documented="$(
    sed -n "/$START_MARKER/,/$END_MARKER/p" "$README" \
        | { grep -oE '^\|[[:space:]]*`[A-Z][A-Z0-9_]*`' || true; } \
        | tr -d '|` ' \
        | sort -u
)"

# Uncommented assignments only: a commented-out example (CSP_POLICY=...) is
# documentation of an override, not a variable the templates set.
declared="$(
    { grep -ohE '^[A-Z][A-Z0-9_]*=' "${TEMPLATES[@]}" || true; } \
        | tr -d '=' \
        | sort -u
)"

# A `grep` that matches nothing would otherwise kill the script under `set -e`
# with no output at all, which reads as an infra failure rather than the exit 2
# the contract reserves for "nothing to check".
if [[ -z "$documented" ]]; then
    echo "error: no variable rows between the table markers in $README" >&2
    exit 2
fi
if [[ -z "$declared" ]]; then
    echo "error: no variable assignments in ${TEMPLATES[*]}" >&2
    exit 2
fi

missing="$(comm -13 <(printf '%s\n' "$documented") <(printf '%s\n' "$declared"))"
extra="$(comm -23 <(printf '%s\n' "$documented") <(printf '%s\n' "$declared"))"

status=0
if [[ -n "$missing" ]]; then
    while IFS= read -r name; do
        echo "missing from README: $name"
    done <<<"$missing"
    status=1
fi
if [[ -n "$extra" ]]; then
    while IFS= read -r name; do
        echo "not in any template: $name"
    done <<<"$extra"
    status=1
fi

if [[ "$status" -eq 0 ]]; then
    echo "OK: $(printf '%s\n' "$declared" | grep -c .) variables documented"
fi
exit "$status"
