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
# is appended to requests.jsonl and changes nothing. A request matching a
# "METHOD path [after-n]" line in $GH_STUB_STATE/fail answers HTTP 500, after
# n successful answers when a count is given.
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
if [[ -f "$GH_STUB_STATE/fail" ]]; then
    while read -r fm fp after; do
        [[ "$fm" == "$method" && "$fp" == "$path" ]] || continue
        seen="$GH_STUB_STATE/seen.$(printf '%s' "$method $path" | md5sum | cut -c1-8)"
        n=$(( $(cat "$seen" 2>/dev/null || echo 0) + 1 ))
        echo "$n" > "$seen"
        if (( n > ${after:-0} )); then echo "gh: Server Error (HTTP 500)" >&2; exit 1; fi
    done < "$GH_STUB_STATE/fail"
fi
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
fail_on() { echo "$*" >> "$GH_STUB_STATE/fail"; } # METHOD path [after-n]

# The stub persists no write, so a clean `apply` re-reads the old state and
# ends DRIFT (exit 1). What tells a clean write from a failed one is the
# report line, which names a write that failed.
expect_clean_apply() { # name status
    if [[ "$2" != 1 ]]; then check "$1" "exit $2, want 1 (DRIFT after an unpersisted write)"
    elif grep -q 'write failed' "$WORK/out"; then check "$1" "a write failed"
    else check "$1" ok; fi
}
expect_failed_write() { # name status
    if [[ "$2" == 1 ]] && grep -q 'write failed' "$WORK/out"; then check "$1" ok; else check "$1" "exit $2 without a 'write failed' report"; fi
}

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
    serve "$ENV_PATH/deployment-branch-policies" '{"total_count": 1, "branch_policies": [{"id": 1, "name": "v*", "type": "tag"}]}'
    expect_status "hero_environment: any-ref deploys are DRIFT" 1 "$(settings check hero_environment)"
}

case_hero_environment_apply_sends_the_full_payload() {
    fresh_state
    expect_clean_apply "hero_environment: apply reports no failed write" "$(settings apply hero_environment)"
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
    expect_clean_apply "main_ruleset: apply reports no failed write" "$(settings apply main_ruleset)"
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
    expect_clean_apply "release_tags: apply reports no failed write" "$(RELEASE_APP_ID=$APP_ID settings apply release_tags)"
    local post; post="$(request POST "repos/$REPO/rulesets")"
    if [[ -n "$post" && "$(normalised <<< "$post")" == "$(release_tags_body "$APP_ID" | normalised)" ]]; then check "release_tags: apply POSTs the specified ruleset" ok; else check "release_tags: apply POSTs the specified ruleset" "${post:-no POST}"; fi
}

case_release_tags_without_app_id_is_unreadable() {
    fresh_state
    serve_release_tags "$(release_tags_body "$APP_ID")"
    expect_status "release_tags: no RELEASE_APP_ID is exit 3" 3 "$(unset RELEASE_APP_ID; settings check release_tags)"
}

case_hero_environment_apply_deletes_stale_policies() {
    fresh_state
    serve_hero_environment '{"total_count": 2, "branch_policies": [{"id": 1, "name": "v*", "type": "tag"}, {"id": 3, "name": "main", "type": "branch"}]}'
    serve "$ENV_PATH" '{"protection_rules": [], "deployment_branch_policy": null}'
    expect_clean_apply "hero_environment: apply with a stale policy reports no failed write" "$(settings apply hero_environment)"
    if [[ "$(jq -sc '[.[] | select(.method == "DELETE") | .path]' "$GH_STUB_STATE/requests.jsonl")" == "[\"$ENV_PATH/deployment-branch-policies/3\"]" ]]; then
        check "hero_environment: apply deletes exactly the stale policy" ok
    else
        check "hero_environment: apply deletes exactly the stale policy" "$(cat "$GH_STUB_STATE/requests.jsonl")"
    fi
    [[ -z "$(request POST "$ENV_PATH/deployment-branch-policies")" ]] && check "hero_environment: an existing v* policy is not re-posted" ok \
        || check "hero_environment: an existing v* policy is not re-posted" "POSTed"
}

case_hero_environment_failed_put_stops_the_write() {
    fresh_state
    fail_on PUT "$ENV_PATH"
    expect_failed_write "hero_environment: a failed PUT is reported" "$(settings apply hero_environment)"
    [[ "$(writes)" == 0 ]] && check "hero_environment: nothing follows a failed PUT" ok \
        || check "hero_environment: nothing follows a failed PUT" "$(cat "$GH_STUB_STATE/requests.jsonl")"
}

