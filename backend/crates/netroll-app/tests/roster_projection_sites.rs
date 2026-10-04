// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
//! The `RosterEntry` projection register, mechanised.
//!
//! Every site reading a `RosterEntry` destructures it exhaustively, with no
//! `..`, so a new domain field fails to compile until a human places it. The
//! compiler enforces the pattern; this file enforces that the pattern is there.

const EXPORT_RS: &str = include_str!("../../netroll-domain/src/export.rs");
const FOLD_RS: &str = include_str!("../../netroll-domain/src/fold.rs");
const DELIVERY_RS: &str = include_str!("../src/delivery.rs");
/// `netroll-app/src/http/net_sessions.rs` — the HTTP handlers. NOT the adapter
/// file of the same name; that one is `PG_NET_SESSIONS_RS`.
const HTTP_NET_SESSIONS_RS: &str = include_str!("../src/http/net_sessions.rs");
/// `netroll-adapters/src/pg/net_sessions.rs` — the Postgres session repo, and
/// the home of `DefinitionSnapshot::from_definition`. NOT the HTTP file of the
/// same name; that one is `HTTP_NET_SESSIONS_RS`.
const PG_NET_SESSIONS_RS: &str = include_str!("../../netroll-adapters/src/pg/net_sessions.rs");
const NET_MOD_RS: &str = include_str!("../../netroll-domain/src/net/mod.rs");

/// One registered source file: every exhaustive destructure of `struct_name`
/// in its production half, and how many fields each of those sites withholds.
struct Guarded {
    /// Workspace-relative, and the label every assertion message names. The two
    /// `net_sessions.rs` rows are DIFFERENT FILES in different crates; the path
    /// is the only thing that tells them apart.
    path: &'static str,
    source: &'static str,
    /// Which struct — and therefore which count this row answers to.
    struct_name: &'static str,
    /// The pattern's opening token, PER ROW rather than global: `NetDefinition`
    /// is also a bare return type (`-> NetDefinition {`), so its row narrows to
    /// the `let` form rather than spanning a function body. See the header.
    pattern_open: &'static str,
    /// Where the field list is parsed from, and the declaration to find in it.
    fields_source: &'static str,
    fields_open: &'static str,
    /// The declaration's field count, asserted EXACTLY against the parsed list so
    /// a silently short parse cannot leave the every-field-named loop vacuous.
    /// A struct gaining a field reds here first, and only a human edit to this
    /// register greens it — the register is where counts live. Snapshot
    /// 2026-09-07, run from `backend/crates/netroll-domain/src`: `RosterEntry`
    /// 20 by
    /// `awk '/^pub struct RosterEntr[y] \{/,/^\}/' fold.rs | grep -c '^    pub '`,
    /// `NetDefinition` 17 by
    /// `awk '/^pub struct NetDefinitio[n] \{/,/^\}/' net/mod.rs | grep -c '^    pub '`.
    field_count: usize,
    /// Withheld (`: _,`) arm count per site, IN SOURCE ORDER.
    /// `len()` is this file's site count.
    withheld: &'static [usize],
}

/// The opening token of a `RosterEntry` pattern, in ANY position — `let`,
/// `if let`, a `match` arm, a closure parameter, or behind a qualified path
/// (`fold::RosterEntry { … }`). Matched at an identifier boundary so
/// `WebhookRosterEntry {` and `PublicRosterEntry {` are not mistaken for it.
const ROSTER_ENTRY_PATTERN_OPEN: &str = "RosterEntry {";

/// The opening token of a `NetDefinition` pattern — the `let` form ONLY, for
/// the reason the header gives: the bare token also opens a function body
/// after `-> NetDefinition`.
const NET_DEFINITION_PATTERN_OPEN: &str = "let NetDefinition {";

const ROSTER_ENTRY_FIELDS_OPEN: &str = "pub struct RosterEntry {";
const NET_DEFINITION_FIELDS_OPEN: &str = "pub struct NetDefinition {";

