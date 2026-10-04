// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
//! The query-key strictness tripwire.
//!
//! An unrecognised query key is a 400 on a FILTERED read and is ignored on a
//! public/unfiltered one, where "filtered" means a silently-dropped key changes
//! the answer's MEANING. This file notices when a route ships unclassified.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

/// Query-reading handler call sites in the tree — a DATED SNAPSHOT, not an
/// invariant. Failing here is not a defect: it means a query-reading route was
/// added, moved or removed. The classification itself is a compile error, so
/// this is a prompt to look, not the thing standing between the tree and an
/// unclassified route. Re-derive with
/// `grep -rhoE '\): App(Strict)?Query<[A-Za-z]+>' crates/netroll-app/src | wc -l`.
const CALL_SITES_AS_SHIPPED: usize = 15;

/// Of those, the ones reached through `AppStrictQuery`. A read is strict when a
/// silently-dropped key changes the meaning of the answer: a dropped cursor on a
/// paged read, or a dropped `ids` that would answer "none favorited" for every
/// net. A DATED SNAPSHOT, like the count above. Re-derive with
/// `grep -rhoE '\): AppStrictQuery<[A-Za-z]+>' crates/netroll-app/src | wc -l`.
const STRICT_CALL_SITES_AS_SHIPPED: usize = 7;

/// The two extractor identifiers a query-reading handler argument can be
/// ascribed.
const STRICT_EXTRACTOR: &str = "AppStrictQuery";
const LENIENT_EXTRACTOR: &str = "AppQuery";

/// The module that DECLARES both extractors, path relative to `src/`.
///
/// Its mentions of them are declarations, `impl` headers and the
/// `FromRequestParts` trait assertion's turbofish — none of which is a call
/// site, and none of which the accountant below can attribute. So this one file
/// is held to a *count* instead, which is the only place in this file that
/// treats a source specially.
const DECLARING_MODULE: &str = "http/mod.rs";

/// Occurrences of either extractor identifier in [`DECLARING_MODULE`]'s
/// production code.
///
/// Six: `struct AppQuery<T>`, `impl … for AppQuery<T>`,
/// `assert_from_request_parts::<AppQuery<LenientProbeQuery>>()`, and the same
/// three for `AppStrictQuery`.
///
/// **A dated snapshot, like [`CALL_SITES_AS_SHIPPED`], and for the same reason.**
/// Failing here is not a defect: it means `http/mod.rs` grew or lost a mention of
/// a wrapper, and someone has to look at whether it was a route. Comments and
/// string literals are already gone by the time this is counted, so prose about
/// the wrappers does not move it.
const EXTRACTOR_MENTIONS_IN_DECLARING_MODULE: usize = 6;

// ---------------------------------------------------------------------------
// Reading Rust as text — the lexer, and why it is not optional
// ---------------------------------------------------------------------------

/// Whether `chars[i..]` begins with `pat`.
fn starts(chars: &[char], i: usize, pat: &str) -> bool {
    pat.chars()
        .enumerate()
        .all(|(n, c)| chars.get(i + n) == Some(&c))
}

/// The index of the delimiter closing the one at `open`, or `None` when the
/// input is unbalanced from there on.
fn matching_close(chars: &[char], open: usize, opener: char, closer: char) -> Option<usize> {
    let mut depth = 0usize;
    for (i, c) in chars.iter().enumerate().skip(open) {
        if *c == opener {
            depth += 1;
        } else if *c == closer {
            if depth == 0 {
                return None;
            }
            depth -= 1;
            if depth == 0 {
                return Some(i);
            }
        }
    }
    None
}

/// The length in CHARS of the raw or byte-raw string literal starting at `i`
/// (`r"…"`, `r#"…"#`, `br##"…"##`), or `None` when there is none.
///
/// Refuses a trailing `r`/`br` of an identifier (`for`, `iter`, `usize`) being
/// read as a prefix.
///
/// Load-bearing, and the fixture now proves it: a raw string may contain a bare
/// `"`, so a scanner that does not recognise the `r#` prefix resumes reading
/// *code* in the middle of a string literal. The review found the old fixture
/// put its raw-string payload where a naive scanner stripped it anyway, which
/// made all 29 lines of this function a dead guard.
fn raw_string_len(chars: &[char], i: usize) -> Option<usize> {
    if i > 0 && (chars[i - 1].is_alphanumeric() || chars[i - 1] == '_') {
        return None;
    }
    let mut at = i;
    if chars.get(at) == Some(&'b') {
        at += 1;
    }
    if chars.get(at) != Some(&'r') {
        return None;
    }
    at += 1;
    let first_hash = at;
    while chars.get(at) == Some(&'#') {
        at += 1;
    }
    let terminator = format!("\"{}", "#".repeat(at - first_hash));
    if chars.get(at) != Some(&'"') {
        return None;
    }
    at += 1;
    while at < chars.len() {
        if starts(chars, at, &terminator) {
            return Some(at + terminator.chars().count() - i);
        }
        at += 1;
    }
    None
}

/// The length in CHARS of the char literal starting at `i` (`'x'`, `'\n'`,
/// `'\''`, `'\u{2026}'`), or `None` when that `'` opens a LIFETIME (`'a`,
/// `'static`).
///
/// Skipped rather than ignored for one reason: an unskipped `'"'` would open a
/// phantom string literal and swallow the rest of the file, which would make
/// every scan below vacuous. `'\''` is the case the review found returning 3 for
/// a four-char literal, leaving a stray `'` behind — pinned directly by
/// [`the_char_literal_scan_measures_every_escape`].
fn char_literal_len(chars: &[char], i: usize) -> Option<usize> {
    if chars.get(i + 1) == Some(&'\\') {
        // The escaped char comes first, so an escaped quote is not the closer.
        let mut at = i + 3;
        while at < chars.len() && chars[at] != '\'' {
            at += 1;
        }
        return (at < chars.len()).then_some(at + 1 - i);
    }
    (chars.get(i + 2) == Some(&'\'')).then_some(3)
}

