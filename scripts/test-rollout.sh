#!/usr/bin/env bash
# SPDX-License-Identifier: RPL-1.5
# Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
# Self-test for deploy/rollout.sh against the real deploy/compose.prod.yml under
# podman compose: every exit branch is driven once and judged by its exit code,
# the .env left in place and the image left running.
#
# Needs two locally built images and the host's port 3001; not run in CI.
#   podman build --format docker -t netroll:hc .                 (this tree)
#   podman build --format docker -t netroll:prev <older tree>    (predates /healthz)
#
#   scripts/test-rollout.sh [case...]   exit 0 = every case behaved, 1 = at least one did not
#   (cases: first a b c d e f g h i j k l; default all)
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
CURRENT="${ROLLOUT_TEST_CURRENT:-hc}"
PREVIOUS="${ROLLOUT_TEST_PREVIOUS:-prev}"
WORK="$(mktemp -d)"
STACK=""
failures=0

teardown() {
    [[ -n "$STACK" ]] && (cd "$STACK" && podman compose -f compose.yml down -v > /dev/null 2>&1 || true)
    STACK=""
}
trap 'teardown; rm -rf "$WORK"' EXIT

# Registry-free: a pull is replaced by the config load it would have done, and
# a prune is skipped so the developer's image store is left alone. The
# ROLLOUT_TEST_* switches make a pull or a prune fail, or every probe hang.
mkdir -p "$WORK/bin"
cat > "$WORK/bin/podman" <<'EOF'
#!/usr/bin/env bash
args=("$@")
for i in "${!args[@]}"; do
    if [[ "${args[$i]}" == pull ]]; then
        [[ -n "${ROLLOUT_TEST_PULL_FAILS:-}" ]] && exit 125
        args[$i]=config; args=("${args[@]/--quiet/-q}"); exec /usr/bin/podman "${args[@]}"
    fi
    [[ "${args[$i]}" == exec && -n "${ROLLOUT_TEST_PROBE_HANGS:-}" ]] && exec sleep infinity
done
if [[ "$1 $2" == "image prune" ]]; then
    [[ -n "${ROLLOUT_TEST_PRUNE_FAILS:-}" ]] && exit 1
    exit 0
fi
exec /usr/bin/podman "$@"
EOF
chmod +x "$WORK/bin/podman"
export PATH="$WORK/bin:$PATH"

env_for() {
    local tag="$1" mail_from="$2"
    printf '%s\n' \
        "IMAGE_REF=localhost/netroll" "IMAGE_TAG=$tag" \
        "POSTGRES_PASSWORD=rollout-test" \
        "DATABASE_URL=postgres://netroll:rollout-test@postgres:5432/netroll" \
        "SMTP_HOST=localhost" "SMTP_PORT=1025" \
        "MAIL_FROM=$mail_from" "PUBLIC_BASE_URL=http://localhost:3001"
}
good() { env_for "$1" "${2:-no-reply@netroll.invalid}"; }
# The app refuses to boot without a sender.
bad() { env_for "$1" ""; }

# deploy.yml ships the compose file as compose.yml.next beside .env.next.
fresh_stack() {
    STACK="$WORK/$1"
    mkdir -p "$STACK"
    cp "$ROOT/deploy/compose.prod.yml" "$STACK/compose.yml"
    cp "$ROOT/deploy/compose.prod.yml" "$STACK/compose.yml.next"
    cp "$ROOT/deploy/rollout.sh" "$STACK/rollout.sh"
}

# A first deploy finds no compose.yml on the host either.
fresh_host() {
    fresh_stack "$1"
    rm "$STACK/compose.yml"
}

# Starts the stack on $1 without the script under test.
running_on() {
    printf '%s\n' "$1" > "$STACK/.env"
    (cd "$STACK" && podman compose -f compose.yml up -d > /dev/null 2>&1)
}

rollout() {
    local status=0
    (cd "$STACK" && bash rollout.sh) > "$STACK/out" 2>&1 || status=$?
    echo "$status"
}

check() {
    local name="$1" outcome="$2"
    if [[ "$outcome" == ok ]]; then
        echo "ok   $name"
    else
        echo "FAIL $name: $outcome"
        sed 's/^/     | /' "$STACK/out" 2>/dev/null || true
        failures=$((failures + 1))
    fi
}

running_image() { podman inspect -f '{{.ImageName}}' netlogger-app; }
running_image_id() { podman inspect -f '{{.Image}}' netlogger-app; }
image_id() { podman image inspect -f '{{.Id}}' "localhost/netroll:$1"; }
image_digest() { podman image inspect -f '{{.Digest}}' "localhost/netroll:$1"; }
running_mail_from() { podman exec netlogger-app printenv MAIL_FROM; }

