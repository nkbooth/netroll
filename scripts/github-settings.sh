#!/usr/bin/env bash
# SPDX-License-Identifier: RPL-1.5
# Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
# Repository settings as a script with a check mode, not a memory.
#
# Every setting below is a property of ONE GitHub repository, and the same
# set has to hold on two of them: the private archive today and the public
# `nkbooth/netroll` once it exists. A runbook would be transcribed twice and
# drift; this reads each setting back through `gh api` so `check` can prove
# the state instead of asserting it.
#
#   scripts/github-settings.sh check <owner/repo> [setting...]
#   scripts/github-settings.sh apply <owner/repo> [setting...]
#
# `check` prints one line per setting — OK, DRIFT or DEFERRED with have/want —
# and exits 1 if anything is DRIFT. `apply` writes every DRIFT setting to its
# target and then re-reads it. Both are idempotent. Name settings to limit
# the run to those; `sha_pinning` is the one you will want to run alone:
#
#   ORDER MATTERS ONCE. `sha_pinning` makes GitHub reject, at parse time, any
#   workflow whose `uses:` is a tag. Flip it before the SHA-pinned commit is
#   on the branch CI reads and every run fails. So: apply everything except
#   it, push the pins, watch CI go green, then `apply <repo> sha_pinning`.
#   That ordering is ENFORCED, not just documented: `sha_pinning` is excluded
#   from a bare run and must be named on the command line. A bare `apply`
#   prints one SKIPPED line for it so the omission is visible, never silent.
#
# DEFERRED means GitHub only allows the setting on a PUBLIC repository —
# private vulnerability reporting, and environment required reviewers on
# Free/Pro/Team plans — so on a private repository it is neither a pass nor
# a failure. It becomes applicable the moment the repository is public.
#
# Callers: `apply nkbooth/netroll` runs after the first public push (that is
# when the DEFERRED items become applicable, and when the `main-ci-gate`
# ruleset must be created — rulesets do not travel with a repository, so on a
# repository without one this script reports the gap rather than inventing
# the check list). `check nkbooth/netroll` is part of the release-readiness
# list.
#
# Contains no secret and no hostname: every value is a boolean, a rule name,
# or the maintainer's public GitHub user id.
set -euo pipefail

MODE="${1:-}"
REPO="${2:-}"
shift 2 2>/dev/null || true
ONLY=("$@")

if [[ ( "$MODE" != "check" && "$MODE" != "apply" ) || -z "$REPO" ]]; then
    echo "usage: $0 <check|apply> <owner/repo> [setting...]" >&2
    echo "  exit 0 = all selected settings OK or DEFERRED" >&2
    echo "  exit 1 = at least one DRIFT" >&2
    echo "  exit 2 = bad usage or unknown setting name" >&2
    echo "  exit 3 = at least one setting could not be read (never reported as drift)" >&2
    exit 2
fi

MAINTAINER_ID=68024347 # nkbooth
RULESET_NAME="main-ci-gate"
# `main_ruleset` runs BEFORE `repo_flags` deliberately: `repo_flags` enables
# auto-merge, and auto-merge on a repository with no required-check ruleset
# merges Dependabot PRs with zero checks. `write_repo_flags` refuses in that
# state, and this order means the ruleset is in place by the time it is asked.
ALL_SETTINGS=(actions_allowed workflow_token vulnerability_alerts security_updates main_ruleset release_tags repo_flags private_vulnerability_reporting hero_environment sha_pinning)

# Excluded from a bare run; must be named explicitly. See "ORDER MATTERS ONCE".
EXPLICIT_ONLY=(sha_pinning)

if ! IS_PRIVATE="$(gh api "repos/$REPO" --jq .private 2>&1)"; then
    echo "$0: cannot read repos/$REPO — $IS_PRIVATE" >&2
    echo "     (is \`gh\` authenticated, and does the repository exist?)" >&2
    exit 3
fi
DRIFT=0
UNREADABLE=0

report() { # status name have want
    printf '%-8s %-32s (have=%s want=%s)\n' "$1" "$2" "$3" "$4"
    [[ "$1" == "DRIFT" ]] && DRIFT=1
    [[ "$1" == "ERROR" ]] && UNREADABLE=1
    return 0
}