/// The source with every COMMENT and the BODY of every string literal removed —
/// Rust code, and nothing that merely looks like it.
///
/// Not tidiness. On this project a register's guard has been **inverted** by
/// prose twice: once by a `///` doc comment quoting the shape the guard forbids,
/// and once by a `/* … */` block comment quoting the correct shape above a call
/// site that did the wrong thing. This file's own module doc contains the literal
/// text `AppQuery<` and `deny_unknown_fields`, so without this the scans below
/// would be reading their own documentation.
///
/// A small lexer rather than a list of rejected shapes, because the shape that
/// breaks a list is always the one not on it. It drops line comments (`//`,
/// `///`, `//!`), NESTED block comments (Rust permits `/* /* */ */`, so a
/// non-nesting scanner stops at the wrong `*/`), and the CONTENTS of ordinary,
/// raw and byte string literals.
///
/// **Every newline of the input survives, including newlines inside a multi-line
/// string literal**, and [`the_lexer_never_loses_a_line`] asserts that per source
/// file. That is the general tripwire for the failure this file shipped with:
/// a parse that silently swallows part of a file reads as a pass.
fn code_only(source: &str) -> String {
    let chars: Vec<char> = source.chars().collect();
    let mut out = String::with_capacity(chars.len());
    let mut i = 0usize;
    while i < chars.len() {
        if starts(&chars, i, "//") {
            while i < chars.len() && chars[i] != '\n' {
                i += 1;
            }
            continue;
        }
        if starts(&chars, i, "/*") {
            let mut depth = 0usize;
            while i < chars.len() {
                if starts(&chars, i, "/*") {
                    depth += 1;
                    i += 2;
                } else if starts(&chars, i, "*/") {
                    depth -= 1;
                    i += 2;
                    if depth == 0 {
                        break;
                    }
                } else {
                    if chars[i] == '\n' {
                        out.push('\n');
                    }
                    i += 1;
                }
            }
            continue;
        }
        if let Some(consumed) = raw_string_len(&chars, i) {
            out.push_str("\"\"");
            for c in &chars[i..i + consumed] {
                if *c == '\n' {
                    out.push('\n');
                }
            }
            i += consumed;
            continue;
        }
        if chars[i] == '"' {
            let mut newlines = 0usize;
            i += 1;
            while i < chars.len() && chars[i] != '"' {
                if chars[i] == '\\' {
                    i += 1;
                }
                if chars.get(i) == Some(&'\n') {
                    newlines += 1;
                }
                i += 1;
            }
            i += 1;
            out.push_str("\"\"");
            for _ in 0..newlines {
                out.push('\n');
            }
            continue;
        }
        // A lifetime falls through to the default arm — see `char_literal_len`.
        if chars[i] == '\''
            && let Some(consumed) = char_literal_len(&chars, i)
        {
            out.push_str("' '");
            i += consumed;
            continue;
        }
        out.push(chars[i]);
        i += 1;
    }
    out
}

/// The char index just past the item a `#[cfg(test)]` at `from` gates.
///
/// Attributes first (there may be several on the item), then the item: a body in
/// braces, or a `;`-terminated declaration. `(`/`[` nesting is tracked so a `;`
/// inside an array type (`fn f(x: [u8; 4])`) is not mistaken for the end.
fn cfg_test_item_end(chars: &[char], from: usize) -> usize {
    let mut at = from;
    loop {
        while at < chars.len() && chars[at].is_whitespace() {
            at += 1;
        }
        if chars.get(at) != Some(&'#') {
            break;
        }
        let open = at
            + if chars.get(at + 1) == Some(&'!') {
                2
            } else {
                1
            };
        match matching_close(chars, open, '[', ']') {
            Some(close) => at = close + 1,
            None => return chars.len(),
        }
    }
    let mut depth = 0usize;
    while at < chars.len() {
        match chars[at] {
            '(' | '[' => depth += 1,
            ')' | ']' => depth = depth.saturating_sub(1),
            '{' if depth == 0 => {
                return matching_close(chars, at, '{', '}').map_or(chars.len(), |close| close + 1);
            }
            ';' if depth == 0 => return at + 1,
            _ => {}
        }
        at += 1;
    }
    chars.len()
}

/// The lexed source with every `#[cfg(test)]`-gated ITEM blanked out, and
/// nothing else.
///
/// A test module names extractors and query types freely and that is not a route
/// declaration: scanning one would make a fixture look like a production guard.
///
/// **This used to truncate the file at the first `#[cfg(test)]`, and that was
/// H2.** `#[cfg(test)]` is legal on a single item mid-file:
/// `src/http/rate_limit.rs:73` is `#[cfg(test)] fn tracked_keys(&self)` — a
/// test-only accessor inside a **production** `impl` — at line 73 of 1241, so the
/// register read 72 lines of that file and discarded ~544 lines of production
/// code. Same shape at `known_addresses.rs:133`, `http/undefinable.rs:84` and
/// `http/tokens.rs:82`. A review mutation put a correctly-shaped, unclassified
/// thirteenth call site below that line and every test here passed.
///
/// Blanked rather than deleted so the line count survives, and the brace balance
/// of what remains is asserted per file: an over-eager cut leaves an unclosed
/// `impl` behind, which is the failure above in its general form.
fn strip_cfg_test_items(lexed: &str) -> String {
    let chars: Vec<char> = lexed.chars().collect();
    let mut out = String::with_capacity(chars.len());
    let mut i = 0usize;
    while i < chars.len() {
        if starts(&chars, i, "#[cfg(test)]") {
            let end = cfg_test_item_end(&chars, i);
            for c in &chars[i..end] {
                if *c == '\n' {
                    out.push('\n');
                }
            }
            i = end;
            continue;
        }
        out.push(chars[i]);
        i += 1;
    }
    out
}

/// The production Rust of one source: lexed, then with `#[cfg(test)]` items
/// blanked. **Lex first** — the raw source of `src/http/mod.rs` has two doc
/// comments that *quote* `#[cfg(test)]` (`:539`, `:550`, both explaining why
/// something is NOT gated), and cutting before lexing landed on the first of
/// them and discarded the following ~1200 lines. Found by mutation M7a on
/// 2026-08-27, when a thirteenth call site added to that file came back GREEN.
fn production_code(source: &str) -> String {
    strip_cfg_test_items(&code_only(source))
}

/// Net brace depth over `code`, and whether it ever went negative.
fn brace_balance(code: &str) -> (i64, bool) {
    let mut depth = 0i64;
    let mut negative = false;
    for c in code.chars() {
        match c {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                negative |= depth < 0;
            }
            _ => {}
        }
    }
    (depth, negative)
}

// ---------------------------------------------------------------------------
// The derivation
// ---------------------------------------------------------------------------

/// Every `.rs` file under the crate's `src/`, as (path relative to `src/`,
/// contents).
///
/// Walked at run time rather than a hand-listed set of `include_str!`s: a new
/// handler module under `src/http/` would be invisible to a fixed list, which is
/// the same "derived coverage that is actually restated" failure this file exists
/// to avoid. `CARGO_MANIFEST_DIR` is compile-time and does not depend on the
/// runner's working directory.
fn source_files() -> Vec<(String, String)> {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut out = Vec::new();
    let mut stack = vec![root.clone()];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).expect("read src/") {
            let path = entry.expect("dir entry").path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().is_some_and(|e| e == "rs") {
                let rel = path
                    .strip_prefix(&root)
                    .expect("under src/")
                    .to_string_lossy()
                    .into_owned();
                out.push((rel, std::fs::read_to_string(&path).expect("read source")));
            }
        }
    }
    assert!(
        out.len() > 5,
        "the src/ walk found only {} files — the walk itself is broken, and every \
         assertion below would be vacuous",
        out.len()
    );
    out.sort();
    out
}

/// One derived handler call site.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct CallSite {
    /// Path relative to `src/`, e.g. `http/admin.rs`.
    file: String,
    /// `AppQuery` or `AppStrictQuery`.
    extractor: String,
    /// The outermost query type name, e.g. `PageQuery`.
    query_type: String,
}

/// An identifier read forward from `i`, and how many chars it spans.
fn ident_at(chars: &[char], i: usize) -> (String, usize) {
    let name: String = chars[i..]
        .iter()
        .take_while(|c| c.is_ascii_alphanumeric() || **c == '_')
        .collect();
    let len = name.chars().count();
    (name, len)
}