case_first_deploy_succeeds() {
    fresh_host first-good
    good "$CURRENT" > "$STACK/.env.next"
    local status; status="$(rollout)"
    [[ "$status" == 0 ]] || { check "first deploy succeeds" "exit $status"; teardown; return; }
    [[ ! -e "$STACK/.env.previous" ]] || { check "first deploy succeeds" ".env.previous invented"; teardown; return; }
    check "first deploy succeeds" ok
    teardown
}

case_a_good_release_replaces_a_good_one() {
    fresh_stack a
    running_on "$(good "$CURRENT")"
    local before; before="$(cat "$STACK/.env")"
    good "$CURRENT" "other@netroll.invalid" > "$STACK/.env.next"
    local next; next="$(cat "$STACK/.env.next")"
    local status; status="$(rollout)"
    if [[ "$status" != 0 ]]; then check "A good release" "exit $status"
    elif [[ "$(cat "$STACK/.env")" != "$next" ]]; then check "A good release" ".env is not the new one"
    elif [[ "$(cat "$STACK/.env.previous")" != "$before" ]]; then check "A good release" ".env.previous is not the old one"
    elif [[ "$(running_mail_from)" != "other@netroll.invalid" ]]; then check "A good release" "the app still runs the old .env"
    else check "A good release" ok; fi
    teardown
}

case_b_bad_release_rolls_back() {
    fresh_stack b
    running_on "$(good "$CURRENT")"
    local before; before="$(cat "$STACK/.env")"
    bad "$CURRENT" > "$STACK/.env.next"
    local status; status="$(rollout)"
    if [[ "$status" != 2 ]]; then check "B bad release rolls back" "exit $status, want 2"
    elif [[ "$(cat "$STACK/.env")" != "$before" ]]; then check "B bad release rolls back" ".env not restored"
    else check "B bad release rolls back" ok; fi
    teardown
}

case_c_rollback_target_is_bad_too() {
    fresh_stack c
    running_on "$(bad "$CURRENT")"
    bad "$CURRENT" > "$STACK/.env.next"
    local status; status="$(rollout)"
    [[ "$status" == 3 ]] && check "C both bad" ok || check "C both bad" "exit $status, want 3"
    teardown
}

case_d_rollback_to_an_image_without_healthz() {
    fresh_stack d
    running_on "$(good "$PREVIOUS")"
    bad "$CURRENT" > "$STACK/.env.next"
    local status; status="$(rollout)"
    if [[ "$status" != 2 ]]; then check "D rollback predates /healthz" "exit $status, want 2"
    elif [[ "$(running_image)" != "localhost/netroll:$PREVIOUS" ]]; then check "D rollback predates /healthz" "running $(running_image)"
    else check "D rollback predates /healthz" ok; fi
    teardown
}

case_e_bad_first_deploy_has_nothing_to_restore() {
    fresh_host e
    bad "$CURRENT" > "$STACK/.env.next"
    local status; status="$(rollout)"
    [[ "$status" == 4 ]] && check "E bad first deploy" ok || check "E bad first deploy" "exit $status, want 4"
    teardown
}

# A .env rendered before IMAGE_REF existed: the default repository has no
# such tag, so the rollback cannot even start its stack.
case_f_rollback_target_cannot_start() {
    fresh_stack f
    running_on "$(good "$CURRENT")"
    command grep -v '^IMAGE_REF=' "$STACK/.env" > "$STACK/.env.old" && mv "$STACK/.env.old" "$STACK/.env"
    bad "$CURRENT" > "$STACK/.env.next"
    local status; status="$(rollout)"
    [[ "$status" == 3 ]] && check "F rollback cannot start" ok || check "F rollback cannot start" "exit $status, want 3"
    teardown
}

# A release that breaks the app through compose.yml alone: the rollback has to
# put the old compose file back too, or it restarts the same breakage.
case_g_rollback_restores_the_compose_file() {
    fresh_stack g
    running_on "$(good "$CURRENT")"
    local before; before="$(cat "$STACK/compose.yml")"
    sed 's|^        env_file: .env$|&\n        environment:\n            MAIL_FROM: ""|' \
        "$ROOT/deploy/compose.prod.yml" > "$STACK/compose.yml.next"
    good "$CURRENT" > "$STACK/.env.next"
    local status; status="$(rollout)"
    if [[ "$status" != 2 ]]; then check "G rollback restores compose.yml" "exit $status, want 2"
    elif [[ "$(cat "$STACK/compose.yml")" != "$before" ]]; then check "G rollback restores compose.yml" "compose.yml not restored"
    else check "G rollback restores compose.yml" ok; fi
    teardown
}