/// The register: each guarded source, the struct it destructures, and the
/// withheld-arm count of each of its projection sites in SOURCE ORDER.
///
/// Adding a projection means adding an element to its file's `withheld` slice;
/// the site count or a withheld count changing by accident is the failure this
/// file exists to make loud. Moving a function within a guarded file reorders
/// the slice and reds the test where the moved sites' counts differ — the
/// assertion prints both vectors, so the fix is a one-element register edit and
/// reads as one. It does NOT hold for an equal-valued pair: `edit_check_in`'s
/// two arms can trade places unseen (see the header).
///
/// The numbers here are the ONLY statement of each site's withheld count. The
/// sites' own comments name the fields and the reasons; they do not repeat the
/// number.
///
/// NOT COVERED: a projection that reads a field directly (`entry.name`)
/// rather than destructuring `RosterEntry`, which this register cannot see by
/// construction (measured: 42 textual near-misses, zero of which were actual
/// projections). Also not covered: a paired field that crosses from carried to
/// withheld (or back) at a site whose withheld COUNT stays the same — arity,
/// not identity, is what the counts below check.
const GUARDED: [Guarded; 4] = [
    Guarded {
        path: "netroll-domain/src/export.rs",
        source: EXPORT_RS,
        struct_name: "RosterEntry",
        pattern_open: ROSTER_ENTRY_PATTERN_OPEN,
        fields_source: FOLD_RS,
        fields_open: ROSTER_ENTRY_FIELDS_OPEN,
        field_count: 20,
        // `to_csv` (drops only the fold-internal three — the row id, the
        // optimistic-concurrency version and the correction history), then
        // `to_adif` (a spec-bounded tag set: a field lands only where ADIF 3.1.4
        // names a tag for it, and it names none for relay, provenance or
        // net-control run-state).
        withheld: &[3, 11],
    },
    Guarded {
        path: "netroll-app/src/delivery.rs",
        source: DELIVERY_RS,
        struct_name: "RosterEntry",
        pattern_open: ROSTER_ENTRY_PATTERN_OPEN,
        fields_source: FOLD_RS,
        fields_open: ROSTER_ENTRY_FIELDS_OPEN,
        field_count: 20,
        // `build_webhook_payload` (the published webhook JSON — drops the same
        // fold-internal three as the CSV), then `build_summary_email`'s body
        // roster (a HUMAN summary: callsign, name and location, with the full
        // record travelling in the attachments).
        withheld: &[3, 17],
    },
    Guarded {
        path: "netroll-app/src/http/net_sessions.rs",
        source: HTTP_NET_SESSIONS_RS,
        struct_name: "RosterEntry",
        pattern_open: ROSTER_ENTRY_PATTERN_OPEN,
        fields_source: FOLD_RS,
        fields_open: ROSTER_ENTRY_FIELDS_OPEN,
        field_count: 20,
        // `build_summary` (the owner body — withholds only the `added_seq` log
        // cursor), then `build_public_view` (the redaction, where the
        // default answer for a new field is NO), then `edit_check_in`'s two
        // arms in order — the staff partial-body resolution and the self staying
        // toggle, where a field the site fails to read is WIPED rather than
        // merely omitted. Both arms read exactly the
        // `CheckinUpdated` field set and withhold the envelope.
        withheld: &[1, 11, 9, 9],
    },
    Guarded {
        path: "netroll-adapters/src/pg/net_sessions.rs",
        source: PG_NET_SESSIONS_RS,
        struct_name: "NetDefinition",
        pattern_open: NET_DEFINITION_PATTERN_OPEN,
        fields_source: NET_MOD_RS,
        fields_open: NET_DEFINITION_FIELDS_OPEN,
        field_count: 17,
        // `DefinitionSnapshot::from_definition`: the frozen snapshot a session
        // starts with carries the
        // render-relevant fields and withholds provenance the row already has
        // in its own columns plus scheduling, visibility, ownership and
        // lifecycle. The site's own comment states each reason.
        withheld: &[9],
    },
];