/// Whether `i` starts an identifier rather than continuing one.
fn is_ident_start(chars: &[char], i: usize) -> bool {
    i == 0 || !(chars[i - 1].is_ascii_alphanumeric() || chars[i - 1] == '_')
}

/// The index of the first char of the `::`-joined path whose last segment starts
/// at `i`, skipping backwards over `a::b::` prefixes.
fn path_start(chars: &[char], i: usize) -> usize {
    let mut at = i;
    while at >= 2 && starts(chars, at - 2, "::") {
        let mut seg_end = at - 2;
        while seg_end > 0 && chars[seg_end - 1].is_whitespace() {
            seg_end -= 1;
        }
        let mut seg_start = seg_end;
        while seg_start > 0
            && (chars[seg_start - 1].is_ascii_alphanumeric() || chars[seg_start - 1] == '_')
        {
            seg_start -= 1;
        }
        if seg_start == seg_end {
            break;
        }
        at = seg_start;
    }
    at
}

/// Whether the extractor name whose path starts at `at` is a handler ARGUMENT's
/// ascribed type.
///
/// Anchored on the type ascription `:` rather than on the destructuring pattern
/// `):` the first version of this file used. `AppQuery<T>(pub(crate) T)` has a
/// `pub(crate)` field, so `async fn h(params: AppStrictQuery<T>)` is a legal,
/// idiomatic axum handler containing no `):` at all — proven GREEN by review
/// mutation MUT-2, which shipped a thirteenth unclassified call site in exactly
/// that shape. The ascription colon is present in both shapes and in
/// `mut q: AppQuery<T>` besides.
///
/// A `::`-prefixed occurrence (`assert_from_request_parts::<AppQuery<…>>()`) is
/// rejected by looking at the char BEFORE the colon, which is what keeps the
/// trait assertion in `http/mod.rs` out of the derived set.
fn is_argument_ascription(chars: &[char], at: usize) -> bool {
    let mut back = path_start(chars, at);
    while back > 0 && chars[back - 1].is_whitespace() {
        back -= 1;
    }
    back > 0 && chars[back - 1] == ':' && (back < 2 || chars[back - 2] != ':')
}

/// The handler call sites in one source.
fn call_sites_in(file: &str, source: &str) -> Vec<CallSite> {
    let code = production_code(source);
    let chars: Vec<char> = code.chars().collect();
    let mut out = Vec::new();
    let mut i = 0usize;
    while i < chars.len() {
        if !is_ident_start(&chars, i) {
            i += 1;
            continue;
        }
        let (name, len) = ident_at(&chars, i);
        if len == 0 {
            i += 1;
            continue;
        }
        if name != LENIENT_EXTRACTOR && name != STRICT_EXTRACTOR {
            i += len;
            continue;
        }
        let mut at = i + len;
        while at < chars.len() && chars[at].is_whitespace() {
            at += 1;
        }
        if chars.get(at) != Some(&'<') || !is_argument_ascription(&chars, i) {
            i += len;
            continue;
        }
        at += 1;
        // The type argument: skip its own path prefix, then take the name. A
        // generic argument (`Wrapper<T>`) yields the OUTERMOST name, which is the
        // type whose declaration carries (or does not carry) the attribute.
        let mut query_type = String::new();
        loop {
            while at < chars.len() && chars[at].is_whitespace() {
                at += 1;
            }
            let (segment, seg_len) = ident_at(&chars, at);
            if segment.is_empty() {
                break;
            }
            at += seg_len;
            if starts(&chars, at, "::") {
                at += 2;
                continue;
            }
            query_type = segment;
            break;
        }
        assert!(
            !query_type.is_empty(),
            "{file}: an `{name}<` argument with no readable type name"
        );
        out.push(CallSite {
            file: file.to_owned(),
            extractor: name,
            query_type,
        });
        i = at;
    }
    out
}

// ---------------------------------------------------------------------------
// Accounting for every mention — failing on the CLASS, not on the shape
// ---------------------------------------------------------------------------

/// What one occurrence of an extractor identifier in production code is doing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MentionKind {
    /// The ascribed type of a handler argument: a call site, and therefore
    /// something [`call_sites_in`] derives and the two duties below cover.
    CallSite,
    /// The destructuring-pattern half of `AppQuery(params): AppQuery<T>`, or any
    /// other pattern/constructor use. Benign: the value it binds came from an
    /// ascription that is itself accounted for.
    Pattern,
    /// A `use` item bringing the wrapper into scope under its own name.
    Import,
    /// Anything else — a `type` alias, a renaming import, a `let` binding, a
    /// struct field. The parse cannot say what it does, so it is a failure.
    Unattributed,
}

/// One occurrence of an extractor identifier, and what it was taken to be.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Mention {
    /// Path relative to `src/`, e.g. `http/admin.rs`.
    file: String,
    /// `AppQuery` or `AppStrictQuery`.
    extractor: String,
    kind: MentionKind,
    /// 1-based line, which numbers the same as the raw source because
    /// [`the_lexer_never_loses_a_line`] asserts the pipeline preserves lines.
    line: usize,
}

/// The `[start, end)` char spans of every `use` item in `code`.
///
/// From the `use` keyword to its `;`. A brace list (`use a::{B, C};`) contains no
/// `;`, so scanning to the first one is sufficient and `pub use` is covered
/// because the keyword itself is what anchors the scan.
fn use_item_spans(chars: &[char]) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    let mut i = 0usize;
    while i < chars.len() {
        if is_ident_start(chars, i) && ident_at(chars, i).0 == "use" {
            let mut at = i + 3;
            while at < chars.len() && chars[at] != ';' {
                at += 1;
            }
            out.push((i, at));
            i = at.max(i + 3);
            continue;
        }
        i += 1;
    }
    out
}

/// What the extractor mention whose identifier starts at `at` is doing.
fn classify_mention(chars: &[char], at: usize, len: usize, uses: &[(usize, usize)]) -> MentionKind {
    let mut after = at + len;
    while after < chars.len() && chars[after].is_whitespace() {
        after += 1;
    }
    if uses.iter().any(|(start, end)| at >= *start && at < *end) {
        // `use … as Aliased` hides the wrapper behind a name no scan in this
        // file knows, which is the import-shaped half of the alias escape.
        return if ident_at(chars, after).0 == "as" {
            MentionKind::Unattributed
        } else {
            MentionKind::Import
        };
    }
    if chars.get(after) == Some(&'(') {
        return MentionKind::Pattern;
    }
    if chars.get(after) == Some(&'<') && is_argument_ascription(chars, at) {
        return MentionKind::CallSite;
    }
    MentionKind::Unattributed
}

