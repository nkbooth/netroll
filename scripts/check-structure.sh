#!/usr/bin/env bash
# SPDX-License-Identifier: RPL-1.5
# Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
# Infrastructure details that have no business in a public repository are
# caught by their SHAPE, not by a list of names.
#
# WHY shapes: a denylist of hosts or people would itself publish what it
# guards. Each class below matches a form, and scripts/check-structure.allow
# names the few legitimate values, one per line with its reason:
#
#   cgnat         an address in 100.64/10 (the tailnet range), anywhere
#   host-suffix   a dotted host ending .ts.net, .internal or .local, in any case,
#                 anywhere, including inside a URL or after `user@`
#   op-vault      a 1Password reference to any vault but the project's own
#   rfc1918       a private IPv4 address in deploy/, .github/ or Markdown; tests
#                 elsewhere use them as fixtures on purpose
#   email         an address outside the reserved example domains and the allowlist
#   ignore-files  .dockerignore and .containerignore are not identical regular files
#
# It does NOT catch a bare hostname, a club's data in a fixture or a person's
# name: those have no shape, and are review's job. A class that misfires twice
# is deleted rather than excepted.
#
#   scripts/check-structure.sh                    the tracked worktree
#   scripts/check-structure.sh --range <revs...>  lines ADDED by each commit in a
#                                                 `git log` range, e.g. a..b, so a
#                                                 value added and removed inside one
#                                                 push is still caught
#
#   exit 0 = clean, 1 = at least one hit (each printed), 2 = usage or tool error
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"
ALLOW="$ROOT/scripts/check-structure.allow"
PROJECT_VAULT='netroll-deploy'

usage() {
    echo "usage: $0 [--range <git log revisions...>]" >&2
    exit 2
}

MODE=worktree
RANGE=()
if [[ $# -gt 0 ]]; then
    [[ "$1" == "--range" && $# -ge 2 ]] || usage
    MODE=range
    RANGE=("${@:2}")
fi
[[ -f "$ALLOW" ]] || { echo "error: allowlist not found: $ALLOW" >&2; exit 2; }

# Every hit is parsed as `<path>:<line>:<content>`, so a path with a colon in it
# would be misread. None exists; refuse rather than misreport if one appears.
# The listing is captured first: piped into `grep -q`, an early match would
# SIGPIPE git and `pipefail` would read the guard as false.
TRACKED="$(git ls-files)" || { echo "error: git ls-files failed" >&2; exit 2; }
if grep -q ':' <<<"$TRACKED"; then
    echo "error: a tracked path contains ':'; this script cannot parse it" >&2
    exit 2
fi

WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT
STREAM="$WORK/stream"

# Worktree lines are `<path>:<line>:<content>`; range lines are
# `<sha>:<path>:<line>:<content>`, with line numbers from each hunk header.
if [[ "$MODE" == worktree ]]; then
    PREFIX='^[^:]*:[0-9]+:'
    PATH_AT='^'
    status=0
    git -c core.quotePath=false grep -nI --no-color -e '^' -- . >"$STREAM" || status=$?
    [[ "$status" -le 1 ]] || { echo "error: git grep failed" >&2; exit 2; }
else
    git rev-list "${RANGE[@]}" >/dev/null 2>&1 || { echo "error: not a revision range: ${RANGE[*]}" >&2; exit 2; }
    PREFIX='^[0-9a-f]+:[^:]*:[0-9]+:'
    PATH_AT='^[0-9a-f]+:'
    # No pathspec: one turns on history simplification, which drops the side of
    # a merge whose tree matches the other parent, with every commit on it.
    # remerge shows only what a merge adds beyond its automatic result. The
    # prefixes are forced because the awk below strips `b/`, whatever the
    # user's diff.noprefix or diff.mnemonicPrefix says.
    git -c core.quotePath=false log -p --reverse --no-color --no-ext-diff --no-textconv -U0 \
        --diff-merges=remerge --src-prefix=a/ --dst-prefix=b/ \
        --format='COMMIT %H' "${RANGE[@]}" \
        | awk '
            (nrem > 0 || orem > 0) {
                c = substr($0, 1, 1)
                if (c == "+") { if (file != "") print sha ":" file ":" nl ":" substr($0, 2); nl++; nrem--; next }
                if (c == "-") { orem--; next }
                if (c == " ") { nl++; nrem--; orem--; next }
                if (c == "\\") next
                nrem = 0; orem = 0
            }
            /^COMMIT / { sha = substr($0, 8); file = ""; next }
            substr($0, 1, 4) == "+++ " {
                # git ends a `+++` path containing a space with a TAB.
                p = substr($0, 5); sub(/\t$/, "", p); file = (p == "/dev/null") ? "" : substr(p, 3); next
            }
            substr($0, 1, 3) == "@@ " {
                split($0, f, " ")
                o = f[2]; sub(/^-/, "", o); n = split(o, op, ","); orem = (n > 1) ? op[2] + 0 : 1
                w = f[3]; sub(/^\+/, "", w); n = split(w, np, ","); nl = np[1] + 0; nrem = (n > 1) ? np[2] + 0 : 1
            }' >"$STREAM"
fi

# Vendored bundles: mermaid's alone is full of address-shaped version strings.
MINIFIED="${PATH_AT}[^:]*\\.min\\.js:"
# Generated, and they carry package authors' addresses nobody here chose.
LOCKFILES="${PATH_AT}([^:]*/)?(package-lock\\.json|[^:/]*\\.lock):"
# Where a private address describes a real network; tests use them as fixtures.
RFC1918_PATHS="${PATH_AT}(deploy/|\\.github/|[^:]*\\.md:)"

EMAIL_RE='[A-Za-z0-9._%+-]+@[A-Za-z0-9-]+(\.[A-Za-z0-9-]+)*\.[A-Za-z]{2,}'
# Split so this line is not itself a hit.
OP_RE='op:''//[^/[:space:]"'"'"'`]+/'

# Loaded once: `<class> <value>` per line, `#` to end of line is the reason.
ALLOWED_VAULTS=()
ALLOWED_ADDRESSES=()
ALLOWED_DOMAINS=()
while read -r class value _; do
    case "$class" in
        op-vault | email) [[ -n "$value" ]] || { echo "error: '$class' with no value in $ALLOW" >&2; exit 2; } ;;&
        op-vault) ALLOWED_VAULTS+=("$value") ;;
        email) if [[ "$value" == *@* ]]; then ALLOWED_ADDRESSES+=("${value,,}"); else ALLOWED_DOMAINS+=("${value,,}"); fi ;;
        "") ;;
        *) echo "error: unknown class '$class' in $ALLOW" >&2; exit 2 ;;
    esac
