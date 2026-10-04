#!/usr/bin/env bash
# SPDX-License-Identifier: RPL-1.5
# Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
# Self-test for scripts/github-settings.sh against a stub `gh` that serves
# canned API responses and records every write, so a setting's read, its
# DRIFT verdict and the body `apply` sends are all checked without a network.
#
#   scripts/test-github-settings.sh   exit 0 = every case behaved, 1 = at least one did not
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT
REPO=owner/repo
APP_ID=123456
MAINTAINER_ID=68024347
failures=0

mkdir -p "$WORK/bin"
cat > "$WORK/bin/gh" <<'EOF'
#!/usr/bin/env bash
# GET serves $GH_STUB_STATE/api/<path>.json (absent = 404); any other method
# is appended to requests.jsonl and changes nothing.
set -euo pipefail
[[ "${1:-}" == api ]] || exit 99
shift
method=GET path="" jqexpr="" input=""
while (($#)); do
    case "$1" in
        -X) method="$2"; shift 2 ;;
        --jq) jqexpr="$2"; shift 2 ;;
        --input) input="$2"; shift 2 ;;
        -f | -F) shift 2 ;;
        --silent) shift ;;
        *) path="${1#/}"; shift ;;
    esac
done
file="$GH_STUB_STATE/api/$path.json"
if [[ "$method" == GET ]]; then
    [[ -f "$file" ]] || { echo "gh: Not Found (HTTP 404)" >&2; exit 1; }
    if [[ -n "$jqexpr" ]]; then jq -r "$jqexpr" "$file"; else cat "$file"; fi
    exit 0
fi
body="{}"
[[ -z "$input" ]] || body="$(cat -- "$input")"
jq -c -n --arg m "$method" --arg p "$path" --argjson b "$body" '{method: $m, path: $p, body: $b}' \
    >> "$GH_STUB_STATE/requests.jsonl"
EOF
chmod +x "$WORK/bin/gh"
export PATH="$WORK/bin:$PATH"

fresh_state() {
    export GH_STUB_STATE="$WORK/state"
    rm -rf "$GH_STUB_STATE"
    mkdir -p "$GH_STUB_STATE/api/repos/$REPO"
    : > "$GH_STUB_STATE/requests.jsonl"
    echo '{"private": false}' > "$GH_STUB_STATE/api/repos/$REPO.json"
}

serve() { # api-path json
    mkdir -p "$(dirname "$GH_STUB_STATE/api/$1")"
    printf '%s\n' "$2" > "$GH_STUB_STATE/api/$1.json"
}

settings() { # mode setting -> exit status
    local status=0
    bash "$ROOT/scripts/github-settings.sh" "$1" "$REPO" "$2" > "$WORK/out" 2>&1 || status=$?
    echo "$status"
}

# The first recorded write matching method and path, as JSON (empty if none).
request() { jq -c --arg m "$1" --arg p "$2" 'select(.method == $m and .path == $p) | .body' "$GH_STUB_STATE/requests.jsonl" | head -n1; }
writes() { wc -l < "$GH_STUB_STATE/requests.jsonl"; }

check() {
    if [[ "$2" == ok ]]; then
        echo "ok   $1"
    else
        echo "FAIL $1: $2"
        sed 's/^/     | /' "$WORK/out"
        failures=$((failures + 1))
    fi
}

expect_status() { # name want got
    if [[ "$3" == "$2" ]]; then check "$1" ok; else check "$1" "exit $3, want $2"; fi
}

# ---- hero_environment ----

ENV_PATH="repos/$REPO/environments/hero-production"

serve_hero_environment() { # tag-policies-json
    serve "$ENV_PATH" "$(jq -n --argjson id "$MAINTAINER_ID" '{
        protection_rules: [{type: "required_reviewers", reviewers: [{type: "User", reviewer: {id: $id}}]}],
        deployment_branch_policy: {protected_branches: false, custom_branch_policies: true}}')"
    serve "$ENV_PATH/deployment-branch-policies" "$1"
}

case_hero_environment_matching_is_ok() {
    fresh_state
    serve_hero_environment '{"total_count": 1, "branch_policies": [{"id": 1, "name": "v*", "type": "tag"}]}'
    expect_status "hero_environment: reviewer + v* tag policy is OK" 0 "$(settings check hero_environment)"
}