/// Every occurrence of either extractor identifier in one source's production
/// code, classified.
///
/// The point is the [`MentionKind::Unattributed`] arm. Every previous version of
/// this register answered "how many call sites can I see?", and a shape it could
/// not see answered **zero** — silently, with the cardinality snapshot still
/// matching and the two-way binding never reaching the type. That is how the
/// `type` alias escape worked and how the two shapes before it worked. This
/// function asks the complementary question — "is there anything here I cannot
/// account for?" — so an unseen shape is a red rather than a zero.
fn extractor_mentions_in(file: &str, source: &str) -> Vec<Mention> {
    let code = production_code(source);
    let chars: Vec<char> = code.chars().collect();
    let uses = use_item_spans(&chars);
    let mut out = Vec::new();
    let mut line = 1usize;
    let mut i = 0usize;
    while i < chars.len() {
        if chars[i] == '\n' {
            line += 1;
            i += 1;
            continue;
        }
        if !is_ident_start(&chars, i) {
            i += 1;
            continue;
        }
        let (name, len) = ident_at(&chars, i);
        if len == 0 {
            i += 1;
            continue;
        }
        if name != LENIENT_EXTRACTOR && name != STRICT_EXTRACTOR {
            i += len;
            continue;
        }
        out.push(Mention {
            file: file.to_owned(),
            extractor: name,
            kind: classify_mention(&chars, i, len, &uses),
            line,
        });
        i += len;
    }
    out
}

/// Every derived handler call site across `src/`.
fn call_sites() -> Vec<CallSite> {
    let mut out: Vec<CallSite> = source_files()
        .iter()
        .flat_map(|(file, source)| call_sites_in(file, source))
        .collect();
    out.sort();
    out
}

/// Whether a struct declaration refuses unknown fields.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Denial {
    /// No `deny_unknown_fields` on the declaration.
    No,
    /// `#[serde(deny_unknown_fields)]`, unconditionally.
    Yes,
    /// `deny_unknown_fields` behind a `cfg_attr` — strict in some builds and not
    /// in others, which this scan refuses to collapse into a yes or a no.
    Conditional,
}

/// Whether `i` starts a declaration of `struct ty` (any body shape, generics and
/// `where` clauses included).
fn is_struct_decl_at(chars: &[char], i: usize, ty: &str) -> bool {
    if !is_ident_start(chars, i) || !starts(chars, i, "struct") {
        return false;
    }
    let mut at = i + "struct".len();
    if !chars.get(at).is_some_and(|c| c.is_whitespace()) {
        return false;
    }
    while at < chars.len() && chars[at].is_whitespace() {
        at += 1;
    }
    if !starts(chars, at, ty) {
        return false;
    }
    at += ty.chars().count();
    // `struct PageQueryExtra` must not match `PageQuery`.
    !chars
        .get(at)
        .is_some_and(|c| c.is_ascii_alphanumeric() || *c == '_')
}

/// The attribute texts attached to the item whose `struct` keyword is at
/// `decl_at`, read backwards through modifiers.
///
/// Bracket-matched rather than walked line by line, which is the fix for the
/// review's M1: the old scan broke on any line not starting with `#[`, including
/// a wrapped attribute's own closing `)]`, so
/// `#[serde(\n    deny_unknown_fields,\n)]` read as **absent** — silently
/// disabling the backward half of the binding, the half that catches a lenient
/// read inheriting a strictness nobody chose.
fn attributes_above(chars: &[char], decl_at: usize) -> Vec<String> {
    let mut out = Vec::new();
    let mut back = decl_at;
    loop {
        while back > 0 && chars[back - 1].is_whitespace() {
            back -= 1;
        }
        if back == 0 {
            return out;
        }
        match chars[back - 1] {
            // `pub`, `pub(super)`, `pub(crate)` … before the `struct` keyword.
            ')' => {
                let mut depth = 0usize;
                let mut at = back - 1;
                loop {
                    match chars[at] {
                        ')' => depth += 1,
                        '(' => {
                            depth -= 1;
                            if depth == 0 {
                                break;
                            }
                        }
                        _ => {}
                    }
                    if at == 0 {
                        return out;
                    }
                    at -= 1;
                }
                back = at;
            }
            ']' => {
                let mut depth = 0usize;
                let mut at = back - 1;
                loop {
                    match chars[at] {
                        ']' => depth += 1,
                        '[' => {
                            depth -= 1;
                            if depth == 0 {
                                break;
                            }
                        }
                        _ => {}
                    }
                    if at == 0 {
                        return out;
                    }
                    at -= 1;
                }
                if at == 0 || chars[at - 1] != '#' {
                    return out;
                }
                out.push(chars[at + 1..back - 1].iter().collect());
                back = at - 1;
            }
            c if c.is_ascii_alphanumeric() || c == '_' => {
                let mut at = back;
                while at > 0 && (chars[at - 1].is_ascii_alphanumeric() || chars[at - 1] == '_') {
                    at -= 1;
                }
                if chars[at..back].iter().collect::<String>() != "pub" {
                    return out;
                }
                back = at;
            }
            _ => return out,
        }
    }
}

/// Whether `ty`'s declaration in `source` refuses unknown fields, or `None` when
/// `source` does not declare it.
///
/// Panics when the same file declares `ty` more than once — the scan genuinely
/// cannot tell which one a call site means, and the review found the old rail
/// counting over FILES (so two declarations in one file slipped through) and
/// reporting "has 0 declarations" for a shape it simply could not read.
fn denies_unknown_fields_in(source: &str, ty: &str) -> Option<Denial> {
    let code = production_code(source);
    let chars: Vec<char> = code.chars().collect();
    let declarations: Vec<usize> = (0..chars.len())
        .filter(|&i| is_struct_decl_at(&chars, i, ty))
        .collect();
    match declarations.len() {
        0 => return None,
        1 => {}
        n => panic!(
            "`{ty}` is declared {n} times in one file under src/; the scan cannot tell which \
             one the call site means"
        ),
    }
    let attributes = attributes_above(&chars, declarations[0]);
    let mut denial = Denial::No;
    for attribute in &attributes {
        if !attribute.contains("deny_unknown_fields") {
            continue;
        }
        if attribute.trim_start().starts_with("cfg_attr") {
            return Some(Denial::Conditional);
        }
        denial = Denial::Yes;
    }
    Some(denial)
}

/// Whether each named query type refuses unknown fields, resolved across `src/`.
fn denies_unknown_fields(types: &BTreeSet<String>) -> BTreeMap<String, Denial> {
    let sources = source_files();
    let mut out = BTreeMap::new();
    for ty in types {
        let found: Vec<Denial> = sources
            .iter()
            .filter_map(|(_, source)| denies_unknown_fields_in(source, ty))
            .collect();
        assert_eq!(
            found.len(),
            1,
            "`{ty}` is declared in {} files under src/, not 1; the scan cannot tell which \
             declaration the call site means",
            found.len()
        );
        out.insert(ty.clone(), found[0]);
    }
    out
}

// ---------------------------------------------------------------------------
// The integrity tripwires — a parse failure must be a red, not a narrowing
// ---------------------------------------------------------------------------