done < <(sed 's/#.*//' "$ALLOW")

vault_allowed() {
    local vault="$1" entry
    [[ "$vault" == "$PROJECT_VAULT" ]] && return 0
    for entry in "${ALLOWED_VAULTS[@]}"; do [[ "$vault" == "$entry" ]] && return 0; done
    return 1
}

email_allowed() {
    local address="${1,,}" domain entry
    domain="${address#*@}"
    # RFC 2606 and RFC 6761 reserve these for documentation and tests.
    case "$domain" in
        example.com | example.org | example.net | *.example.com | *.example.org | *.example.net) return 0 ;;
        *.example | *.test | *.invalid | *.localhost) return 0 ;;
    esac
    for entry in "${ALLOWED_ADDRESSES[@]}"; do [[ "$address" == "$entry" ]] && return 0; done
    for entry in "${ALLOWED_DOMAINS[@]}"; do
        [[ "$domain" == "$entry" || "$domain" == *".$entry" ]] && return 0
    done
    return 1
}

# Every non-overlapping match of an ERE in a string, into MATCHES. Results are
# returned in globals rather than through `$(…)` because the email class visits
# about fifteen hundred lines, and a subshell per line made the run take seconds.
matches() {
    local rest="$1" re="$2"
    MATCHES=()
    while [[ "$rest" =~ $re ]]; do
        MATCHES+=("${BASH_REMATCH[0]}")
        rest="${rest#*"${BASH_REMATCH[0]}"}"
    done
}

hits=0

report() {
    local line="$1" class="$2" detail="$3" loc
    if [[ "$MODE" == range ]]; then
        loc="${line%%:*}:"; line="${line#*:}"
    else
        loc=""
    fi
    local path="${line%%:*}"; line="${line#*:}"
    echo "$loc$path:${line%%:*}: $class: $detail"
    hits=$((hits + 1))
}

# The text after the location prefix, into CONTENT.
content_of() {
    local line="$1"
    [[ "$MODE" == range ]] && line="${line#*:}"
    line="${line#*:}"
    CONTENT="${line#*:}"
}

# Stream lines whose CONTENT matches. The bare pattern runs first because it is
# cheap and a superset (it can also match inside the location prefix); the
# anchored one, whose leading `.*` makes grep backtrack, then sees only those.
candidates() {
    { grep -E ${3:+"$3"} -- "$1" "$STREAM" || [[ $? -eq 1 ]]; } | { grep -E ${3:+"$3"} -- "$2" || [[ $? -eq 1 ]]; } \
        >"$WORK/candidates" || { echo "error: grep failed" >&2; exit 2; }
}