# `gh api` with the two failure modes told apart: a 404 usually MEANS something
# (the feature is off) and is the caller's to interpret, while anything else —
# no network, no auth, a rate limit — means the setting was never inspected and
# must never be reported as drift. Returns 44 for not-found, 1 for unreachable.
gh_get() { # path jq-expr
    local out rc
    out="$(gh api "$1" --jq "$2" 2>&1)" && { printf '%s' "$out"; return 0; }
    rc=$?
    [[ "$out" == *"HTTP 404"* || "$out" == *"Not Found"* ]] && return 44
    printf '%s' "$out" >&2
    return "$rc"
}

# --- actions_allowed: Actions enabled, any action may run (sha_pinning is the control) ---
read_actions_allowed() { gh_get "repos/$REPO/actions/permissions" '"enabled=\(.enabled) allowed_actions=\(.allowed_actions)"'; }
want_actions_allowed="enabled=true allowed_actions=all"
write_actions_allowed() {
    local pin; pin="$(gh api "repos/$REPO/actions/permissions" --jq .sha_pinning_required)"
    gh api -X PUT "repos/$REPO/actions/permissions" -F enabled=true -f allowed_actions=all -F "sha_pinning_required=$pin" --silent
}

# --- sha_pinning: every `uses:` must be a full-length commit SHA (server-side backstop) ---
read_sha_pinning() { gh_get "repos/$REPO/actions/permissions" '"sha_pinning_required=\(.sha_pinning_required)"'; }
want_sha_pinning="sha_pinning_required=true"
write_sha_pinning() {
    local allowed; allowed="$(gh api "repos/$REPO/actions/permissions" --jq .allowed_actions)"
    gh api -X PUT "repos/$REPO/actions/permissions" -F enabled=true -f "allowed_actions=$allowed" -F sha_pinning_required=true --silent
}

# --- workflow_token: GITHUB_TOKEN read-only by default; Actions may not approve PRs ---
read_workflow_token() { gh_get "repos/$REPO/actions/permissions/workflow" '"default_workflow_permissions=\(.default_workflow_permissions) can_approve_pull_request_reviews=\(.can_approve_pull_request_reviews)"'; }
want_workflow_token="default_workflow_permissions=read can_approve_pull_request_reviews=false"
write_workflow_token() { gh api -X PUT "repos/$REPO/actions/permissions/workflow" -f default_workflow_permissions=read -F can_approve_pull_request_reviews=false --silent; }

# --- repo_flags: auto-merge available (Dependabot workflow needs it); Discussions on (issue form routes questions there) ---
read_repo_flags() { gh_get "repos/$REPO" '"allow_auto_merge=\(.allow_auto_merge) has_discussions=\(.has_discussions)"'; }
want_repo_flags="allow_auto_merge=true has_discussions=true"
write_repo_flags() {
    # Auto-merge without a required-check ruleset merges Dependabot PRs on zero
    # checks. `write_main_ruleset` cannot create the ruleset (they do not
    # export), so this refuses rather than opening that window.
    if [[ -z "$(ruleset_id "$RULESET_NAME")" ]]; then
        echo "ERROR repo_flags: refusing to enable allow_auto_merge while no ruleset named" >&2
        echo "      '$RULESET_NAME' exists on $REPO — dependabot-auto-merge.yml would then" >&2
        echo "      merge third-party action bumps into main with nothing gating them." >&2
        echo "      Create the ruleset with its required status checks first, then re-run." >&2
        return 1
    fi
    gh api -X PATCH "repos/$REPO" -F allow_auto_merge=true -F has_discussions=true --silent
}

# --- vulnerability_alerts: Dependabot alerts (GET is 204 when on, 404 when off) ---
read_vulnerability_alerts() { if gh api "repos/$REPO/vulnerability-alerts" --silent 2>/dev/null; then echo enabled; else echo disabled; fi; }
want_vulnerability_alerts="enabled"
write_vulnerability_alerts() { gh api -X PUT "repos/$REPO/vulnerability-alerts" --silent; }