#[test]
fn the_lexer_never_loses_a_line() {
    // The cheapest guard in this file, and NOT the general form of H2 — the
    // module doc used to say it was, and mechanism verification
    // measured otherwise (Low 3).
    //
    // What it detects: a DELETION-shaped narrowing, where the pipeline returns a
    // shorter string than it was given. That is what the original
    // `drop_test_modules` did.
    //
    // What it is blind to: a BLANKING-shaped one — which is what this pipeline
    // does by design, since `strip_cfg_test_items` blanks precisely so the count
    // survives. Under a restored truncating strip this assertion PASSED while
    // ~1168 of `rate_limit.rs`'s 1241 lines were blank;
    // `the_production_code_of_every_source_still_balances` was the sole detector.
    // The guard against silent truncation is brace balance, and duty 4's mention
    // accountant is what catches a mention that stopped being readable.
    for (file, source) in source_files() {
        let lexed = code_only(&source);
        assert_eq!(
            lexed.lines().count(),
            source.lines().count(),
            "the lexer lost lines of {file}: every scan over it would be reading a truncated \
             file and passing"
        );
        let stripped = strip_cfg_test_items(&lexed);
        assert_eq!(
            stripped.lines().count(),
            lexed.lines().count(),
            "blanking the #[cfg(test)] items of {file} lost lines"
        );
    }
}

#[test]
fn the_lexer_keeps_the_newlines_inside_a_multi_line_literal() {
    // The per-file tripwire above proves the ORDINARY multi-line string case on
    // the real tree. It does NOT prove the RAW one: `src/` contains no multi-line
    // raw string today, and blinding that branch alone left every assertion in
    // this file passing (measured 2026-08-27 — a dead guard of exactly the kind
    // the review found two of). Asserted directly, so both branches are live.
    const MULTI: &str = "let a = \"one\ntwo\";\nlet b = r#\"three\nfour\"#;\nlet c = 1;\n";
    assert_eq!(
        code_only(MULTI).lines().count(),
        MULTI.lines().count(),
        "the lexer dropped a newline from inside a multi-line literal, which is how a \
         line-count tripwire becomes a false alarm and then gets deleted"
    );
}

#[test]
fn the_production_code_of_every_source_still_balances() {
    // The general form of H2 stated structurally. The truncating
    // `drop_test_modules` this file shipped with cut
    // `src/http/rate_limit.rs` at line 73 of 1241, mid-`impl`, and left the
    // braces open — so this assertion would have caught that defect without
    // anyone having to know the shape in advance. It is the guard against the
    // FIFTH occurrence of the class, not the fourth.
    for (file, source) in source_files() {
        let (depth, negative) = brace_balance(&production_code(&source));
        assert_eq!(
            depth, 0,
            "the production code derived from {file} has {depth} unclosed braces — the lexer or \
             the #[cfg(test)] strip cut an item in half, and every scan below is reading a \
             fragment"
        );
        assert!(!negative, "{file}: brace depth went negative");
    }
}

// ---------------------------------------------------------------------------
// The register
// ---------------------------------------------------------------------------

#[test]
fn no_query_type_serves_both_a_strict_and_a_lenient_call_site() {
    // `#[serde(deny_unknown_fields)]` is all-or-nothing PER STRUCT, so a type
    // used on both sides cannot be satisfied at all — the design would have to
    // change, not the classification. The compiler does not catch this one: a
    // type may implement both marker traits. No type does; four types serve two
    // call sites each and every pair lands on the same side.
    let mut by_type: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for site in call_sites() {
        by_type
            .entry(site.query_type)
            .or_default()
            .insert(site.extractor);
    }
    let straddling: Vec<(&String, &BTreeSet<String>)> = by_type
        .iter()
        .filter(|(_, extractors)| extractors.len() > 1)
        .collect();
    assert!(
        straddling.is_empty(),
        "these query types are reached through more than one extractor, which \
         `deny_unknown_fields` cannot express: {straddling:?}. Split the type, or reclassify \
         the route"
    );
}

#[test]
fn the_call_site_count_is_the_tripwire_on_a_dated_snapshot() {
    // The cardinality half. A count is a snapshot of that day's signatures,
    // never an invariant — trait-mutation count went stale twice for
    // exactly that reason, so this one is labelled and lives in ONE place.
    // Failing here is not a defect: it means a query-reading route was added,
    // moved or removed, and someone should look at its classification. What it is
    // NOT is the thing standing between the tree and an unclassified route —
    // `http/mod.rs`'s `QueryKeyPolicy` bound is, and that is a compile error.
    let sites = call_sites();
    assert_eq!(
        sites.len(),
        CALL_SITES_AS_SHIPPED,
        "the query-reading call-site count changed. Sites derived now: {sites:#?}"
    );
    let strict = sites
        .iter()
        .filter(|s| s.extractor == STRICT_EXTRACTOR)
        .count();
    assert_eq!(
        strict, STRICT_CALL_SITES_AS_SHIPPED,
        "the STRICT call-site count changed. Sites derived now: {sites:#?}"
    );
}

#[test]
fn the_extractor_at_each_call_site_agrees_with_the_attribute_on_its_struct() {
    // The two-way binding, and the half the type system cannot state.
    //
    // Forward: a route declared `AppStrictQuery` whose struct forgot the
    // attribute would answer 200-with-the-unfiltered-answer while its signature,
    // its `StrictQuery` impl and the compiler all say otherwise.
    //
    // Backward: a route declared `AppQuery` whose struct DOES carry the attribute
    // has become strict without anyone choosing it.
    //
    // Both directions are derived from the shipping source on both sides — the
    // extractor from the handler argument, the attribute from the declaration.
    // There is no classification table here to fall behind the tree.
    let sites = call_sites();
    let types: BTreeSet<String> = sites.iter().map(|s| s.query_type.clone()).collect();
    let denies = denies_unknown_fields(&types);

    for site in &sites {
        // An index, not an assertion: `denies` is built from these same types.
        let attribute = denies[&site.query_type];
        let strict = site.extractor == STRICT_EXTRACTOR;
        match (strict, attribute) {
            (_, Denial::Conditional) => panic!(
                "{}: `{}` carries `deny_unknown_fields` behind a `cfg_attr`, so whether this \
                 route refuses an unrecognised key depends on the build. Rule it strict or \
                 lenient and say so unconditionally",
                site.file, site.query_type
            ),
            (true, Denial::No) => panic!(
                "{}: `{}` is reached through `AppStrictQuery` but its declaration does not carry \
                 `#[serde(deny_unknown_fields)]`, so the strictness the signature promises does \
                 not exist",
                site.file, site.query_type
            ),
            (false, Denial::Yes) => panic!(
                "{}: `{}` is reached through `AppQuery` — a lenient read — but its declaration \
                 carries `#[serde(deny_unknown_fields)]`. That route now refuses an unrecognised \
                 key without anyone having ruled it filtered",
                site.file, site.query_type
            ),
            _ => {}
        }
    }
}

