#!/usr/bin/env bash
# SPDX-License-Identifier: RPL-1.5
# Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
# Self-test for scripts/lint-comments.sh and scripts/check-structure.sh: every
# rule is planted once in a throwaway repository and must fail by name, and
# every negative case must pass. A gate nobody has seen fail is not a gate.
#
# WHY the planted strings are assembled at runtime: a literal violation in this
# tracked file would fail the very gates it tests.
#
#   scripts/test-gates.sh        exit 0 = every case behaved, 1 = at least one did not
set -euo pipefail

# The developer's own git config (commit signing, diff.noprefix) must not decide
# whether a case passes; a case that wants a setting passes it explicitly.
export GIT_CONFIG_GLOBAL=/dev/null GIT_CONFIG_NOSYSTEM=1

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT
REPO="$WORK/repo"
failures=0

# Each scope the density rule measures gets one comment-free file, so a fresh
# repository is clean under every rule before a case plants anything.
fresh_repo() {
    rm -rf "$REPO"
    mkdir -p "$REPO/scripts" "$REPO/docs" "$REPO/deploy" "$REPO/frontend/src" \
        "$REPO/backend/crates/netroll-domain/src" "$REPO/backend/crates/netroll-adapters/src" \
        "$REPO/backend/crates/netroll-app/src"
    cp "$ROOT/scripts/lint-comments.sh" "$ROOT/scripts/check-structure.sh" \
        "$ROOT/scripts/check-structure.allow" "$REPO/scripts/"
    mkdir -p "$REPO/scripts/git-hooks"
    cp "$ROOT/scripts/git-hooks/pre-push" "$REPO/scripts/git-hooks/"
    # commitlint has cases of its own below; here it must not decide a push.
    printf '#!/bin/sh\nexit 0\n' >"$REPO/scripts/git-hooks/commit-msg"
    chmod +x "$REPO/scripts/git-hooks/commit-msg"
    local crate
    for crate in netroll-domain netroll-adapters netroll-app; do
        echo 'pub fn f() {}' >"$REPO/backend/crates/$crate/src/lib.rs"
    done
    echo 'export const f = 1;' >"$REPO/frontend/src/main.ts"
    echo 'target/' >"$REPO/.containerignore"
    cp "$REPO/.containerignore" "$REPO/.dockerignore"
    git -C "$REPO" init -q -b main
    git -C "$REPO" config user.email gate@example.com
    git -C "$REPO" config user.name gate
    commit_all baseline
}

commit_all() {
    git -C "$REPO" add -A
    git -C "$REPO" commit -q --no-verify -m "$1"
}

plant() {
    mkdir -p "$(dirname "$REPO/$1")"
    printf '%s\n' "$2" >"$REPO/$1"
    git -C "$REPO" add -A
}

run_gate() {
    local out status=0
    out="$(cd "$REPO" && "$@" 2>&1)" || status=$?
    printf '%s\n' "$out" >"$WORK/out"
    return "$status"
}

# Exit status only, for errors that have no rule name to report.
expect_status() {
    local name="$1" want="$2" status=0
    shift 2
    run_gate "$@" || status=$?
    if [[ "$status" -ne "$want" ]]; then fail "$name" "expected exit $want, got $status"; return; fi
    pass "$name"
}

pass() { echo "PASS  $1"; }
fail() { echo "FAIL  $1: $2"; sed 's/^/        /' "$WORK/out"; failures=$((failures + 1)); }

# The gate must exit 1 and name the rule in a `<location>: <rule>:` line.
expect_rule() {
    local name="$1" rule="$2" status=0
    shift 2
    run_gate "$@" || status=$?
    if [[ "$status" -ne 1 ]]; then fail "$name" "expected exit 1, got $status"; return; fi
    if ! grep -q ": $rule:" "$WORK/out"; then fail "$name" "no '$rule' hit reported"; return; fi
    pass "$name"
}

expect_clean() {
    local name="$1" status=0
    shift
    run_gate "$@" || status=$?
    if [[ "$status" -ne 0 ]]; then fail "$name" "expected exit 0, got $status"; return; fi
    pass "$name"
}