case_hero_environment_failed_policy_post_is_reported() {
    fresh_state
    fail_on POST "$ENV_PATH/deployment-branch-policies"
    expect_failed_write "hero_environment: a failed tag-policy POST is reported" "$(settings apply hero_environment)"
}

case_main_ruleset_disabled_drifts() {
    fresh_state
    serve_main_ruleset "[$PULL_REQUEST_RULE]"
    serve "repos/$REPO/rulesets/7" "$(jq '.enforcement = "disabled"' "$GH_STUB_STATE/api/repos/$REPO/rulesets/7.json")"
    expect_status "main_ruleset: a disabled ruleset is DRIFT" 1 "$(settings check main_ruleset)"
}

case_main_ruleset_not_targeting_main_drifts() {
    fresh_state
    serve_main_ruleset "[$PULL_REQUEST_RULE]"
    serve "repos/$REPO/rulesets/7" "$(jq '.conditions.ref_name.include = ["refs/heads/release"]' "$GH_STUB_STATE/api/repos/$REPO/rulesets/7.json")"
    expect_status "main_ruleset: a ruleset not covering main is DRIFT" 1 "$(settings check main_ruleset)"
}

case_main_ruleset_apply_enforces_on_main() {
    fresh_state
    serve_main_ruleset "[$PULL_REQUEST_RULE]"
    serve "repos/$REPO/rulesets/7" "$(jq '.enforcement = "evaluate" | .conditions.ref_name.include = ["refs/heads/release"]' "$GH_STUB_STATE/api/repos/$REPO/rulesets/7.json")"
    expect_clean_apply "main_ruleset: apply on a disabled ruleset reports no failed write" "$(settings apply main_ruleset)"
    local put; put="$(request PUT "repos/$REPO/rulesets/7")"
    if jq -e '.enforcement == "active" and (.conditions.ref_name.include | index("~DEFAULT_BRANCH") != null)
        and (.conditions.ref_name.include | index("refs/heads/release") != null)' <<< "${put:-null}" > /dev/null; then
        check "main_ruleset: apply activates it and adds the default branch" ok
    else
        check "main_ruleset: apply activates it and adds the default branch" "${put:-no PUT}"
    fi
}

case_release_tags_apply_updates_an_existing_ruleset() {
    fresh_state
    serve_release_tags "$(release_tags_body "$APP_ID" | jq '.rules = [{type: "creation"}]')"
    expect_clean_apply "release_tags: apply on a drifted ruleset reports no failed write" "$(RELEASE_APP_ID=$APP_ID settings apply release_tags)"
    local put; put="$(request PUT "repos/$REPO/rulesets/9")"
    if [[ -n "$put" && "$(normalised <<< "$put")" == "$(release_tags_body "$APP_ID" | normalised)" && -z "$(request POST "repos/$REPO/rulesets")" ]]; then
        check "release_tags: apply PUTs the specified ruleset over the existing one" ok
    else
        check "release_tags: apply PUTs the specified ruleset over the existing one" "$(cat "$GH_STUB_STATE/requests.jsonl")"
    fi
}

case_release_tags_failed_lookup_creates_nothing() {
    fresh_state
    serve "repos/$REPO/rulesets" '[]'
    fail_on GET "repos/$REPO/rulesets" 1
    expect_failed_write "release_tags: a failed lookup during apply is reported" "$(RELEASE_APP_ID=$APP_ID settings apply release_tags)"
    [[ "$(writes)" == 0 ]] && check "release_tags: a failed lookup creates no ruleset" ok \
        || check "release_tags: a failed lookup creates no ruleset" "$(cat "$GH_STUB_STATE/requests.jsonl")"
}

case_hero_environment_matching_is_ok
case_hero_environment_without_tag_policy_drifts
case_hero_environment_any_branch_drifts
case_hero_environment_apply_sends_the_full_payload
case_hero_environment_apply_deletes_stale_policies
case_hero_environment_failed_put_stops_the_write
case_hero_environment_failed_policy_post_is_reported
case_main_ruleset_without_pull_request_drifts
case_main_ruleset_requiring_an_approval_drifts
case_main_ruleset_tolerates_an_unrelated_rule
case_main_ruleset_apply_adds_pull_request
case_main_ruleset_disabled_drifts
case_main_ruleset_not_targeting_main_drifts
case_main_ruleset_apply_enforces_on_main
case_release_tags_matching_is_ok
case_release_tags_extra_bypass_actor_drifts
case_release_tags_missing_drifts_and_apply_creates_it
case_release_tags_apply_updates_an_existing_ruleset
case_release_tags_failed_lookup_creates_nothing
case_release_tags_without_app_id_is_unreadable

if ((failures > 0)); then
    echo "test-github-settings: $failures case(s) misbehaved"
    exit 1
fi
echo "test-github-settings: every case behaved"