# --- security_updates: Dependabot security updates for cargo and npm (a toggle, not a dependabot.yml entry) ---
read_security_updates() {
    # 404 here means Dependabot alerts are off, which is a real answer, not a
    # read failure — the feature cannot be enabled without them.
    local v rc
    v="$(gh_get "repos/$REPO/automated-security-fixes" '"enabled=\(.enabled)"')" && { printf '%s' "$v"; return 0; }
    rc=$?
    [[ "$rc" -eq 44 ]] && { printf 'enabled=false'; return 0; }
    return "$rc"
}
want_security_updates="enabled=true"
write_security_updates() { gh api -X PUT "repos/$REPO/automated-security-fixes" --silent; }

# --- main_ruleset: the six required checks BY NAME, a PR with no required approval, no force-push, no deletion ---
#
# The CONTEXTS are asserted here, not just the rule types. ci.yml's header says
# renaming any of the required job names silently un-gates merges; a check that
# compared only `[.rules[].type]` would report OK on a `required_status_checks`
# rule whose context list had been emptied, which is the exact failure it
# exists to catch. The WRITE still reads the contexts rather than setting them
# (see write_main_ruleset) — reading is where they must be named.
#
# Each wanted rule is read on its own, and a rule this script does not manage
# is ignored: comparing the whole sorted type list meant one extra rule added
# in the web UI made `apply` report DRIFT forever, since it never removes rules.
#
# The pull request needs no approval because a sole maintainer cannot approve
# their own PR, so a count of one would wedge every merge. The gate is that
# `main` changes only through a PR whose six checks are green.
ruleset_id() { gh_get "repos/$REPO/rulesets" ".[] | select(.name==\"$1\") | .id"; }
read_main_ruleset() {
    local id; id="$(ruleset_id "$RULESET_NAME")" || return 1
    if [[ -z "$id" ]]; then echo "absent"; return 0; fi
    gh_get "repos/$REPO/rulesets/$id" '
        def has($t): any(.rules[]; .type == $t);
        "deletion=\(has("deletion")) non_fast_forward=\(has("non_fast_forward")) approvals=" +
        ([.rules[] | select(.type=="pull_request") | .parameters.required_approving_review_count | tostring]
            | if length == 0 then "no-pr" else join(",") end) + " checks=" +
        ([.rules[] | select(.type=="required_status_checks")
                   | .parameters.required_status_checks[].context] | sort | join(","))'
}
want_main_ruleset="deletion=true non_fast_forward=true approvals=0 checks=audit,backend,commitlint,deploy-smoke,frontend,gitleaks"
write_main_ruleset() {
    local id; id="$(ruleset_id "$RULESET_NAME")"
    if [[ -z "$id" ]]; then
        echo "ERROR main_ruleset: no ruleset named '$RULESET_NAME' on $REPO. Rulesets do not export;" >&2
        echo "      create it with the required status checks first, then re-run." >&2
        return 1
    fi
    # Carry name/target/enforcement/conditions/bypass_actors verbatim; keep every
    # existing rule (the status-check contexts are read, never hard-coded), add
    # the two branch-protection rules, and set the PR rule's approval count while
    # keeping any other PR parameter already chosen.
    gh api "repos/$REPO/rulesets/$id" | jq '{
        name, target, enforcement,
        conditions,
        bypass_actors: (.bypass_actors // []),
        rules: ((.rules | map(select(.type != "non_fast_forward" and .type != "deletion" and .type != "pull_request")))
                + [{type: "non_fast_forward"}, {type: "deletion"},
                   {type: "pull_request", parameters: (
                       {dismiss_stale_reviews_on_push: false, require_code_owner_review: false,
                        require_last_push_approval: false, required_review_thread_resolution: false}
                       + ([.rules[] | select(.type == "pull_request") | .parameters][0] // {})
                       + {required_approving_review_count: 0})}])
    }' | gh api -X PUT "repos/$REPO/rulesets/$id" --input - --silent
}

# --- release_tags: only writers, admins and the release App create, move or delete a v* tag ---
#
# A `v*` tag is what deploys (hero_environment admits only `v*` tags), so this
# keeps every workflow running on GITHUB_TOKEN, and every other App, from
# minting one. Unlike `main-ci-gate` it is fully specified here, so `apply`
# creates it. RELEASE_APP_ID is the release App's numeric id (its settings
# page), not the client id the workflow uses; it is read from the environment
# so this file names no App.
#
# Role ids 4 (write) and 5 (admin) are not in GitHub's REST reference; that gap
# is open as github/rest-api-description#4406, and the values come from the
# integrations/github Terraform provider's `github_repository_ruleset` docs.
# The web UI's bypass list shows them by name, which is the check to make.
RELEASE_RULESET_NAME="release-tags"
release_tags_spec() {
    jq -n --argjson app "$RELEASE_APP_ID" '{
        name: "release-tags", target: "tag", enforcement: "active",
        conditions: {ref_name: {include: ["refs/tags/v*"], exclude: []}},
        rules: [{type: "creation"}, {type: "update"}, {type: "deletion"}],
        bypass_actors: [
            {actor_id: 4, actor_type: "RepositoryRole", bypass_mode: "always"},
            {actor_id: 5, actor_type: "RepositoryRole", bypass_mode: "always"},
            {actor_id: $app, actor_type: "Integration", bypass_mode: "always"}]}'
}
RELEASE_TAGS_SUMMARY='"target=\(.target) enforcement=\(.enforcement) include=\(.conditions.ref_name.include | sort | join(",")) exclude=\(.conditions.ref_name.exclude | sort | join(",")) rules=\([.rules[].type] | sort | join(",")) bypass=\([.bypass_actors[]? | "\(.actor_type):\(.actor_id):\(.bypass_mode)"] | sort | join(","))"'
read_release_tags() {
    if [[ ! "${RELEASE_APP_ID:-}" =~ ^[0-9]+$ ]]; then
        echo "release_tags: set RELEASE_APP_ID to the release App's numeric id" >&2
        return 1
    fi
    local id; id="$(ruleset_id "$RELEASE_RULESET_NAME")" || return 1
    if [[ -z "$id" ]]; then echo "absent"; return 0; fi
    gh_get "repos/$REPO/rulesets/$id" "$RELEASE_TAGS_SUMMARY"
}
want_release_tags="$(if [[ "${RELEASE_APP_ID:-}" =~ ^[0-9]+$ ]]; then release_tags_spec | jq -r "$RELEASE_TAGS_SUMMARY"; fi)"
write_release_tags() {
    local id; id="$(ruleset_id "$RELEASE_RULESET_NAME")"
    if [[ -z "$id" ]]; then
        release_tags_spec | gh api -X POST "repos/$REPO/rulesets" --input - --silent
    else
        release_tags_spec | gh api -X PUT "repos/$REPO/rulesets/$id" --input - --silent
    fi
}