lint() { expect_rule "$1" "$2" scripts/lint-comments.sh; }
lint_clean() { expect_clean "$1" scripts/lint-comments.sh; }
structure() { expect_rule "$1" "$2" scripts/check-structure.sh; }
structure_clean() { expect_clean "$1" scripts/check-structure.sh; }

STORY="Sto""ry"
CGNAT="$(printf '%s.%s.0.1' 100 64)"
PRIVATE="$(printf '%s.%s.1.10' 192 168)"

# --- lint-comments.sh -------------------------------------------------------

fresh_repo
lint_clean "lint: a clean tree passes"

fresh_repo; plant docs/a.md "As decided in ${STORY} 13.4."
lint "lint: a capitalised citation in Markdown" citation

fresh_repo; plant docs/a.js "// fixes the flash (${STORY,,} 9.4)"
lint "lint: a lowercase citation in a comment" citation

fresh_repo; plant docs/a.md "Covers A""C 5 and the rest."
lint "lint: an acceptance-criterion id with a space" citation

fresh_repo; plant docs/a.md "Per F""R64."
lint "lint: a requirement id" citation

fresh_repo; plant docs/a.md "fix: handle AC power loss"
lint_clean "lint: AC with no number passes"

fresh_repo; plant CHANGELOG.md "* ${STORY} 13.4 shipped"
lint_clean "lint: CHANGELOG.md is excluded"

fresh_repo; plant docs/a.md "${STORY} 13.4"; plant docs/b.md "${STORY} 13.5"
run_gate scripts/lint-comments.sh || true
if [[ "$(grep -c ': citation:' "$WORK/out")" -eq 2 ]]; then
    pass "lint: every hit is reported, not just the first"
else
    fail "lint: every hit is reported, not just the first" "expected 2 citation lines"
fi

fresh_repo; plant backend/crates/netroll-app/src/x.rs "// keeps D""3 true"
lint "lint: a decision id on a Rust comment line" decision-id

fresh_repo; plant frontend/src/x.css "/* D""12 */"
lint "lint: a decision id on a CSS comment line" decision-id

fresh_repo; plant backend/crates/netroll-app/src/x.rs "$(printf '/*\n * see D%s\n */' 4)"
lint "lint: a decision id inside a block comment" decision-id

fresh_repo; plant backend/crates/netroll-app/src/x.rs 'const D''3: u8 = 3;'
lint_clean "lint: a decision id on a code line passes"

fresh_repo; plant docs/a.md "D""3 in prose"
lint_clean "lint: a decision id outside source comments passes"

# One plant per alternation, so a typo in any branch of a pattern is seen.
for cite in "ep""ic 3" "Ep""ics 4" "ep""ic 3a" "Stor""ies 4.7" "NF""R12" "A""R12" "UX-D""R19" "O""Q4" "F""R-64"; do
    fresh_repo; plant docs/a.md "As decided in $cite."
    lint "lint: the citation spelling '$cite'" citation
done

for ext in ts tsx js; do
    fresh_repo; plant "frontend/src/x.$ext" "// keeps D""3 true"
    lint "lint: a decision id on a .$ext comment line" decision-id
done

for name in "epics"".md" "project-context"".md"; do
    fresh_repo; plant docs/a.md "see $name"
    lint "lint: the planning file name '$name'" planning-file
done

fresh_repo; plant frontend/src/x.ts "// tracked in deferred""-work"
lint "lint: a planning file name" planning-file

fresh_repo; plant docs/a.md "see sprint-status"".yaml"
lint "lint: the sprint register's file name" planning-file

fresh_repo; plant backend/crates/netroll-domain/src/x.rs "$(printf '// %s\n' a b c d e f g h)"
lint "lint: a scope over its density ceiling" density

# The license notice heads every source file; counted as comment it would
# push every scope over its ceiling without a word of commentary being added.
fresh_repo
for i in 1 2 3 4 5 6 7 8; do
    printf '// SPDX-License-Identifier: RPL-1.5\n// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.\npub fn f%s() {}\n' "$i" \
        >"$REPO/backend/crates/netroll-domain/src/n$i.rs"
