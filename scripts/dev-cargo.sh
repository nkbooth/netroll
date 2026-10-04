#!/usr/bin/env bash
# SPDX-License-Identifier: RPL-1.5
# Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
# One-shot podman runner for backend cargo commands.
#
# Why a one-shot container and not `devpod ssh`: the Postgres integration
# tests use testcontainers, which needs a container socket, and the devpod
# ssh session has none. This mounts the rootless podman socket as
# /var/run/docker.sock so testcontainers finds it, disables Ryuk (a reaper
# container that cannot start rootless), and runs the argument as a shell
# command with the working directory already at backend/ and sqlx offline.
#
# Usage: scripts/dev-cargo.sh "<command>"
#   e.g. scripts/dev-cargo.sh "cargo test -p netroll-app --test api_discovery --locked"
#
# Image: DEV_CARGO_IMAGE if set, otherwise the newest devpod image built for
# this checkout. Devpod names images `localhost/vsc-<folder>-<hash>`, so a
# clone called `netroll` produces `vsc-netroll-…`; the prefix is derived from
# the checkout's directory name so the script works under any clone name.
set -euo pipefail

# `bash -lc ""` exits 0, so a lost argument would report a passing test run that
# executed nothing. Refuse instead — a green that proves nothing is worse here
# than a hard failure.
if [[ $# -lt 1 || -z "${1// /}" ]]; then
    echo "usage: $0 \"<command>\"" >&2
    echo "  e.g. $0 \"cargo test -p netroll-app --test api_discovery --locked\"" >&2
    exit 2
fi

REPO="$(git rev-parse --show-toplevel)"
NAME="$(basename "$REPO")"
WORKSPACE="/workspaces/$NAME"
SOCKET="${XDG_RUNTIME_DIR:-/run/user/$(id -u)}/podman/podman.sock"

IMAGE="${DEV_CARGO_IMAGE:-$(podman images --format '{{.Repository}}:{{.Tag}}' | grep "^localhost/vsc-$NAME-" | head -1 || true)}"
if [[ -z "$IMAGE" ]]; then
    echo "dev-cargo: no devcontainer image found for '$NAME'." >&2
    echo "Run 'devpod up .' once to build it, or set DEV_CARGO_IMAGE to an image name." >&2
    exit 1
fi
if [[ -n "${DEV_CARGO_IMAGE:-}" ]] && ! podman image exists "$IMAGE"; then
    echo "dev-cargo: DEV_CARGO_IMAGE='$IMAGE' is not a local image." >&2
    exit 1
fi

if [[ ! -S "$SOCKET" ]]; then
    echo "dev-cargo: podman socket not found at $SOCKET." >&2
    echo "Enable it with: systemctl --user enable --now podman.socket" >&2
    exit 1
fi

# :Z (a private SELinux relabel) rather than label=disable: the container
# WRITES target/ and .cargo-cache/ into this mount, and on an SELinux-
# enforcing host a private relabel is what permits that. The git hooks mount
# the repo read-only and use label=disable instead, for the reason their own
# comments give.
exec podman run --rm \
    --userns=keep-id --network host \
    -v "$REPO":"$WORKSPACE":Z \
    -v "$SOCKET":/var/run/docker.sock \
    -e TESTCONTAINERS_RYUK_DISABLED=true \
    -e CARGO_HOME="$WORKSPACE/.cargo-cache" \
    -e SQLX_OFFLINE=true \
    -w "$WORKSPACE/backend" \
    --user dev \
    "$IMAGE" \
    bash -lc "$*"