#[test]
fn every_extractor_mention_under_src_is_accounted_for() {
    // The FIFTH shape's answer, and it is deliberately not a sixth pattern.
    //
    // mechanism verification shipped a thirteenth route as
    //
    // type AliasedStrict = crate::http::AppStrictQuery<ProbeAliasTyQuery>;
    // async fn probe_alias_ty(params: AliasedStrict) -> String { … }
    //
    // with `ProbeAliasTyQuery` deliberately lacking
    // `#[serde(deny_unknown_fields)]` — a route whose signature, whose
    // `StrictQuery` impl and whose compiler all promise refusal while it returns
    // the unfiltered answer. Result: 12 passed / 0 failed, `EXIT=0`. The alias's
    // `=` is not an ascription colon and the handler argument never names the
    // extractor, so the call-site parse derived ZERO from the file: the
    // cardinality snapshot still read 12 and the two-way binding never saw the
    // type.
    //
    // Four shapes had already beaten this parse before that one. Adding a fifth
    // pattern — resolve single-segment `type` aliases — would close the shape and
    // leave the class open, which is the move this project has now made four
    // times. So the duty is inverted instead: every mention of either wrapper in
    // production code must be attributable to a call site, a destructuring
    // pattern, or a plain import. Anything else reds HERE, naming the file and
    // the line, whether or not anyone thought of the shape.
    //
    // What this does NOT catch: a type alias to either wrapper used as the
    // handler argument's own type (resolved above), a hand-rolled
    // `FromRequestParts` impl that never mentions `AppQuery`/`AppStrictQuery`
    // by name, and a macro-synthesized wrapper whose expansion this text scan
    // never sees.
    let mut unattributed: Vec<Mention> = Vec::new();
    let mut in_declaring_module = 0usize;
    for (file, source) in source_files() {
        let mentions = extractor_mentions_in(&file, &source);
        if file == DECLARING_MODULE {
            in_declaring_module = mentions.len();
            continue;
        }
        unattributed.extend(
            mentions
                .into_iter()
                .filter(|m| m.kind == MentionKind::Unattributed),
        );
    }
    assert!(
        unattributed.is_empty(),
        "these mentions of a query extractor under src/ are not a call site, a destructuring \
         pattern or a plain import, so this register cannot say whether they read a query \
         string: {unattributed:#?}. A `type` alias or a renaming `use` puts a route behind a \
         name the call-site parse cannot see — give the handler argument the wrapper's own \
         name, or add the shape to the parse AND to the module doc"
    );
    assert_eq!(
        in_declaring_module, EXTRACTOR_MENTIONS_IN_DECLARING_MODULE,
        "the number of times {DECLARING_MODULE} names a query extractor in production code \
         changed. It declares them, so its mentions cannot be attributed one by one and this \
         count is what stands in for that — including for a route added to that file"
    );
}

/// A synthetic source carrying every way an extractor identifier can reach
/// production code, including the two that put a route behind a name this file
/// cannot see.
///
/// Separate from [`CALL_SITE_SHAPES`] because that fixture deliberately contains
/// the extractors' own declarations and trait assertion, which are exactly the
/// unattributable mentions the declaring module is exempted for.
const MENTION_SHAPES: &str = r##"
//! AppQuery<NeverSeenInProse> and AppStrictQuery<NeverSeenInProseEither>.
/* AppQuery<NeverSeenInABlockComment> */
const PROSE: &str = "AppStrictQuery<NeverSeenInAStringLiteral>";

use super::{AppState, AppStrictQuery, rfc3339};
use crate::http::AppQuery;
pub use crate::http::AppStrictQuery;

async fn destructured(AppQuery(params): AppQuery<Lenient>) {}

async fn not_destructured(params: AppStrictQuery<Strict>) {}

async fn qualified(p: crate::http::AppQuery<Qualified>) {}

/* REVIEW (mechanism verification, Med 1): the fifth shape. Neither of these two
   lines carries a type ascription of the extractor, so the call-site parse
   derives nothing from either and the cardinality snapshot does not move. The
   `type` item is the one the verifier shipped a live defect through. */
type AliasedStrict = crate::http::AppStrictQuery<AliasedAway>;

async fn through_a_type_alias(params: AliasedStrict) {}

/* The same trick played through the import instead of a `type` item. */
use crate::http::AppStrictQuery as RenamedStrict;

async fn through_a_renamed_import(params: RenamedStrict<RenamedAway>) {}

#[cfg(test)]
mod tests {
    type InATestModule = AppQuery<NeverSeenInATestModule>;
}
"##;

#[test]
fn the_mention_accountant_classifies_every_shape() {
    let kinds: Vec<(String, MentionKind)> = extractor_mentions_in("fixture.rs", MENTION_SHAPES)
        .into_iter()
        .map(|m| (m.extractor, m.kind))
        .collect();
    assert_eq!(
        kinds,
        vec![
            ("AppStrictQuery".to_owned(), MentionKind::Import),
            ("AppQuery".to_owned(), MentionKind::Import),
            ("AppStrictQuery".to_owned(), MentionKind::Import),
            ("AppQuery".to_owned(), MentionKind::Pattern),
            ("AppQuery".to_owned(), MentionKind::CallSite),
            ("AppStrictQuery".to_owned(), MentionKind::CallSite),
            ("AppQuery".to_owned(), MentionKind::CallSite),
            ("AppStrictQuery".to_owned(), MentionKind::Unattributed),
            ("AppStrictQuery".to_owned(), MentionKind::Unattributed),
        ],
        "the accountant must attribute imports, patterns and ascriptions, see nothing in prose \
         or a test module, and refuse a `type` alias and a renaming import"
    );
}

#[test]
fn the_mention_accountant_refuses_the_two_alias_shapes() {
    // Separated from the sequence assertion above on purpose: an `assert_eq!` on
    // a whole derived vector panics first, so a check written after it in the
    // same test runs only on a value already proved equal. That is the vacuous
    // shape the review found nine of in this file, and the two rows below are
    // the ones that must stay live — they are the finding.
    let mentions = extractor_mentions_in("fixture.rs", MENTION_SHAPES);
    let refused: Vec<usize> = mentions
        .iter()
        .filter(|m| m.kind == MentionKind::Unattributed)
        .map(|m| m.line)
        .collect();
    assert_eq!(
        refused.len(),
        2,
        "the `type` alias and the renaming `use` must BOTH be refused, and nothing else in the \
         fixture may be: {mentions:#?}"
    );
    let aliased = MENTION_SHAPES
        .lines()
        .position(|l| l.starts_with("type AliasedStrict"))
        .expect("the `type` alias row is in the fixture")
        + 1;
    let renamed = MENTION_SHAPES
        .lines()
        .position(|l| l.contains("as RenamedStrict"))
        .expect("the renaming import row is in the fixture")
        + 1;
    assert_eq!(
        refused,
        vec![aliased, renamed],
        "the refusal must name the line the alias is written on, because that is the only thing \
         the author has to go on"
    );
}

// ---------------------------------------------------------------------------
// Proving the two extractors, not only extending them
// ---------------------------------------------------------------------------

/// A synthetic source carrying one handler argument of every shape, plus every
/// prose channel and every look-alike that must contribute nothing.
///
/// Kept beside the parse it exercises rather than in a fixture file, because the
/// point is that reading it and reading the parser are one act.
///
/// The extension proof — add a call site, watch it be covered with no test edit —
/// is **necessary and not sufficient**: the site you add is by construction in
/// the shape the parser already handles. A sibling register passed its own
/// proof while missing every non-bare shape, and this file's first version
/// passed six tests while blind to two shapes. Every row below marked REVIEW is one the review actually beat the
/// parse with, or one it found the old fixture had placed where a naive scanner
/// stripped it anyway.
const CALL_SITE_SHAPES: &str = r##"
//! AppQuery(doc): AppQuery<NeverSeenInModuleDoc>
//!
//! A doc comment that QUOTES `#[cfg(test)]` while explaining why something is not
//! gated. `src/http/mod.rs` has two of these (`:539`, `:550`) and they truncated
//! this register's view of that whole file until 2026-08-27 — see
//! `production_code`. Every real call site below sits AFTER this line, so a
//! parse that cuts the raw source instead of the lexed source sees none of them.
const ALSO_NOT_A_CUT: &str = "#[cfg(test)]";
/* AppQuery(block): AppQuery<NeverSeenInBlockComment>
   /* AppQuery(nested): AppQuery<NeverSeenInNestedComment> */
   REVIEW: the look-alike below sits AFTER the inner comment closes, so a
   non-nesting scanner resumes in code here and sees it. The old fixture put
   its payload INSIDE the inner comment, where a naive scanner stripped it
   anyway, which made the whole depth counter a dead guard.
   AppQuery(after_nested): AppQuery<NeverSeenAfterANestedComment> */