done
git -C "$REPO" add -A
lint_clean "lint: license notice lines are not counted as comments"

fresh_repo
printf 'pub fn a() {}\npub fn b() {}\npub fn c() {}\npub fn d() {}\npub fn e() {}\n%s\n' \
    "$(printf '// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.\n%.0s' 1 2 3 4 5 6 7 8)" \
    >"$REPO/backend/crates/netroll-domain/src/deep.rs"
git -C "$REPO" add -A
lint "lint: a notice-shaped line below a file's head still counts" density

# More files than one xargs batch, so the per-batch counts have to be summed:
# the comment-only files sort first and the code-only files last, and a run
# that kept only one batch's figures would report a different total.
fresh_repo
for i in $(seq -w 0 1999); do
    echo '// c' >"$REPO/backend/crates/netroll-domain/src/c$i.rs"
    echo 'fn z() {}' >"$REPO/backend/crates/netroll-domain/src/z$i.rs"
done
commit_all many
run_gate scripts/lint-comments.sh || true
if grep -q 'netroll-domain comment=2000 total=4001 ' "$WORK/out"; then
    pass "lint: density sums every xargs batch"
else
    fail "lint: density sums every xargs batch" "expected comment=2000 total=4001"
fi

# --- check-structure.sh -----------------------------------------------------

fresh_repo
structure_clean "structure: a clean tree passes"

fresh_repo; plant backend/crates/netroll-domain/src/x.rs "let ip = \"$CGNAT\";"
structure "structure: a CGNAT address" cgnat

fresh_repo; plant docs/a.md "Reach it at $CGNAT."
structure "structure: a CGNAT address before a full stop" cgnat

fresh_repo; plant docs/vendor.min.js "var a=\"$CGNAT\";"
structure_clean "structure: minified bundles are excluded"

fresh_repo; plant docs/a.md "ssh nas.tailcafe.ts"".net"
structure "structure: a tailnet host" host-suffix

fresh_repo; plant deploy/a.yml "host: db.intern""al"
structure "structure: an internal host suffix" host-suffix

fresh_repo; plant docs/a.md "print to printer.loc""al"
structure "structure: a .local host" host-suffix

fresh_repo; plant docs/a.md "open http://nas.intern""al:8080/"
structure "structure: a host in a URL" host-suffix

fresh_repo; plant docs/a.md "ssh user""@box.tailcafe.ts"".net"
structure "structure: a host after a user name" host-suffix

fresh_repo; plant docs/a.md "scp f user""@box.tailcafe.ts"".net:/srv"
structure "structure: an scp target" host-suffix

fresh_repo; plant docs/a.md "DATABASE_URL=postgres://app:pw""@db.corp.intern""al:5432/app"
structure "structure: a host in a credential URL" host-suffix

fresh_repo; plant docs/a.md "ssh NAS.LOC""AL"
structure "structure: an uppercase host suffix" host-suffix

fresh_repo; plant docs/a.md 'cookies for ${DOMAIN}.intern''al and *.loc''al'
structure_clean "structure: a templated or wildcard host passes"

fresh_repo; plant deploy/a.tpl "KEY=op:""//someone-else/item/field"
structure "structure: a foreign op vault" op-vault

fresh_repo; plant deploy/a.tpl "KEY=op:""//netroll-deploy/item/field"
structure_clean "structure: the project vault passes"

fresh_repo; plant docs/a.md "mail someone@""corp-mail.org"
structure "structure: an unlisted email" email

fresh_repo; plant docs/a.md "mail someone@""example.com or x@""y.test"
structure_clean "structure: reserved email domains pass"

fresh_repo; plant docs/a.md "mail nick@n1cck"".us"
structure_clean "structure: an allowlisted address passes"

fresh_repo; plant docs/a.md "git clone git@""github.com:owner/repo.git"
structure_clean "structure: an scp-style SSH URL is not an email"

