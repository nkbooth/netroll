#!/usr/bin/env bash
# SPDX-License-Identifier: RPL-1.5
# Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
# Swaps the stack beside this script onto .env.next and compose.yml.next, waits
# for /healthz, and rolls both back to the previous pair when the new one never
# turns healthy. Run on the host by .github/workflows/deploy.yml.
# Exit: 0 healthy; 1 nothing changed (no .next files, or the pull failed);
# 2 rolled back, healthy; 3 rollback unhealthy too; 4 nothing to roll back to.
#
# It never touches the database. A rollback across a release that carried a
# migration fails its health poll by design: the older binary refuses a ledger
# it doesn't recognise, and the operator restores the dump per the upgrade guide.
#
# Output lands in a public Actions log, so nothing here prints paths, hosts or
# the app's own logs; read those on the host with `podman compose logs app`.

set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")"

# Overridable only so the self-test can reach a verdict quickly.
readonly DEADLINE_SECONDS="${ROLLOUT_DEADLINE_SECONDS:-120}"
readonly POLL_SECONDS=3
readonly PROBE_SECONDS=5
readonly HEALTHY_BODY='{"status":"ok"}'
readonly SWAPPED=(.env compose.yml)
compose=(podman compose -f compose.yml)

# A wedged app or a hung exec must not outlive the deadline it is polled
# against: wait_until only checks the clock between probes.
probe() {
    timeout --kill-after=5 $((PROBE_SECONDS * 2)) \
        "${compose[@]}" exec -T app curl -fsS --max-time "$PROBE_SECONDS" "$@" 2>/dev/null
}

current_is_healthy() {
    [[ "$(probe http://127.0.0.1:3000/healthz)" == "$HEALTHY_BODY" ]]
}

# An image from before /healthz existed answers it with the HTML shell, so the
# rollback target is judged by the liveness signal it did have: the discovery
# read, which needs the database.
previous_is_healthy() {
    current_is_healthy && return 0
    [[ "$(probe -o /dev/null -w '%{content_type}' http://127.0.0.1:3000/healthz)" == text/html* ]] \
        && probe -o /dev/null http://127.0.0.1:3000/api/discovery
}

wait_until() {
    local check="$1" deadline=$((SECONDS + DEADLINE_SECONDS))
    while ((SECONDS < deadline)); do
        "$check" && return 0
        sleep "$POLL_SECONDS"
    done
    return 1
}

nothing_changed() {
    echo "rollout: $1; nothing changed" >&2
    exit 1
}

for file in "${SWAPPED[@]}"; do
    [[ -f "$file.next" ]] || nothing_changed "no $file.next to deploy"
done

has_previous=false
if [[ -f .env ]]; then
    has_previous=true
    # Pull against the incoming pair first, so a bad tag fails here with the
    # running stack and its files untouched.
    podman compose -f compose.yml.next --env-file .env.next pull --quiet \
        || nothing_changed "the pull failed"
    for file in "${SWAPPED[@]}"; do
        cp -p "$file" "$file.previous"
        mv "$file.next" "$file"
    done
else
    # Compose refuses to load a file whose env_file is missing, and on a first
    # deploy nothing is running to protect. A failed pull moves the pair back,
    # or the next deploy would keep a never-started .env as its rollback target.
    for file in "${SWAPPED[@]}"; do mv "$file.next" "$file"; done
    if ! "${compose[@]}" pull --quiet; then
        for file in "${SWAPPED[@]}"; do mv "$file" "$file.next"; done
        nothing_changed "the pull failed"
    fi
fi

if "${compose[@]}" up -d && wait_until current_is_healthy; then
    echo "rollout healthy"
    # Housekeeping only: the release is already live.
    podman image prune -f > /dev/null || echo "rollout: image prune failed; the release is live" >&2
    exit 0
fi

if [[ "$has_previous" == false ]]; then
    echo "rollout UNHEALTHY, and this was the first deploy here: nothing to roll back to" >&2
    exit 4
fi

echo "rollout UNHEALTHY; rolling back to the previous .env and compose.yml" >&2
for file in "${SWAPPED[@]}"; do cp -p "$file.previous" "$file"; done

if "${compose[@]}" up -d && wait_until previous_is_healthy; then
    echo "rollback healthy; the new release is NOT running" >&2
    exit 2
fi
echo "rollback ALSO unhealthy — see \"Roll back\" in docs/self-hosting/upgrade-an-instance.md" >&2
exit 3