const PROSE: &str = "AppQuery(s): AppQuery<NeverSeenInStringLiteral>";
const RAW_PROSE: &str = r#"AppQuery(r): AppQuery<NeverSeenInRawLiteral>"#;
/* REVIEW: a raw string containing a bare `"`. Without `raw_string_len` the
   lexer treats the `"` after `r#` as an ordinary string, closes it on the
   EMBEDDED quote, and resumes reading code inside the literal. */
const RAW_WITH_QUOTE: &str = r#"a " quote, then AppQuery(q): AppQuery<NeverSeenAfterARawQuote>"#;
const SEP: char = '"';
const ESCAPED_QUOTE: char = '\'';

pub(crate) struct AppQuery<T>(pub(crate) T);
struct AppStrictQuery<T>(T);

impl<S, T> FromRequestParts<S> for AppQuery<T>
where
    Query<T>: FromRequestParts<S, Rejection = QueryRejection>,
{
}

const _: fn() = || {
    fn assert_from_request_parts<T: FromRequestParts<AppState>>() {}
    assert_from_request_parts::<AppQuery<LenientProbeQuery>>();
    assert_from_request_parts::<AppStrictQuery<StrictProbeQuery>>();
};

async fn bare(AppQuery(params): AppQuery<BareShape>) {}

async fn strict_bare(AppStrictQuery(params): AppStrictQuery<StrictBareShape>) {}

async fn qualified(AppQuery(p): crate::http::AppQuery<QualifiedShape>) {}

async fn split_across_lines(
    State(state): State<AppState>,
    AppStrictQuery(params): AppStrictQuery<
        SplitShape,
    >,
) {
}

async fn wildcard_pattern(AppQuery(_): AppQuery<WildcardShape>) {}

async fn generic_argument(AppQuery(p): AppQuery<Wrapper<Inner>>) {}

// REVIEW (MUT-2): a legal handler that does NOT destructure the extractor. The
// field is `pub(crate)`, so the body reads `params.0`. There is no `):` anywhere
// in this signature and the old anchor derived zero call sites from it.
async fn not_destructured(params: AppStrictQuery<NonDestructuredShape>) {}

async fn not_destructured_qualified(p: crate::http::AppQuery<NonDestructuredQualifiedShape>) {}

async fn mut_bound(mut params: AppQuery<MutBoundShape>) {}

async fn other_extractors(
    Path(id): Path<Uuid>,
    Extension(current): Extension<CurrentAccount>,
    AppJson(body): AppJson<NotAQueryType>,
) {
}

// REVIEW (MUT-1): an item-level `#[cfg(test)]` inside a PRODUCTION impl, the
// exact shape of `src/http/rate_limit.rs:73`. The old truncating cut discarded
// everything below this point — including the production method beside it and
// the call site after it.
struct HasATestOnlyAccessor;

impl HasATestOnlyAccessor {
    #[cfg(test)]
    fn only_in_tests(&self) -> [u8; 2] {
        [0; 2]
    }

    fn in_production(&self) -> usize {
        1
    }
}

#[cfg(test)]
const GATED_CONST: usize = 0;

async fn after_an_item_level_cfg_test(AppQuery(p): AppQuery<SeenAfterAnItemLevelCfgTest>) {}

#[cfg(test)]
mod tests {
    async fn in_a_test_module(AppQuery(p): AppQuery<NeverSeenInTestModule>) {}
}

async fn after_a_test_module(AppStrictQuery(p): AppStrictQuery<SeenAfterATestModule>) {}

// REVIEW (mechanism verification, Med 1): the fifth shape, and the row that says
// what this parse does NOT do. A `type` alias's `=` is not an ascription colon
// and the handler argument never names the extractor, so BOTH lines derive
// nothing here — asserted as an absence in
// `the_lexer_hides_every_look_alike_the_fixture_plants`. The parse was not
// extended to resolve aliases; the duty was inverted instead, and
// `every_extractor_mention_under_src_is_accounted_for` is what reds on it.
type AliasedExtractor = crate::http::AppStrictQuery<NeverSeenThroughATypeAlias>;

async fn through_an_alias(params: AliasedExtractor) {}
"##;

#[test]
fn the_call_site_parse_sees_every_shape_and_no_prose() {
    let sites = call_sites_in("fixture.rs", CALL_SITE_SHAPES);
    let seen: Vec<(String, String)> = sites
        .iter()
        .map(|s| (s.extractor.clone(), s.query_type.clone()))
        .collect();
    assert_eq!(
        seen,
        vec![
            ("AppQuery".to_owned(), "BareShape".to_owned()),
            ("AppStrictQuery".to_owned(), "StrictBareShape".to_owned()),
            ("AppQuery".to_owned(), "QualifiedShape".to_owned()),
            ("AppStrictQuery".to_owned(), "SplitShape".to_owned()),
            ("AppQuery".to_owned(), "WildcardShape".to_owned()),
            ("AppQuery".to_owned(), "Wrapper".to_owned()),
            (
                "AppStrictQuery".to_owned(),
                "NonDestructuredShape".to_owned()
            ),
            (
                "AppQuery".to_owned(),
                "NonDestructuredQualifiedShape".to_owned()
            ),
            ("AppQuery".to_owned(), "MutBoundShape".to_owned()),
            (
                "AppQuery".to_owned(),
                "SeenAfterAnItemLevelCfgTest".to_owned()
            ),
            (
                "AppStrictQuery".to_owned(),
                "SeenAfterATestModule".to_owned()
            ),
        ],
        "every handler-argument shape must be seen — destructured and not, bare, strict, \
         path-qualified, split across lines, `_`-patterned, `mut`-bound, generic, and after both \
         an item-level `#[cfg(test)]` and a whole test module — and nothing else"
    );
}