fresh_repo; plant deploy/a.yml "addr: $PRIVATE"
structure "structure: a private address in deploy config" rfc1918

fresh_repo; plant .github/workflows/a.yml "addr: $PRIVATE"
structure "structure: a private address in a workflow" rfc1918

fresh_repo; plant docs/a.md "Reach it at $PRIVATE."
structure "structure: a private address in Markdown" rfc1918

fresh_repo; plant backend/crates/netroll-app/tests/x.rs "let ip = \"$PRIVATE\";"
structure_clean "structure: a private address in a backend fixture passes"

fresh_repo
rm "$REPO/.dockerignore"; ln -s .containerignore "$REPO/.dockerignore"; git -C "$REPO" add -A
structure "structure: a symlinked .dockerignore" ignore-files

fresh_repo; plant .dockerignore "node_modules/"
structure "structure: diverging ignore files" ignore-files

# Range mode reads every commit's added lines: a value added and then removed
# inside one push is still published, though the range's net diff is empty.
fresh_repo
base="$(git -C "$REPO" rev-parse HEAD)"
plant docs/a.md "Reach it at $CGNAT."; commit_all add
leaked="$(git -C "$REPO" rev-parse --short=12 HEAD)"
plant docs/a.md "Reach it via the gateway."; commit_all remove
structure_clean "structure: worktree mode sees only the final tree"
expect_rule "structure: range mode sees an added-then-removed value" cgnat \
    scripts/check-structure.sh --range "$base..HEAD"
if grep -q "^${leaked}[0-9a-f]*:docs/a.md:1: cgnat:" "$WORK/out"; then
    pass "structure: range mode names the commit, file and line"
else
    fail "structure: range mode names the commit, file and line" "expected $leaked…:docs/a.md:1"
fi

fresh_repo
base="$(git -C "$REPO" rev-parse HEAD)"
plant docs/a.md "nothing to see"; commit_all clean
expect_clean "structure: a clean range passes" scripts/check-structure.sh --range "$base..HEAD"

fresh_repo
base="$(git -C "$REPO" rev-parse HEAD)"
rm "$REPO/.dockerignore"; ln -s .containerignore "$REPO/.dockerignore"; commit_all link
expect_rule "structure: range mode checks the ignore files at its tip" ignore-files \
    scripts/check-structure.sh --range "$base..HEAD"

# A merge whose tree equals its first parent's must not hide the commits it
# brings in: history simplification would drop that whole side of the graph.
fresh_repo
base="$(git -C "$REPO" rev-parse HEAD)"
git -C "$REPO" checkout -q -b side
plant docs/a.md "Reach it at $CGNAT."; commit_all add
git -C "$REPO" rm -q docs/a.md; commit_all remove
git -C "$REPO" checkout -q main
git -C "$REPO" merge -q --no-ff --no-edit side
expect_rule "structure: range mode sees a value added and removed on a merged branch" cgnat \
    scripts/check-structure.sh --range "$base..HEAD"

fresh_repo
base="$(git -C "$REPO" rev-parse HEAD)"
git -C "$REPO" checkout -q -b side
plant docs/b.md "side"; commit_all side
git -C "$REPO" checkout -q main
git -C "$REPO" merge -q --no-ff --no-commit side >/dev/null
plant docs/c.md "Reach it at $CGNAT."
git -C "$REPO" commit -q --no-verify -m merge
expect_rule "structure: range mode sees a line the merge commit adds itself" cgnat \
    scripts/check-structure.sh --range "$base..HEAD"

fresh_repo
base="$(git -C "$REPO" rev-parse HEAD)"
plant deploy/a.yml "addr: $PRIVATE"; commit_all add
expect_rule "structure: range mode ignores the user's diff.noprefix" rfc1918 \
    env GIT_CONFIG_COUNT=1 GIT_CONFIG_KEY_0=diff.noprefix GIT_CONFIG_VALUE_0=true \
    scripts/check-structure.sh --range "$base..HEAD"