case_hero_environment_without_tag_policy_drifts() {
    fresh_state
    serve_hero_environment '{"total_count": 0, "branch_policies": []}'
    expect_status "hero_environment: missing v* tag policy is DRIFT" 1 "$(settings check hero_environment)"
}

case_hero_environment_any_branch_drifts() {
    fresh_state
    serve "$ENV_PATH" "$(jq -n --argjson id "$MAINTAINER_ID" '{
        protection_rules: [{type: "required_reviewers", reviewers: [{type: "User", reviewer: {id: $id}}]}],
        deployment_branch_policy: null}')"
    expect_status "hero_environment: any-ref deploys are DRIFT" 1 "$(settings check hero_environment)"
}

case_hero_environment_apply_sends_the_full_payload() {
    fresh_state
    settings apply hero_environment > /dev/null
    local put post
    put="$(request PUT "$ENV_PATH")"
    post="$(request POST "$ENV_PATH/deployment-branch-policies")"
    if [[ -z "$put" ]]; then check "hero_environment: apply PUTs the environment" "no PUT"; return; fi
    if jq -e --argjson id "$MAINTAINER_ID" '
        .deployment_branch_policy == {protected_branches: false, custom_branch_policies: true}
        and .wait_timer == 0 and .reviewers == [{type: "User", id: $id}]' <<< "$put" > /dev/null; then
        check "hero_environment: apply PUTs reviewer, wait timer and branch policy" ok
    else
        check "hero_environment: apply PUTs reviewer, wait timer and branch policy" "$put"
    fi
    if [[ "$(jq -cS . <<< "${post:-null}")" == '{"name":"v*","type":"tag"}' ]]; then check "hero_environment: apply POSTs the v* tag policy" ok; else check "hero_environment: apply POSTs the v* tag policy" "${post:-no POST}"; fi
}

# ---- main_ruleset ----

serve_main_ruleset() { # extra-rules-json
    serve "repos/$REPO/rulesets" '[{"id": 7, "name": "main-ci-gate", "target": "branch"}]'
    serve "repos/$REPO/rulesets/7" "$(jq -n --argjson extra "$1" '{
        id: 7, name: "main-ci-gate", target: "branch", enforcement: "active",
        conditions: {ref_name: {include: ["~DEFAULT_BRANCH"], exclude: []}},
        rules: ([{type: "deletion"}, {type: "non_fast_forward"},
                 {type: "required_status_checks", parameters: {strict_required_status_checks_policy: false,
                    required_status_checks: [{context: "audit"}, {context: "backend"}, {context: "commitlint"},
                                             {context: "deploy-smoke"}, {context: "frontend"}, {context: "gitleaks"}]}}]
                + $extra)}')"
}

PULL_REQUEST_RULE='{"type": "pull_request", "parameters": {"required_approving_review_count": 0,
    "dismiss_stale_reviews_on_push": false, "require_code_owner_review": false,
    "require_last_push_approval": false, "required_review_thread_resolution": false}}'

case_main_ruleset_without_pull_request_drifts() {
    fresh_state
    serve_main_ruleset '[]'
    expect_status "main_ruleset: no pull_request rule is DRIFT" 1 "$(settings check main_ruleset)"
}

case_main_ruleset_requiring_an_approval_drifts() {
    fresh_state
    serve_main_ruleset "[$(jq -c '.parameters.required_approving_review_count = 1' <<< "$PULL_REQUEST_RULE")]"
    expect_status "main_ruleset: a required approval is DRIFT" 1 "$(settings check main_ruleset)"
}

case_main_ruleset_tolerates_an_unrelated_rule() {
    fresh_state
    serve_main_ruleset "[$PULL_REQUEST_RULE, {\"type\": \"required_linear_history\"}]"
    expect_status "main_ruleset: complete, plus an unrelated rule, is OK" 0 "$(settings check main_ruleset)"
}

