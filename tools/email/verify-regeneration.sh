#!/usr/bin/env bash
# SPDX-License-Identifier: RPL-1.5
# Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
# Proves that recompiling every .mjml source is a byte-exact no-op against the
# checked-in HTML. Without this, a generated artifact can drift from its source
# and committing the output would be unsound.
#
# CI invokes THIS script (ci.yml, `frontend` job) rather than repeating the
# commands, so the local and CI invocations cannot diverge. Run it from anywhere.
#
# THE PATHSPECS ARE NOT WRITTEN HERE. `git diff --exit-code -- <path that matches
# nothing>` exits 0 silently, and so does `git ls-files --others -- <no such
# path>`, so a hard-coded list that drifts from build.mjs's TARGETS by one rename
# or one typo turns this gate into a no-op that still prints success — a guard
# that has shipped inert in this repository before. Instead build.mjs reports
# each file it wrote on
# stdout as `artifact <repo-relative-path>`, and every one of those is asserted
# to be a TRACKED file before anything is compared. A renamed or mistyped target
# therefore fails here rather than passing vacuously.
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo="$(cd "$here/../.." && pwd)"

# Lockfiles are law (ci.yml:12-13) — never `npm install` here.
npm ci --prefix "$here" --no-audit --no-fund

# Command substitution, not a pipe or a process substitution: under `set -e` an
# assignment propagates the failure, so a build.mjs that throws stops the script
# instead of leaving `artifacts` empty and the gate reporting on nothing.
build_output="$(node "$here/build.mjs")"

artifacts=()
while IFS= read -r line; do
    case "$line" in
        artifact\ *) artifacts+=("${line#artifact }") ;;
        *) printf '%s\n' "$line" ;;
    esac
done <<< "$build_output"

cd "$repo"

# The four message kinds plus palette.lock.json. EXACT, not "at least one":
# a floor would still pass if a TARGETS entry were dropped, which leaves its
# artifact tracked, stale and never compared. Hard-coded on purpose, the same way
# EXPECTED_BINDINGS is in palette-projections.test.ts — a fifth message kind is
# meant to make somebody change this line.
readonly EXPECTED_ARTIFACTS=5
if [ "${#artifacts[@]}" -ne "$EXPECTED_ARTIFACTS" ]; then
    echo >&2 "ERROR: build.mjs reported ${#artifacts[@]} artifact(s); expected $EXPECTED_ARTIFACTS"
    echo >&2 "(the four generated templates plus palette.lock.json)."
    echo >&2 "If a message kind was added or removed, update EXPECTED_ARTIFACTS here and"
    echo >&2 "EXPECTED_BINDINGS in frontend/src/ui/tokens/palette-projections.test.ts."
    exit 1
fi

# The count alone is satisfied by a DUPLICATE filling the slot of a dropped
# path: reporting magic_link.html twice in place of net_summary.html still
# reports five, and `git diff -- a a b` compares `a` twice while the real drift
# in net_summary.html is never looked at — the script printed "5 tracked
# artifacts byte-compared" and exited 0. Distinctness is what makes the count
# mean "five different files".
readarray -t distinct_artifacts < <(printf '%s\n' "${artifacts[@]}" | sort -u)
if [ "${#distinct_artifacts[@]}" -ne "${#artifacts[@]}" ]; then
    echo >&2 "ERROR: build.mjs reported the same artifact path more than once:"
    printf >&2 '  %s\n' "${artifacts[@]}"
    echo >&2
    echo >&2 "Two TARGETS entries in tools/email/build.mjs share a destination, so one"
    echo >&2 "generated artifact would never have been byte-compared. Fix the paths."
    exit 1
fi

# Every reported path must resolve to a file git already tracks. This is what
# makes the diff below non-vacuous: an untracked or nonexistent pathspec is
# invisible to `git diff --exit-code`, which is why the check is made here
# explicitly instead of being inferred from that command's exit status.
missing=()
for artifact in "${artifacts[@]}"; do
    if ! git ls-files --error-unmatch -- "$artifact" >/dev/null 2>&1; then
        missing+=("$artifact")
    fi
done
if [ "${#missing[@]}" -gt 0 ]; then
    echo >&2 "ERROR: build.mjs wrote paths that git does not track:"
    printf >&2 '  %s\n' "${missing[@]}"
    echo >&2
    echo >&2 "Either a TARGETS entry in tools/email/build.mjs was renamed or mistyped"
    echo >&2 "(in which case nothing would have been byte-compared), or a new generated"
    echo >&2 "artifact was never \`git add\`ed. Fix the path or stage the file."
    exit 1
fi

if ! git diff --exit-code -- "${artifacts[@]}"; then
    echo >&2
    echo "ERROR: regenerating the MJML templates changed the checked-in output." >&2
    echo "The committed HTML is not what tools/email/src/*.mjml compiles to." >&2
    echo "Run tools/email/verify-regeneration.sh locally and commit the result." >&2
    exit 1
fi

# A brand-new .mjml whose output was never committed shows up as untracked, not
# as a diff — `git diff` would pass and the artifact would ship uninstalled,
# which is exactly how an earlier guard here shipped inert. The directories scanned
# are derived from the artifact paths above for the same reason the diff's
# pathspecs are.
scan_dirs=("tools/email")
for artifact in "${artifacts[@]}"; do
    scan_dirs+=("$(dirname "$artifact")")
done
readarray -t scan_dirs < <(printf '%s\n' "${scan_dirs[@]}" | sort -u)
for dir in "${scan_dirs[@]}"; do
    if [ ! -d "$repo/$dir" ]; then
        echo >&2 "ERROR: untracked scan directory '$dir' does not exist — the scan would"
        echo >&2 "have covered nothing."
        exit 1
    fi
done

untracked="$(git ls-files --others --exclude-standard -- "${scan_dirs[@]}")"
if [ -n "$untracked" ]; then
    echo >&2 "ERROR: generated email artifacts are untracked (git add them):"
    echo >&2 "$untracked"
    exit 1
fi

echo "MJML regeneration is a no-op (${#artifacts[@]} tracked artifacts byte-compared)."