fresh_repo
base="$(git -C "$REPO" rev-parse HEAD)"
plant "docs/a b.md" "Reach it at $PRIVATE."; commit_all add
expect_rule "structure: range mode reads a path with a space in it" rfc1918 \
    scripts/check-structure.sh --range "$base..HEAD"

# Enough paths after the bad one that the listing outlives an early-exiting
# reader: the guard must not lose the match to SIGPIPE.
fresh_repo
plant "docs/a:b.md" "x"
for i in $(seq -w 0 3999); do echo x >"$REPO/z-file-with-a-long-name-$i.txt"; done
commit_all colon
expect_status "structure: a tracked path containing ':' is refused" 2 scripts/check-structure.sh

rm -rf "$WORK/nogit"; mkdir -p "$WORK/nogit/scripts"
cp "$ROOT/scripts/check-structure.sh" "$ROOT/scripts/check-structure.allow" "$WORK/nogit/scripts/"
status=0; (cd "$WORK/nogit" && scripts/check-structure.sh) >"$WORK/out" 2>&1 || status=$?
if [[ "$status" -eq 2 ]]; then pass "structure: a git failure exits 2"; else fail "structure: a git failure exits 2" "expected exit 2, got $status"; fi

fresh_repo; echo "op-vault" >>"$REPO/scripts/check-structure.allow"; commit_all allow
expect_status "structure: an allowlist line with no value is refused" 2 scripts/check-structure.sh

# --- pre-push ---------------------------------------------------------------

# gitleaks has its own CI step; a stub keeps these cases about the hook itself.
mkdir -p "$WORK/bin"
printf '#!/bin/sh\nexit 0\n' >"$WORK/bin/gitleaks"; chmod +x "$WORK/bin/gitleaks"
ZERO=0000000000000000000000000000000000000000

# Feeds one `<local ref> <local sha> <remote ref> <remote sha>` line to the
# hook, as git does, pushing to the remote named origin.
push_hook() {
    local name="$1" want="$2" refs="$3" status=0
    (cd "$REPO" && PATH="$WORK/bin:$PATH" scripts/git-hooks/pre-push origin "$WORK/origin.git" \
        <<<"$refs") >"$WORK/out" 2>&1 || status=$?
    if [[ "$status" -ne "$want" ]]; then fail "$name" "expected exit $want, got $status"; return; fi
    pass "$name"
}

fresh_remotes() {
    fresh_repo
    rm -rf "$WORK/origin.git" "$WORK/mirror.git"
    git init -q --bare "$WORK/origin.git"
    git init -q --bare "$WORK/mirror.git"
    git -C "$REPO" remote add origin "$WORK/origin.git"
    git -C "$REPO" remote add mirror "$WORK/mirror.git"
}

fresh_remotes
plant docs/a.md "nothing to see"; commit_all clean
head="$(git -C "$REPO" rev-parse HEAD)"
push_hook "pre-push: a clean new branch passes" 0 "refs/heads/x $head refs/heads/x $ZERO"
push_hook "pre-push: a wip/ branch is refused" 1 "refs/heads/wip/x $head refs/heads/wip/x $ZERO"
push_hook "pre-push: deleting a wip/ branch is allowed" 0 "(delete) $ZERO refs/heads/wip/x $head"

# A commit another remote already has is still unpublished on this one.
fresh_remotes
plant docs/a.md "Reach it at $CGNAT."; commit_all leak
git -C "$REPO" push -q --no-verify mirror HEAD:refs/heads/x
head="$(git -C "$REPO" rev-parse HEAD)"
push_hook "pre-push: a new branch is checked against the push target only" 1 \
    "refs/heads/x $head refs/heads/x $ZERO"

fresh_remotes
push_hook "pre-push: a commit that cannot be listed blocks the push" 1 \
    "refs/heads/x 1234567890123456789012345678901234567890 refs/heads/x $ZERO"

# --- commitlint -------------------------------------------------------------