#[test]
fn the_lexer_hides_every_look_alike_the_fixture_plants() {
    // Separated from the shape proof on purpose. The review found these nine
    // names asserted in a loop placed AFTER an exact-equality assert on the same
    // derived value, so `assert_eq!` panicked first and the loop body only ever
    // ran on a set that provably contained none of them — a vacuous guard.
    // Here the derived set is recomputed from a
    // fixture that is NOT compared for equality, so each name is a live check.
    let sites = call_sites_in("fixture.rs", CALL_SITE_SHAPES);
    for absent in [
        "NeverSeenInModuleDoc",
        "NeverSeenInBlockComment",
        "NeverSeenInNestedComment",
        "NeverSeenAfterANestedComment",
        "NeverSeenInStringLiteral",
        "NeverSeenInRawLiteral",
        "NeverSeenAfterARawQuote",
        "NeverSeenInTestModule",
        // REVIEW (Med 1): a `type` alias derives nothing, which is the finding
        // and not a fix. `every_extractor_mention_under_src_is_accounted_for` is
        // where that becomes a failure.
        "NeverSeenThroughATypeAlias",
        // The extractor's own declaration, its `impl` header, and the trait
        // assertion all name `AppQuery<` without a type-ascription colon.
        "T",
        "LenientProbeQuery",
        "StrictProbeQuery",
        "NotAQueryType",
    ] {
        assert!(
            !sites.iter().any(|s| s.query_type == absent),
            "`{absent}` reached the call-site set from prose, a declaration, an impl header, \
             a trait assertion, a non-query extractor or a test module"
        );
    }
}

#[test]
fn the_char_literal_scan_measures_every_escape() {
    // L4: this returned 3 for `'\''`, a FOUR-char literal, leaving a stray `'`
    // behind. Its own doc says an unskipped quote would make every scan in this
    // file vacuous, so it is asserted directly rather than through a fixture row
    // whose failure mode is incidental.
    for (literal, expected) in [
        ("'x'", Some(3)),
        ("'\\n'", Some(4)),
        ("'\\''", Some(4)),
        ("'\\\\'", Some(4)),
        ("'\\u{2026}'", Some(10)),
        ("'\"'", Some(3)),
        // A lifetime, not a literal.
        ("'a, ", None),
        ("'static>", None),
    ] {
        let chars: Vec<char> = literal.chars().collect();
        assert_eq!(
            char_literal_len(&chars, 0),
            expected,
            "char_literal_len misread {literal:?}"
        );
    }
}

/// A synthetic source carrying one struct declaration of every shape that can
/// precede — or fail to precede — the attribute.
const DECLARATION_SHAPES: &str = r##"
//! #[serde(deny_unknown_fields)]
//! struct ProseDenies {}
//!
//! A doc comment quoting `#[cfg(test)]`, for the same reason the call-site
//! fixture carries one: every declaration below sits after it.

/// A doc comment mentioning deny_unknown_fields, above a struct that does not.
#[derive(Deserialize)]
struct DocMentionsOnly {
    a: Option<u32>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct OwnAttribute {
    a: Option<u32>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct CombinedAttribute {
    a: Option<u32>,
}

/* REVIEW (M1): a WRAPPED attribute. The old line-by-line walk broke on any line
   not starting with `#[` — including this attribute's own closing `)]` — so this
   struct read as LENIENT while the compiled struct is strict. On a
   lenient-classified type that direction is silent. */
#[derive(Deserialize)]
#[serde(
    rename_all = "camelCase",
    deny_unknown_fields,
)]
struct MultiLineAttribute {
    a: Option<u32>,
}

/* REVIEW (M2): feature-gated strictness. A bare `contains` reports `true` while
   the compiled struct is lenient in every build without the feature. */
#[derive(Deserialize)]
#[cfg_attr(feature = "pedantic", serde(deny_unknown_fields))]
struct ConditionalAttribute {
    a: Option<u32>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct NeighbourOfADenier {
    a: Option<u32>,
}

#[derive(Deserialize)]
struct TupleShape(u32);

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct UnitShape;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GenericShape<T>
where
    T: Clone,
{
    a: Option<T>,
}

#[derive(Deserialize)]
struct NoSpaceShape{
    a: Option<u32>,
}

/* A longer name starting with a shorter one must not be mistaken for it. */
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct UnitShapeExtended {
    a: Option<u32>,
}

const NOT_A_STRUCT: &str = "#[serde(deny_unknown_fields)] struct InAStringLiteral {}";

#[cfg(test)]
mod tests {
    #[serde(deny_unknown_fields)]
    struct InATestModule {}
}
"##;

#[test]
fn the_declaration_scan_sees_every_shape_and_no_prose() {
    // The SECOND extractor in this file, proved separately from the first. A
    // derived guard needs the extractor tested against a synthetic source
    // carrying every shape, not just an extension proof — and this scan is the
    // one that decides whether a route
    // performs the strictness its signature and its `StrictQuery` impl promise,
    // so a false answer here inverts the binding.
    for (ty, expected) in [
        ("DocMentionsOnly", Some(Denial::No)),
        ("OwnAttribute", Some(Denial::Yes)),
        ("CombinedAttribute", Some(Denial::Yes)),
        ("MultiLineAttribute", Some(Denial::Yes)),
        ("ConditionalAttribute", Some(Denial::Conditional)),
        // The struct immediately BELOW a denier must not inherit it: the
        // attribute run stops at the first non-attribute, non-modifier token.
        ("NeighbourOfADenier", Some(Denial::No)),
        ("TupleShape", Some(Denial::No)),
        ("UnitShape", Some(Denial::Yes)),
        ("GenericShape", Some(Denial::Yes)),
        ("NoSpaceShape", Some(Denial::No)),
        ("UnitShapeExtended", Some(Denial::Yes)),
        // Prose and test modules declare nothing.
        ("ProseDenies", None),
        ("InAStringLiteral", None),
        ("InATestModule", None),
    ] {
        assert_eq!(
            denies_unknown_fields_in(DECLARATION_SHAPES, ty),
            expected,
            "the declaration scan misread `{ty}`"
        );
    }
}

#[test]
fn the_declaration_scan_refuses_two_declarations_in_one_file() {
    // The rail the review found blind: the old count was over FILES, and `find`
    // took the first hit, so two declarations in the SAME file were read as one.
    const TWICE: &str = r##"
#[serde(deny_unknown_fields)]
struct Twin {
    a: Option<u32>,
}

struct Twin {
    b: Option<u32>,
}
"##;
    let panicked = std::panic::catch_unwind(|| denies_unknown_fields_in(TWICE, "Twin"));
    assert!(
        panicked.is_err(),
        "two declarations of one type in a single file must be a loud refusal, not a coin flip"
    );
}

#[test]
fn the_cfg_test_strip_removes_the_item_and_not_the_rest_of_the_file() {
    // H2 in miniature, asserted on structure rather than on a derived verdict:
    // the gated item goes, its production neighbour stays, and the braces of the
    // enclosing `impl` still close.
    let stripped = production_code(CALL_SITE_SHAPES);
    assert!(
        !stripped.contains("only_in_tests"),
        "the #[cfg(test)] accessor survived the strip"
    );
    assert!(
        !stripped.contains("GATED_CONST"),
        "the #[cfg(test)] const survived the strip"
    );
    assert!(
        !stripped.contains("in_a_test_module"),
        "the #[cfg(test)] module survived the strip"
    );
    assert!(
        stripped.contains("in_production"),
        "the production method BESIDE the gated one was discarded — this is H2"
    );
    assert!(
        stripped.contains("after_a_test_module"),
        "production code after a test module was discarded — this is H2"
    );
    let (depth, negative) = brace_balance(&stripped);
    assert_eq!(depth, 0, "the strip left {depth} unclosed braces");
    assert!(!negative, "the strip left the brace depth negative");
}