# --- private_vulnerability_reporting: public repositories only ---
deferred_private_vulnerability_reporting() { [[ "$IS_PRIVATE" == "true" ]]; }
read_private_vulnerability_reporting() {
    local v rc
    v="$(gh_get "repos/$REPO/private-vulnerability-reporting" '"enabled=\(.enabled)"')" && { printf '%s' "$v"; return 0; }
    rc=$?
    [[ "$rc" -eq 44 ]] && { printf 'enabled=false'; return 0; }
    return "$rc"
}
want_private_vulnerability_reporting="enabled=true"
write_private_vulnerability_reporting() { gh api -X PUT "repos/$REPO/private-vulnerability-reporting" --silent; }

# --- hero_environment: `hero-production`, the maintainer as required reviewer, `v*` tags only (public-only on Free/Pro/Team) ---
#
# The PUT replaces the environment, so every field is sent: a payload naming
# only the reviewer resets the deployment policy to "any ref may deploy".
# Both of deploy.yml's triggers run on a tag ref, so a dispatch from a branch
# is refused before a reviewer is even asked.
ENV_PATH_HERO="environments/hero-production"
deferred_hero_environment() { [[ "$IS_PRIVATE" == "true" ]]; }
read_hero_environment() {
    local env policies rc
    env="$(gh_get "repos/$REPO/$ENV_PATH_HERO" '
        "reviewers=" + ([.protection_rules[]? | select(.type=="required_reviewers") | .reviewers[]?.reviewer.id | tostring] | sort | join(",")) +
        " refs=" + (.deployment_branch_policy | if . == null then "any" elif .custom_branch_policies then "custom" else "protected" end)')" || {
        rc=$?; [[ "$rc" -eq 44 ]] && { echo "absent"; return 0; }; return "$rc"; }
    policies="$(gh_get "repos/$REPO/$ENV_PATH_HERO/deployment-branch-policies" \
        '[.branch_policies[] | "\(.type):\(.name)"] | sort | join(",")')" || {
        rc=$?; [[ "$rc" -eq 44 ]] || return "$rc"; policies=""; }
    printf '%s policies=%s' "$env" "$policies"
}
want_hero_environment="reviewers=$MAINTAINER_ID refs=custom policies=tag:v*"
write_hero_environment() {
    jq -n --argjson id "$MAINTAINER_ID" '{
        wait_timer: 0,
        reviewers: [{type: "User", id: $id}],
        deployment_branch_policy: {protected_branches: false, custom_branch_policies: true}}' \
        | gh api -X PUT "repos/$REPO/$ENV_PATH_HERO" --input - --silent
    local policies stale rc
    policies="$(gh_get "repos/$REPO/$ENV_PATH_HERO/deployment-branch-policies" '.branch_policies')" || {
        rc=$?; [[ "$rc" -eq 44 ]] || return "$rc"; policies='[]'; }
    for stale in $(jq -r '.[] | select(.type != "tag" or .name != "v*") | .id' <<< "$policies"); do
        gh api -X DELETE "repos/$REPO/$ENV_PATH_HERO/deployment-branch-policies/$stale" --silent
    done
    if [[ "$(jq '[.[] | select(.type == "tag" and .name == "v*")] | length' <<< "$policies")" == 0 ]]; then
        jq -n '{name: "v*", type: "tag"}' \
            | gh api -X POST "repos/$REPO/$ENV_PATH_HERO/deployment-branch-policies" --input - --silent
    fi
}

