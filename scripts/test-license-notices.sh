#!/usr/bin/env bash
# SPDX-License-Identifier: RPL-1.5
# Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
#
# Self-test for scripts/license-notices.sh: every file class is planted in a
# throwaway repository, and `check` and `apply` are judged by exit status and
# by the bytes they leave behind.
#
#   scripts/test-license-notices.sh   exit 0 = every case behaved, 1 = at least one did not
set -euo pipefail

export GIT_CONFIG_GLOBAL=/dev/null GIT_CONFIG_NOSYSTEM=1 LC_ALL=C

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT
REPO="$WORK/repo"
SPDX="SPDX-License-Identifier: RPL-1.5"
COPYRIGHT="Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE."
failures=0

check() {
    if [[ "$2" == ok ]]; then
        echo "ok   $1"
    else
        echo "FAIL $1: $2"
        sed 's/^/     | /' "$WORK/out" 2>/dev/null || true
        failures=$((failures + 1))
    fi
}

# NOTICE names one vendored file, as the real one does.
fresh_repo() {
    rm -rf "$REPO"
    mkdir -p "$REPO/scripts"
    cp "$ROOT/scripts/license-notices.sh" "$REPO/scripts/"
    printf 'vendored thing\n  File:      vendor/lib.min.js\n' > "$REPO/NOTICE"
    git -C "$REPO" init -q -b main
}

plant() { # path content
    mkdir -p "$(dirname "$REPO/$1")"
    printf '%s' "$2" > "$REPO/$1"
    git -C "$REPO" add -A
}

notices() { # mode -> exit status
    local status=0
    (cd "$REPO" && bash scripts/license-notices.sh "$1") > "$WORK/out" 2>&1 || status=$?
    echo "$status"
}

with_notice() { # comment-open comment-close
    printf '%s%s%s\n%s%s%s\n' "$1" "$SPDX" "$2" "$1" "$COPYRIGHT" "$2"
}

# Whether a file holds exactly these bytes; `$(cat)` would drop trailing newlines.
has_bytes() { # path expected
    local b; b="$(cat "$1"; printf x)"
    [[ "${b%x}" == "$2" ]]
}

expect_status() { # name want got
    if [[ "$3" == "$2" ]]; then check "$1" ok; else check "$1" "exit $3, want $2"; fi
}

case_missing_notice_fails() {
    fresh_repo
    plant src/lib.rs $'pub fn f() {}\n'
    expect_status "a source file without the notice fails check" 1 "$(notices check)"
}

case_present_notice_passes() {
    fresh_repo
    plant src/lib.rs "$(with_notice '// ' '')"$'\npub fn f() {}\n'
    plant run.sh $'#!/usr/bin/env bash\n'"$(with_notice '# ' '')"$'\necho hi\n'
    plant style.css "$(with_notice '/* ' ' */')"$'\nbody {}\n'
    expect_status "notices in each comment syntax pass check" 0 "$(notices check)"
}

case_notice_too_far_down_fails() {
    fresh_repo
    plant src/lib.rs $'pub fn a() {}\npub fn b() {}\npub fn c() {}\npub fn d() {}\npub fn e() {}\npub fn f() {}\n'"$(with_notice '// ' '')"$'\n'
    expect_status "a notice below the file's head does not count" 1 "$(notices check)"
}

case_unclassified_file_fails() {
    fresh_repo
    plant data.xyz $'something\n'
    expect_status "an unclassified file fails check" 1 "$(notices check)"
}

case_exempt_files_pass() {
    fresh_repo
    plant vendor/lib.min.js $'!function(){}\n'
    plant backend/migrations/20260715000000_initial_schema.sql $'CREATE TABLE t ();\n'
    plant backend/crates/netroll-app/templates/net_summary.html $'<!doctype html>\n'
    plant package.json $'{}\n'
    plant README.md $'# R\n'
    expect_status "vendored, frozen, generated and comment-less files pass" 0 "$(notices check)"
}