/// How many sites project a `RosterEntry`. Asserted against the register below,
/// so it cannot drift from it silently.
///
/// A new FIELD reopens every destructure without changing this number, and that
/// is the normal case; this number changing is the loud one. A registered
/// `NetDefinition` site does not move it either — this is a statement about
/// `RosterEntry`, and the sum below is filtered to that struct so it stays one.
const PROJECTION_SITE_COUNT: usize = 8;

/// Where a source's test module begins. Everything from here down is EXCLUDED
/// from the scan: a test fixture builds and destructures `RosterEntry` freely
/// and that is not a projection. Verified 2026-08-26 — scanning test modules
/// made a `let RosterEntry { callsign, .. }` inside `export.rs`'s own
/// `mod tests` turn all three assertions RED, one of them accusing the author of
/// disarming a production guard. A guard that reds on a behaviourally
/// meaningless edit is the guard the next person deletes.
const TEST_MODULE_OPEN: &str = "#[cfg(test)]";

fn production_source(source: &str) -> &str {
    match source.find(TEST_MODULE_OPEN) {
        Some(at) => &source[..at],
        None => source,
    }
}

/// The text between a pattern's opening `{` and its matching `}`. Brace-matched
/// rather than scanned for `} = ` so a `match`-arm or closure-parameter pattern,
/// which closes with `} =>` or `}|`, is spanned correctly too.
fn pattern_body(rest: &str) -> &str {
    let mut depth: usize = 1;
    for (index, ch) in rest.char_indices() {
        match ch {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return &rest[..index];
                }
            }
            _ => {}
        }
    }
    panic!("a destructure pattern that never closes");
}

/// The pattern regions of every guarded destructure in one source file, with
/// `//` comments stripped so a field name mentioned in prose cannot stand in for
/// a binding.
fn destructure_patterns(source: &str, pattern_open: &str) -> Vec<String> {
    let source = production_source(source);
    let mut patterns = Vec::new();
    let mut cursor = 0;
    while let Some(offset) = source[cursor..].find(pattern_open) {
        let open = cursor + offset;
        let body_start = open + pattern_open.len();
        let at_boundary = source[..open]
            .chars()
            .next_back()
            .is_none_or(|c| !(c.is_ascii_alphanumeric() || c == '_'));
        cursor = body_start;
        if !at_boundary {
            continue;
        }
        let body = pattern_body(&source[body_start..]);
        cursor = body_start + body.len();
        patterns.push(
            body.lines()
                .map(|line| line.split("//").next().unwrap_or(""))
                .collect::<Vec<_>>()
                .join("\n"),
        );
    }
    patterns
}

/// The field names of a struct, read off its declaration rather than
/// hand-listed here — a hand-maintained copy is a failure mode this project has
/// shipped and had to fix before.
fn struct_fields(source: &str, fields_open: &str) -> Vec<String> {
    let start = source.find(fields_open).expect("the struct declaration");
    let body = &source[start..];
    let end = body.find("\n}").expect("the struct's closing brace");
    body[..end]
        .lines()
        .filter_map(|line| {
            let line = line.trim();
            let name = line.strip_prefix("pub ")?.split(':').next()?;
            // Skips the declaration line itself, which also starts `pub `.
            name.chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_')
                .then(|| name.to_owned())
        })
        .collect()
}

/// The `_`-bound arms of one destructure pattern: the fields the site
/// deliberately withholds.
///
/// The predicate is the one the sites' own published commands used
/// (`grep -c ': _,$'`), and the trailing comma is load-bearing: a naive
/// `matches(": _").count()` also counts `grid: _grid,`, so renaming a withheld
/// arm into a live binding would leave this count — and the test — unmoved.
/// (A private helper with a doc comment, against the house rule, because the
/// predicate is a non-obvious invariant and the WHY is the point.)
fn withheld_arms(pattern: &str) -> usize {
    pattern
        .lines()
        .filter(|line| line.trim_end().ends_with(": _,"))
        .count()
}