# deploy.yml pins IMAGE_TAG to tag@digest. A redeploy that re-pushes the same
# tag must still roll back to the image the old .env names, not the tag's new one.
case_h_rollback_follows_the_digest_not_the_tag() {
    fresh_stack h
    podman tag "localhost/netroll:$CURRENT" localhost/netroll:same
    running_on "$(good "same@$(image_digest "$CURRENT")")"
    podman tag "localhost/netroll:$PREVIOUS" localhost/netroll:same
    good "same@$(image_digest "$PREVIOUS")" > "$STACK/.env.next"
    local status; status="$(rollout)"
    if [[ "$status" != 2 ]]; then check "H rollback follows the digest" "exit $status, want 2"
    elif [[ "$(running_image_id)" != "$(image_id "$CURRENT")" ]]; then check "H rollback follows the digest" "not running the old image"
    else check "H rollback follows the digest" ok; fi
    # Naming the tag twice untags only it; a bare image argument strips every name.
    podman untag localhost/netroll:same localhost/netroll:same > /dev/null 2>&1 || true
    teardown
}

case_i_failed_pull_changes_nothing() {
    fresh_stack i
    running_on "$(good "$CURRENT")"
    local before; before="$(cat "$STACK/.env" "$STACK/compose.yml")"
    good "$CURRENT" "other@netroll.invalid" > "$STACK/.env.next"
    local status; status="$(ROLLOUT_TEST_PULL_FAILS=1 rollout)"
    if [[ "$status" != 1 ]]; then check "I failed pull" "exit $status, want 1"
    elif [[ "$(cat "$STACK/.env" "$STACK/compose.yml")" != "$before" ]]; then check "I failed pull" "the running stack's files changed"
    else check "I failed pull" ok; fi
    teardown
}

# Otherwise the next deploy keeps a never-started .env as its rollback target.
case_j_failed_first_pull_leaves_no_env() {
    fresh_host j
    good "$CURRENT" > "$STACK/.env.next"
    local status; status="$(ROLLOUT_TEST_PULL_FAILS=1 rollout)"
    if [[ "$status" != 1 ]]; then check "J failed first pull" "exit $status, want 1"
    elif [[ -e "$STACK/.env" || -e "$STACK/compose.yml" ]]; then check "J failed first pull" "left a .env or compose.yml behind"
    elif [[ ! -e "$STACK/.env.next" || ! -e "$STACK/compose.yml.next" ]]; then check "J failed first pull" "lost the .next files"
    else check "J failed first pull" ok; fi
    teardown
}

case_k_failed_prune_keeps_a_healthy_rollout_green() {
    fresh_stack k
    running_on "$(good "$CURRENT")"
    good "$CURRENT" "other@netroll.invalid" > "$STACK/.env.next"
    local status; status="$(ROLLOUT_TEST_PRUNE_FAILS=1 rollout)"
    [[ "$status" == 0 ]] && check "K failed prune" ok || check "K failed prune" "exit $status, want 0"
    teardown
}

# A probe that never returns must not hold the rollout past its deadline.
case_l_hung_probe_still_reaches_a_verdict() {
    fresh_stack l
    running_on "$(good "$CURRENT")"
    good "$CURRENT" "other@netroll.invalid" > "$STACK/.env.next"
    local status=0
    (cd "$STACK" && ROLLOUT_TEST_PROBE_HANGS=1 ROLLOUT_DEADLINE_SECONDS=15 timeout 150 bash rollout.sh) \
        > "$STACK/out" 2>&1 || status=$?
    [[ "$status" == 3 ]] && check "L hung probe" ok || check "L hung probe" "exit $status, want 3 (124 = hung)"
    teardown
}

declare -A CASES=(
    [first]=case_first_deploy_succeeds
    [a]=case_a_good_release_replaces_a_good_one
    [b]=case_b_bad_release_rolls_back
    [c]=case_c_rollback_target_is_bad_too
    [d]=case_d_rollback_to_an_image_without_healthz
    [e]=case_e_bad_first_deploy_has_nothing_to_restore
    [f]=case_f_rollback_target_cannot_start
    [g]=case_g_rollback_restores_the_compose_file
    [h]=case_h_rollback_follows_the_digest_not_the_tag
    [i]=case_i_failed_pull_changes_nothing
    [j]=case_j_failed_first_pull_leaves_no_env
    [k]=case_k_failed_prune_keeps_a_healthy_rollout_green
    [l]=case_l_hung_probe_still_reaches_a_verdict
)
selected=("$@")
((${#selected[@]} > 0)) || selected=(first a b c d e f g h i j k l)
for name in "${selected[@]}"; do
    "${CASES[$name]:?unknown case $name}"
done

if ((failures > 0)); then
    echo "test-rollout: $failures case(s) misbehaved"
    exit 1
fi
echo "test-rollout: every case behaved"