# NOTICE lists an entry's further files on indented continuation lines.
case_continuation_lines_are_vendored_too() {
    fresh_repo
    printf 'theme\n  Files:     theme/a.html\n             theme/b.js\n  License:   MIT\n' > "$REPO/NOTICE"
    plant theme/a.html $'<p></p>\n'
    plant theme/b.js $'void 0;\n'
    expect_status "a file on a NOTICE continuation line is vendored" 0 "$(notices check)"
    plant other.js $'void 0;\n'
    expect_status "a file after the entry is not" 1 "$(notices check)"
}

# Every comment style apply writes, and where: after a shebang or a doctype,
# and as a template comment in a Jinja partial so it is never rendered.
case_apply_writes_each_style() {
    fresh_repo
    plant src/lib.rs $'//! Module docs.\npub fn f() {}\n'
    plant src/view.tsx $'export const A = 1;\n'
    plant run.sh $'#!/usr/bin/env bash\necho hi\n'
    plant style.css $'body {}\n'
    plant index.html $'<!doctype html>\n<html></html>\n'
    plant overrides/partials/x.html $'{% block x %}{% endblock %}\n'
    plant mail/a.mjml $'<mjml></mjml>\n'
    plant ci.yml $'name: ci\n'
    plant Containerfile $'FROM scratch\n'
    local before; before="$(git -C "$REPO" write-tree)"
    expect_status "apply exits 0" 0 "$(notices apply)"
    local -A want=(
        [src/lib.rs]="$(with_notice '// ' '')"$'\n//! Module docs.\npub fn f() {}\n'
        [src/view.tsx]="$(with_notice '// ' '')"$'\nexport const A = 1;\n'
        [run.sh]=$'#!/usr/bin/env bash\n'"$(with_notice '# ' '')"$'\necho hi\n'
        [style.css]="$(with_notice '/* ' ' */')"$'\nbody {}\n'
        [index.html]=$'<!doctype html>\n'"$(with_notice '<!-- ' ' -->')"$'\n<html></html>\n'
        [overrides/partials/x.html]="$(with_notice '{# ' ' #}')"$'\n{% block x %}{% endblock %}\n'
        [mail/a.mjml]="$(with_notice '<!-- ' ' -->')"$'\n<mjml></mjml>\n'
        [ci.yml]="$(with_notice '# ' '')"$'\nname: ci\n'
        [Containerfile]="$(with_notice '# ' '')"$'\nFROM scratch\n'
    )
    local p bad=()
    for p in "${!want[@]}"; do
        has_bytes "$REPO/$p" "${want[$p]}" || bad+=("$p")
    done
    if ((${#bad[@]} == 0)); then check "apply writes each comment style in place" ok; else check "apply writes each comment style in place" "${bad[*]}"; fi
    git -C "$REPO" add -A
    expect_status "check passes after apply" 0 "$(notices check)"
    [[ "$(git -C "$REPO" write-tree)" != "$before" ]] || check "apply changed the tree" "nothing written"
}

case_apply_is_idempotent_and_spares_exempt_files() {
    fresh_repo
    plant src/lib.rs $'pub fn f() {}\n'
    plant vendor/lib.min.js $'!function(){}\n'
    plant backend/migrations/20260715000000_initial_schema.sql $'CREATE TABLE t ();\n'
    expect_status "the first apply exits 0" 0 "$(notices apply)"
    has_bytes "$REPO/src/lib.rs" "$(with_notice '// ' '')"$'\npub fn f() {}\n' \
        && check "the first apply writes the notice" ok || check "the first apply writes the notice" "src/lib.rs unchanged"
    git -C "$REPO" add -A
    local once; once="$(git -C "$REPO" write-tree)"
    expect_status "a second apply exits 0" 0 "$(notices apply)"
    git -C "$REPO" add -A
    if [[ "$(git -C "$REPO" write-tree)" == "$once" ]]; then check "a second apply changes nothing" ok; else check "a second apply changes nothing" "tree moved"; fi
    if has_bytes "$REPO/vendor/lib.min.js" $'!function(){}\n' && has_bytes "$REPO/backend/migrations/20260715000000_initial_schema.sql" $'CREATE TABLE t ();\n'; then
        check "apply leaves vendored and frozen files byte-identical" ok
    else
        check "apply leaves vendored and frozen files byte-identical" "an exempt file was written"
    fi
}

case_apply_refuses_unclassified() {
    fresh_repo
    plant src/lib.rs $'pub fn f() {}\n'
    plant data.xyz $'something\n'
    expect_status "apply refuses while a file is unclassified" 1 "$(notices apply)"
    if has_bytes "$REPO/src/lib.rs" $'pub fn f() {}\n'; then check "a refused apply writes nothing" ok; else check "a refused apply writes nothing" "src/lib.rs was written"; fi
}

# git quotes a non-ASCII path by default; the check must still see the file.
case_non_ascii_path_is_checked() {
    fresh_repo
    plant 'src/caf'$'\xc3\xa9''.rs' $'pub fn f() {}\n'
    expect_status "a non-ASCII path without the notice fails check" 1 "$(notices check)"
}

# Lines that only work as the first line(s) of a file keep their place.
case_apply_keeps_first_line_directives() {
    fresh_repo
    plant spaced.sh $'#! /usr/bin/env bash\necho hi\n'
    plant bare.sh '#!/bin/sh'
    plant page.html $'<?xml version="1.0"?>\n<html></html>\n'
    plant docker/Containerfile $'# syntax=docker/dockerfile:1\n# escape=`\nFROM scratch\n'
    expect_status "apply exits 0 with first-line directives" 0 "$(notices apply)"
    local -A want=(
        [spaced.sh]=$'#! /usr/bin/env bash\n'"$(with_notice '# ' '')"$'\necho hi\n'
        [bare.sh]=$'#!/bin/sh\n'"$(with_notice '# ' '')"$'\n'
        [page.html]=$'<?xml version="1.0"?>\n'"$(with_notice '<!-- ' ' -->')"$'\n<html></html>\n'
        [docker/Containerfile]=$'# syntax=docker/dockerfile:1\n# escape=`\n'"$(with_notice '# ' '')"$'\nFROM scratch\n'
    )
    local p bad=()
    for p in "${!want[@]}"; do has_bytes "$REPO/$p" "${want[$p]}" || bad+=("$p"); done
    if ((${#bad[@]} == 0)); then check "apply writes below first-line directives" ok; else check "apply writes below first-line directives" "${bad[*]}"; fi
}

case_foreign_copyright_fails() {
    fresh_repo
    plant src/lib.rs $'// '"$SPDX"$'\n// Copyright (C) 2019 Someone Else\npub fn f() {}\n'
    expect_status "someone else's copyright line is not the notice" 1 "$(notices check)"
}

case_partial_notice_is_refused_not_doubled() {
    fresh_repo
    plant src/lib.rs $'// '"$SPDX"$'\npub fn f() {}\n'
    expect_status "a partial notice fails check" 1 "$(notices check)"
    expect_status "apply refuses a partial notice" 1 "$(notices apply)"
    has_bytes "$REPO/src/lib.rs" $'// '"$SPDX"$'\npub fn f() {}\n' \
        && check "a refused partial notice is left as it was" ok || check "a refused partial notice is left as it was" "src/lib.rs was written"
}

case_missing_notice_fails
case_present_notice_passes
case_notice_too_far_down_fails
case_unclassified_file_fails
case_exempt_files_pass
case_continuation_lines_are_vendored_too
case_apply_writes_each_style
case_apply_is_idempotent_and_spares_exempt_files
case_apply_refuses_unclassified
case_non_ascii_path_is_checked
case_apply_keeps_first_line_directives
case_foreign_copyright_fails
case_partial_notice_is_refused_not_doubled

if ((failures > 0)); then
    echo "test-license-notices: $failures case(s) misbehaved"
    exit 1
fi
echo "test-license-notices: every case behaved"