case_main_ruleset_apply_adds_pull_request() {
    fresh_state
    serve_main_ruleset '[{"type": "required_linear_history"}]'
    settings apply main_ruleset > /dev/null
    local put; put="$(request PUT "repos/$REPO/rulesets/7")"
    if jq -e '
        ([.rules[] | select(.type == "pull_request") | .parameters.required_approving_review_count] == [0])
        and ([.rules[].type] | sort == ["deletion", "non_fast_forward", "pull_request", "required_linear_history", "required_status_checks"])
        and ([.rules[] | select(.type == "required_status_checks") | .parameters.required_status_checks[].context] | length == 6)' \
        <<< "${put:-null}" > /dev/null; then
        check "main_ruleset: apply adds pull_request and keeps every other rule" ok
    else
        check "main_ruleset: apply adds pull_request and keeps every other rule" "${put:-no PUT}"
    fi
}

# ---- release_tags ----

release_tags_body() { # app-id -> the ruleset the setting specifies
    jq -n --argjson app "$1" '{
        name: "release-tags", target: "tag", enforcement: "active",
        conditions: {ref_name: {include: ["refs/tags/v*"], exclude: []}},
        rules: [{type: "creation"}, {type: "update"}, {type: "deletion"}],
        bypass_actors: [
            {actor_id: 4, actor_type: "RepositoryRole", bypass_mode: "always"},
            {actor_id: 5, actor_type: "RepositoryRole", bypass_mode: "always"},
            {actor_id: $app, actor_type: "Integration", bypass_mode: "always"}]}'
}

normalised() { jq -S '{name, target, enforcement, conditions,
    rules: (.rules | sort_by(.type)), bypass_actors: (.bypass_actors | sort_by(.actor_type, .actor_id))}'; }

serve_release_tags() { # ruleset-json
    serve "repos/$REPO/rulesets" '[{"id": 9, "name": "release-tags", "target": "tag"}]'
    serve "repos/$REPO/rulesets/9" "$(jq '. + {id: 9}' <<< "$1")"
}

case_release_tags_matching_is_ok() {
    fresh_state
    serve_release_tags "$(release_tags_body "$APP_ID")"
    expect_status "release_tags: the specified ruleset is OK" 0 "$(RELEASE_APP_ID=$APP_ID settings check release_tags)"
}

case_release_tags_extra_bypass_actor_drifts() {
    fresh_state
    serve_release_tags "$(release_tags_body "$APP_ID" | jq '.bypass_actors += [{actor_id: 29110, actor_type: "Integration", bypass_mode: "always"}]')"
    expect_status "release_tags: an extra bypass actor is DRIFT" 1 "$(RELEASE_APP_ID=$APP_ID settings check release_tags)"
}

case_release_tags_missing_drifts_and_apply_creates_it() {
    fresh_state
    serve "repos/$REPO/rulesets" '[]'
    expect_status "release_tags: a missing ruleset is DRIFT" 1 "$(RELEASE_APP_ID=$APP_ID settings check release_tags)"
    [[ "$(writes)" == 0 ]] || check "release_tags: check writes nothing" "$(writes) write(s)"
    RELEASE_APP_ID=$APP_ID settings apply release_tags > /dev/null
    local post; post="$(request POST "repos/$REPO/rulesets")"
    if [[ -n "$post" && "$(normalised <<< "$post")" == "$(release_tags_body "$APP_ID" | normalised)" ]]; then check "release_tags: apply POSTs the specified ruleset" ok; else check "release_tags: apply POSTs the specified ruleset" "${post:-no POST}"; fi
}

case_release_tags_without_app_id_is_unreadable() {
    fresh_state
    serve_release_tags "$(release_tags_body "$APP_ID")"
    expect_status "release_tags: no RELEASE_APP_ID is exit 3" 3 "$(unset RELEASE_APP_ID; settings check release_tags)"
}

case_hero_environment_matching_is_ok
case_hero_environment_without_tag_policy_drifts
case_hero_environment_any_branch_drifts
case_hero_environment_apply_sends_the_full_payload
case_main_ruleset_without_pull_request_drifts
case_main_ruleset_requiring_an_approval_drifts
case_main_ruleset_tolerates_an_unrelated_rule
case_main_ruleset_apply_adds_pull_request
case_release_tags_matching_is_ok
case_release_tags_extra_bypass_actor_drifts
case_release_tags_missing_drifts_and_apply_creates_it
case_release_tags_without_app_id_is_unreadable

if ((failures > 0)); then
    echo "test-github-settings: $failures case(s) misbehaved"
    exit 1
fi
echo "test-github-settings: every case behaved"
