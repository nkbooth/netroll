#!/usr/bin/env bash
# SPDX-License-Identifier: RPL-1.5
# Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
# Point this clone at the repository's git hooks and make `git push` explicit.
#
# Why a script for two `git config` lines: hooks are per-clone and git never
# enables them on its own, so every fresh checkout has to opt in, and a
# one-liner that has to be copied from a README is the one that gets skipped.
# `--local` so nothing leaks into the user's global config; the path is
# RELATIVE so it resolves against whatever worktree this clone lives in.
#
# `push.default nothing` makes a bare `git push` refuse — you name the ref you
# are publishing, every time. Safe to run again; both writes are idempotent.
set -euo pipefail

# Resolved from the SCRIPT's location, never the cwd. `git rev-parse
# --show-toplevel` would configure whatever repository the caller happens to be
# standing in, which for a script invoked by absolute path from a sibling
# checkout is silently the wrong one.
REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

# git accepts a core.hooksPath that points nowhere and then runs NO hooks, so
# this script would otherwise announce that the gitleaks secret scan is live
# while it is inert. Check before claiming.
if [[ ! -d "$REPO/scripts/git-hooks" ]]; then
    echo "bootstrap-hooks: $REPO/scripts/git-hooks does not exist — refusing to point" >&2
    echo "                 core.hooksPath at a directory with no hooks in it." >&2
    exit 1
fi

git -C "$REPO" config --local core.hooksPath scripts/git-hooks
git -C "$REPO" config --local push.default nothing

echo "bootstrap-hooks: core.hooksPath = scripts/git-hooks (pre-commit, commit-msg and pre-push are live)."
echo "bootstrap-hooks: push.default = nothing — a bare 'git push' now refuses;"
echo "                 push a named ref deliberately, e.g. 'git push origin HEAD'."