# `boundary` is the character class that must precede the match (or the start
# of the content); expressing it as `(.*B)?` after the location prefix keeps the
# prefix itself from ever satisfying it. `nocase` as the sixth argument matches
# the body in any case.
shape_class() {
    local class="$1" boundary="$2" body="$3" include="$4" exclude="$5" nocase="${6:-}" line_re line
    if [[ -n "$boundary" ]]; then line_re="${PREFIX}(.*${boundary})?${body}"; else line_re="${PREFIX}.*${body}"; fi
    candidates "$body" "$line_re" ${nocase:+-i}
    [[ -n "$nocase" ]] && shopt -s nocasematch
    while IFS= read -r line; do
        [[ -n "$include" && ! "$line" =~ $include ]] && continue
        [[ -n "$exclude" && "$line" =~ $exclude ]] && continue
        content_of "$line"; matches "$CONTENT" "$body"
        report "$line" "$class" "${MATCHES[0]%[^0-9A-Za-z]}"
    done <"$WORK/candidates"
    shopt -u nocasematch
}

op_vaults() {
    local line token vault
    candidates "$OP_RE" "${PREFIX}.*${OP_RE}"
    while IFS= read -r line; do
        content_of "$line"; matches "$CONTENT" "$OP_RE"
        for token in "${MATCHES[@]}"; do
            vault="${token#op://}"; vault="${vault%/}"
            vault_allowed "$vault" && continue
            report "$line" op-vault "$token"
        done
    done <"$WORK/candidates"
}

# `git@github.com:owner/repo` is an scp-style SSH URL, not an address: a token
# followed by a colon and a non-space is skipped.
emails() {
    local line token
    candidates "$EMAIL_RE" "${PREFIX}.*${EMAIL_RE}"
    while IFS= read -r line; do
        [[ "$line" =~ $LOCKFILES ]] && continue
        content_of "$line"; matches "$CONTENT" "${EMAIL_RE}(:[^[:space:]])?"
        for token in "${MATCHES[@]}"; do
            [[ "$token" =~ :[^[:space:]]$ ]] && continue
            email_allowed "$token" && continue
            report "$line" email "$token"
        done
    done <"$WORK/candidates"
}

# The two must be identical REGULAR files: BuildKit is reported not to load a
# symlinked .dockerignore (moby/buildkit#739), which would silently send the
# whole tree as build context; a regular file removes the question.
ignore_files() {
    local listing
    if [[ "$MODE" == worktree ]]; then
        listing="$(git ls-files -s -- .dockerignore .containerignore)"
    else
        local tip; tip="$(git rev-list -n 1 "${RANGE[@]}")"
        [[ -n "$tip" ]] || return 0
        listing="$(git ls-tree "$tip" -- .dockerignore .containerignore)"
    fi
    local modes blobs
    modes="$(awk '{ print $1 }' <<<"$listing" | sort -u)"
    blobs="$(awk '{ print ($2 ~ /^[0-9a-f]+$/) ? $2 : $3 }' <<<"$listing" | sort -u)"
    if [[ "$(grep -c . <<<"$listing")" -ne 2 || "$modes" != "100644" || "$(grep -c . <<<"$blobs")" -ne 1 ]]; then
        echo ".dockerignore: ignore-files: must be a regular file identical to .containerignore"
        hits=$((hits + 1))
    fi
}

shape_class cgnat '[^0-9.]' \
    '100\.(6[4-9]|[7-9][0-9]|1[01][0-9]|12[0-7])\.[0-9]{1,3}\.[0-9]{1,3}([^0-9]|$)' '' "$MINIFIED"
# `/` and `@` may precede a host, so one inside a URL or after `user@` is
# caught; `*` and `$` may not, so a wildcard or a `${VAR}` template is not.
shape_class host-suffix '[^-A-Za-z0-9_.*$]' \
    '([a-z0-9]([a-z0-9-]*[a-z0-9])?\.)+(ts\.net|internal|local)([^-A-Za-z0-9_]|$)' '' "$MINIFIED" nocase
shape_class rfc1918 '[^0-9.]' \
    '(10\.[0-9]{1,3}|172\.(1[6-9]|2[0-9]|3[01])|192\.168)\.[0-9]{1,3}\.[0-9]{1,3}' "$RFC1918_PATHS" "$MINIFIED"
op_vaults
emails
ignore_files

if [[ "$hits" -ne 0 ]]; then
    echo "check-structure: $hits hit(s)"
    exit 1
fi
echo "check-structure: clean"
