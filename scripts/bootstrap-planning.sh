#!/usr/bin/env bash
# SPDX-License-Identifier: RPL-1.5
# Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
# Clone the private planning repository into _bmad-output/ for those with
# access.
#
# The planning artifacts (PRD, architecture, story records) live in a
# separate private repository and are gitignored here, so a fresh clone has
# an empty _bmad-output/. They are not needed to build, test or contribute;
# this script exists for the maintainer's own checkouts and for anyone the
# planning repository is shared with. A refused clone is therefore not an
# error — it is the expected outcome for everyone else — and exits 0.
#
# Override the source with NETROLL_PLANNING_REPO (e.g. a local mirror).
set -euo pipefail

REPO="$(git rev-parse --show-toplevel)"
PLANNING_REPO="${NETROLL_PLANNING_REPO:-git@github.com:nkbooth/netroll-planning.git}"
TARGET="$REPO/_bmad-output"

if [[ -d "$TARGET/.git" ]]; then
    exit 0
fi

# A directory that exists but is not a clone is a DIFFERENT problem from a
# refused clone, and git's message for it ("destination path already exists")
# would otherwise be swallowed and reported as "you do not have access".
if [[ -d "$TARGET" ]] && [[ -n "$(ls -A "$TARGET" 2>/dev/null)" ]]; then
    echo "bootstrap-planning: $TARGET already exists and is not a git clone." >&2
    echo "                    Move or remove it, then re-run." >&2
    exit 1
fi

# Only an access-shaped failure is the expected outcome for everyone else. A
# DNS failure, an expired key or a bad NETROLL_PLANNING_REPO override are real
# errors for the person who DOES have access, and reporting them as "private"
# tells the one reader who can act that there is nothing to act on.
if ! err="$(git clone -q "$PLANNING_REPO" "$TARGET" 2>&1)"; then
    case "$err" in
        *"Permission denied"*|*"access rights"*|*"not found"*|*"Repository not found"*|*"could not read Username"*)
            echo "bootstrap-planning: netroll-planning is private; planning artifacts are not needed to build or contribute." >&2
            exit 0
            ;;
    esac
    echo "bootstrap-planning: clone of $PLANNING_REPO failed, and not for want of access:" >&2
    echo "$err" >&2
    exit 1
fi
echo "bootstrap-planning: cloned planning artifacts into _bmad-output/."