# The hook's own runner: a host commitlint, else its pinned image.
COMMITLINT_IMAGE="$(sed -n 's/^COMMITLINT_IMAGE="\(.*\)"$/\1/p' "$ROOT/scripts/git-hooks/commit-msg")"
commitlint_run() { # [config]
    local config="${1:-commitlint.config.cjs}"
    if command -v commitlint >/dev/null 2>&1; then
        (cd "$ROOT" && commitlint --config "$config")
    elif command -v podman >/dev/null 2>&1; then
        podman run --rm -i --security-opt label=disable -v "$ROOT:/repo:ro" -w /repo \
            "$COMMITLINT_IMAGE" --config "$config"
    elif command -v docker >/dev/null 2>&1; then
        docker run --rm -i -v "$ROOT:/repo:ro" -w /repo "$COMMITLINT_IMAGE" --config "$config"
    else
        echo "neither commitlint, podman nor docker is available" >&2
        return 2
    fi
}

# Status 1 is commitlint's rejection; anything else means it did not run.
expect_message() { # name want message [config]
    local name="$1" want="$2" message="$3" config="${4:-}" status=0
    commitlint_run "$config" <<<"$message" >"$WORK/out" 2>&1 || status=$?
    if [[ "$status" -ne "$want" ]]; then fail "$name" "expected exit $want, got $status"; return; fi
    pass "$name"
}

expect_message "commitlint: a conventional message passes" 0 "fix: handle AC power loss"
expect_message "commitlint: a citation in the body is refused" 1 "$(printf 'fix: x\n\nCovers %s 13.4.' "$STORY")"
expect_message "commitlint: a hyphenated requirement id is refused" 1 "$(printf 'fix: x\n\nRefs F%s-64' R)"
expect_message "commitlint: a revert cannot carry a citation" 1 "Revert \"fix: covers ${STORY} 13.4\""
expect_message "commitlint: a fixup cannot carry a citation" 1 "fixup! fix: covers ${STORY} 13.4"
expect_message "commitlint: a GitHub pull request merge passes" 0 "Merge pull request #56 from owner/feat/x"
expect_message "commitlint: a GitHub update-branch merge passes" 0 "Merge branch 'main' into feat/x"
expect_message "commitlint: a local merge of a ref passes" 0 "Merge origin/main into feat/x"
expect_message "commitlint: a local merge of a remote-tracking branch passes" 0 "Merge remote-tracking branch 'origin/main' into feat/x"
expect_message "commitlint: a local merge into the default branch passes" 0 "Merge branch 'feat/x'"
expect_message "commitlint: a local merge of two branches passes" 0 "Merge branches 'a', 'b' and 'c' into feat/x"
expect_message "commitlint: a bare merged name cannot be a citation" 1 "Merge A""C5 into main"
expect_message "commitlint: a merge-shaped subject with prose cannot carry a citation" 1 "Merge branch 'x' and fix ${STORY} 13.4"

# Dependabot writes a compare URL longer than the body limit into every
# version-update PR. CI lints a PR that GitHub records as Dependabot's with
# commitlint.dependabot.cjs, which lifts that one rule and no other.
DEPENDABOT_BODY="$(printf 'ci(deps): bump example/action from 1.0.0 to 1.1.0\n\nBumps example/action.\n- [Commits](https://github.com/example/action/compare/%s...%s)' \
    "$(printf 'a%.0s' {1..40})" "$(printf 'b%.0s' {1..40})")"
expect_message "commitlint: a Dependabot PR may carry a long body line" 0 "$DEPENDABOT_BODY" commitlint.dependabot.cjs
expect_message "commitlint: anyone else's long body line is refused" 1 "$DEPENDABOT_BODY"
expect_message "commitlint: a Dependabot PR still cannot cite a plan" 1 "$(printf 'ci(deps): bump x for %s 13.1' "$STORY")" commitlint.dependabot.cjs
expect_message "commitlint: a Dependabot PR is still conventional" 1 "Bump example/action from 1.0.0 to 1.1.0" commitlint.dependabot.cjs

echo
if [[ "$failures" -ne 0 ]]; then
    echo "test-gates: $failures case(s) failed"
    exit 1
fi
echo "test-gates: every case behaved"