known_setting() {
    local s; for s in "${ALL_SETTINGS[@]}"; do [[ "$s" == "$1" ]] && return 0; done
    return 1
}

explicit_only() {
    local s; for s in "${EXPLICIT_ONLY[@]}"; do [[ "$s" == "$1" ]] && return 0; done
    return 1
}

# An unrecognised name used to match nothing, run nothing and exit 0 — a green
# release-readiness gate that verified no settings at all. Names are validated
# up front instead.
for s in "${ONLY[@]}"; do
    if ! known_setting "$s"; then
        echo "$0: unknown setting '$s'" >&2
        echo "     known: ${ALL_SETTINGS[*]}" >&2
        exit 2
    fi
done

selected() {
    if [[ ${#ONLY[@]} -eq 0 ]]; then
        explicit_only "$1" && return 1
        return 0
    fi
    local s; for s in "${ONLY[@]}"; do [[ "$s" == "$1" ]] && return 0; done
    return 1
}

run_one() {
    local name="$1" have want
    want="$(eval "printf '%s' \"\$want_$name\"")"
    if declare -F "deferred_$name" >/dev/null && "deferred_$name"; then
        report DEFERRED "$name" "public repository only" "$want"
        return 0
    fi
    have="$("read_$name")" || { report ERROR "$name" "unreadable" "$want"; return 0; }
    if [[ "$have" == "$want" ]]; then report OK "$name" "$have" "$want"; return 0; fi
    if [[ "$MODE" == "check" ]]; then report DRIFT "$name" "$have" "$want"; return 0; fi
    if ! "write_$name"; then report DRIFT "$name" "$have" "$want"; return 0; fi
    have="$("read_$name")" || { report ERROR "$name" "unreadable after write" "$want"; return 0; }
    if [[ "$have" == "$want" ]]; then report OK "$name" "$have" "$want (applied)"; else report DRIFT "$name" "$have" "$want"; fi
}

for s in "${ALL_SETTINGS[@]}"; do
    if ! selected "$s"; then
        if [[ ${#ONLY[@]} -eq 0 ]] && explicit_only "$s"; then
            report SKIPPED "$s" "not in a bare run" "name it explicitly"
        fi
        continue
    fi
    run_one "$s"
done

# 3 beats 1: a run that could not inspect a setting is a different answer from
# a run that inspected it and found drift, and the readiness gate has to tell
# them apart.
if [[ "$UNREADABLE" -eq 1 ]]; then exit 3; fi
if [[ "$DRIFT" -eq 1 ]]; then exit 1; fi
exit 0