fn identifiers(text: &str) -> Vec<String> {
    text.split(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
        .filter(|token| !token.is_empty())
        .map(str::to_owned)
        .collect()
}

#[test]
fn the_register_names_every_roster_entry_projection_site() {
    // Filtered to `RosterEntry` because `PROJECTION_SITE_COUNT` is a statement
    // about that struct and is cited as one; the `NetDefinition` row is guarded
    // by every loop below but deliberately not counted here.
    let total: usize = GUARDED
        .iter()
        .filter(|guarded| guarded.struct_name == "RosterEntry")
        .map(|guarded| guarded.withheld.len())
        .sum();
    assert_eq!(
        total, PROJECTION_SITE_COUNT,
        "the register and the stated count must be the same number"
    );
    for guarded in &GUARDED {
        // No `..`: this file asks every site it guards to name every field, so
        // it holds itself to it. A field added to `Guarded` is `E0027` here, not
        // a silent pass.
        let Guarded {
            path,
            source,
            struct_name,
            pattern_open,
            fields_source: _,
            fields_open: _,
            field_count: _,
            withheld,
        } = guarded;
        assert_eq!(
            destructure_patterns(source, pattern_open).len(),
            withheld.len(),
            "{path} holds a different number of `{struct_name}` destructures than the register says; \
             a site was added, removed, or disarmed"
        );
    }
}

#[test]
fn no_projection_site_has_been_disarmed_with_a_rest_pattern() {
    // The disarm the compiler cannot see: `..` makes the site accept a new
    // domain field forever, silently, and fmt/clippy/the suite all stay green.
    for guarded in &GUARDED {
        for pattern in destructure_patterns(guarded.source, guarded.pattern_open) {
            assert!(
                !pattern.contains(".."),
                "a `{}` destructure in {} carries a `..` rest pattern, \
                 which disarms the forward guard at that site:\n{pattern}",
                guarded.struct_name,
                guarded.path
            );
        }
    }
}

#[test]
fn every_projection_site_names_every_roster_entry_field() {
    for guarded in &GUARDED {
        let fields = struct_fields(guarded.fields_source, guarded.fields_open);
        assert_eq!(
            fields.len(),
            guarded.field_count,
            "the `{}` declaration behind {} parsed to a different number of fields than the \
             register says (actual vs expected); a field was added, or the parse fell short and \
             the loop below would be vacuous: {fields:?}",
            guarded.struct_name,
            guarded.path
        );
        for pattern in destructure_patterns(guarded.source, guarded.pattern_open) {
            let bound = identifiers(&pattern);
            for field in &fields {
                assert!(
                    bound.contains(field),
                    "a `{}` destructure in {} does not name `{field}`, \
                     so that field is neither carried nor deliberately dropped there:\n{pattern}",
                    guarded.struct_name,
                    guarded.path
                );
            }
        }
    }
}

#[test]
fn every_projection_site_withholds_exactly_what_the_register_says() {
    // A field silently joining or leaving a projection — one unpaired move —
    // changes this vector at exactly one index; one joining as another leaves
    // the same site does not (see the header). A function moved within the file
    // permutes it, where the counts differ. Both print as the whole
    // actual-vs-expected pair so the reader sees which.
    for guarded in &GUARDED {
        let actual: Vec<usize> = destructure_patterns(guarded.source, guarded.pattern_open)
            .iter()
            .map(|pattern| withheld_arms(pattern))
            .collect();
        assert_eq!(
            actual, guarded.withheld,
            "{}'s `{}` destructures withhold a different set of fields, site by site in source \
             order, than the register says (actual vs expected); a field crossed into or out of a \
             projection, or a site moved",
            guarded.path, guarded.struct_name
        );
    }
}
