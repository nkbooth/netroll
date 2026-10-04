// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
//! The delivery-destination register, mechanised.
//!
//! One destination's permanent failure must not suppress the others. The risk
//! is COVERAGE, not structure, so this register DERIVES the kind list by parsing
//! `DeliveryDestinationKind` out of `delivery.rs` rather than restating it.

const DELIVERY_RS: &str = include_str!("../src/delivery.rs");
const ON_CLOSE_TESTS_RS: &str = include_str!("api_on_close_delivery.rs");
const DURABILITY_TESTS_RS: &str = include_str!("api_delivery_durability.rs");

/// Each declared destination kind and the `#[test]` that proves its permanent
/// failure does not suppress the others. The KEYS are checked against the enum
/// declaration in both directions, so this table cannot silently fall behind it
/// or carry a kind that no longer exists.
///
/// **What this register does NOT see.** It can require a row per kind and
/// require the named function to exist; it cannot check that the scenario is
/// MEANINGFUL — a scenario that arms the kind and asserts nothing satisfies
/// every check here. It also reads Rust as TEXT, so coverage is derived only
/// for kinds declared as variants of `DeliveryDestinationKind`: a destination
/// declared some OTHER way — a trait object, a `const` table, a second enum, a
/// `#[cfg]`-gated variant — is invisible to this file, a limit stated here
/// rather than implied away.
const INDEPENDENCE_SCENARIOS: [(&str, &str); 3] = [
    (
        "Email",
        "a_permanently_failing_email_does_not_block_the_other_email_or_the_webhook",
    ),
    (
        "Webhook",
        "a_permanently_failing_webhook_does_not_suppress_the_email_or_the_announcement",
    ),
    (
        "Discord",
        "a_permanently_failing_discord_destination_still_delivers_the_email_and_the_webhook",
    ),
];

/// Each declared destination kind and the `#[test]` that proves its RECOVERY
/// POLICY: what happens to a leg whose attempt was in flight
/// when the process died. The policy is derived from the kind's dedupe basis —
/// webhook and email carry a key derived from the session id and are retried;
/// Discord has none and an interrupted POST is abandoned rather than doubled —
/// so a fourth kind cannot ship without stating its basis and proving what
/// recovery does with it. Same both-directions check as the table above.
///
/// Webhook and Discord name the SAME scenario deliberately: it interrupts one
/// close carrying both and asserts both halves in one test, because asserting
/// only "Discord not re-posted" passes against a system that recovers nothing.
const DURABILITY_SCENARIOS: [(&str, &str); 3] = [
    (
        "Email",
        "an_email_delivery_aborted_in_flight_is_recovered_and_completed_by_a_later_sweep",
    ),
    (
        "Webhook",
        "recovery_retries_an_interrupted_webhook_and_never_re_posts_an_interrupted_discord",
    ),
    (
        "Discord",
        "recovery_retries_an_interrupted_webhook_and_never_re_posts_an_interrupted_discord",
    ),
];

/// Everything from the first `#[cfg(test)]` down is excluded from every scan: a
/// test module names variants freely and that is not a declaration. Same
/// reasoning — and the same lesson — as `roster_projection_sites.rs`, where
/// scanning test modules made a fixture look like a disarmed production guard.
fn production_source(source: &str) -> &str {
    match source.find("#[cfg(test)]") {
        Some(at) => &source[..at],
        None => source,
    }
}

/// Whether `chars[i..]` begins with `pat`.
fn starts(chars: &[char], i: usize, pat: &str) -> bool {
    pat.chars()
        .enumerate()
        .all(|(n, c)| chars.get(i + n) == Some(&c))
}

/// The length in CHARS of the raw or byte-raw string literal starting at `i`
/// (`r"…"`, `r#"…"#`, `br##"…"##`), or `None` when there is none. Refuses a
/// trailing `r`/`br` of an identifier (`for`, `iter`, `usize`) being read as a
/// prefix.
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
/// `'\u{2026}'`), or `None` when that `'` opens a LIFETIME (`'a`, `'static`)
/// instead.
///
/// Skipped rather than ignored for one reason: an unskipped `'"'` would open a
/// phantom string literal and swallow the rest of the file, which would make
/// every scan below vacuous.
fn char_literal_len(chars: &[char], i: usize) -> Option<usize> {
    if chars.get(i + 1) == Some(&'\\') {
        let mut at = i + 2;
        while at < chars.len() && chars[at] != '\'' {
            at += 1;
        }
        return (at < chars.len()).then_some(at + 1 - i);
    }
    (chars.get(i + 2) == Some(&'\'')).then_some(3)
}

/// The production source with every COMMENT and the BODY of every string
/// literal removed — i.e. Rust code and nothing that merely LOOKS like it.
///
/// Not optional, and not tidiness. This register's own guard has now been
/// inverted twice by prose:
///
/// - 2026-08-27, by the dev: `delivery.rs`'s doc comment for
///   `combine_leg_storage_results` QUOTES the very destructure shape this
///   register forbids, so a scan over the raw text found the PROSE before the
///   code. The guard was red in the unmutated tree and green under the mutation
///   it exists to catch — exactly inverted. The fix stripped `//` lines.
/// - 2026-08-27, by independent review: that fix was HALF a fix. A `/* … */`
///   block comment quoting a correct destructure, placed above the call site,
///   passed the guard while the real call site discarded the Discord leg.
///
/// So this is a small lexer rather than a second special case bolted onto a
/// list of rejected shapes — the shape that broke it was the one not on the
/// list, and a list will always have a next one. It drops line comments (`//`,
/// `///`, `//!`), NESTED block comments (Rust permits `/* /* */ */`, so a
/// non-nesting scanner stops at the wrong `*/`), and the CONTENTS of ordinary,
/// raw (`r#"…"#`) and byte (`br"…"`) string literals. The string half also
/// closes the latent hole the old `line.split("//")` had: it truncated a line at
/// a `://` inside a string literal, so a URL in code could blind the scan.
/// Anything that reads Rust as text has to do this.
fn code_only(source: &str) -> String {
    let chars: Vec<char> = production_source(source).chars().collect();
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
                    // Newlines survive, so a comment cannot join two code lines
                    // into one and make a line-oriented scan below misread them.
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
            i += consumed;
            continue;
        }
        if chars[i] == '"' {
            i += 1;
            while i < chars.len() && chars[i] != '"' {
                if chars[i] == '\\' {
                    i += 1;
                }
                i += 1;
            }
            i += 1;
            out.push_str("\"\"");
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

/// The leading identifier of one comma-separated piece of an enum body, pushed
/// onto `names` when it is a variant name.
fn push_variant(piece: &str, names: &mut Vec<String>) {
    let flat = piece.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut rest = flat.trim();
    // Attributes may precede a variant on their own line or on the same one.
    while let Some(after) = rest.strip_prefix("#[") {
        match after.find(']') {
            Some(at) => rest = after[at + 1..].trim_start(),
            None => return,
        }
    }
    let name: String = rest
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
        .collect();
    if !name.is_empty() && name.starts_with(|c: char| c.is_ascii_uppercase()) {
        names.push(name);
    }
}

/// The variant names of `DeliveryDestinationKind`, read off the declaration in
/// `delivery.rs` rather than hand-listed here.
///
/// **Every variant SHAPE is seen, not only a bare identifier.** The body is
/// split on TOP-LEVEL commas — depth-tracked through `(`, `[` and `{`, so a
/// comma inside a payload or a generic argument list does not split a variant —
/// and the leading identifier of each piece is the name. That covers
/// `Matrix`, `Matrix(&'static str)`, `Matrix { url: String }`, `Matrix = 3`,
/// an attributed `#[serde(rename = "m")] Matrix`, and two variants written on
/// one line.
///
/// The previous parse kept a line only if EVERY character of it was
/// `[A-Za-z0-9_]`, which made all of those invisible. Independent review proved
/// the consequence on 2026-08-27: adding `Matrix(&'static str)` with an
/// `is_armed` arm, ABSENT from `ALL` and with NO independence scenario, left all
/// four tests in this file green — a fourth destination kind the delivery-off
/// predicate could not see. The derivation fired when a kind was added to `ALL`
/// and not when a variant existed outside it, so "derived coverage" was true in
/// one direction only.
fn declared_kinds() -> Vec<String> {
    declared_kinds_in(DELIVERY_RS)
}

/// [`declared_kinds`] over an arbitrary source, so the parse itself can be
/// tested against variant shapes `delivery.rs` does not currently contain.
fn declared_kinds_in(source: &str) -> Vec<String> {
    let source = code_only(source);
    let source = source.as_str();
    let start = source
        .find("enum DeliveryDestinationKind {")
        .expect("the `DeliveryDestinationKind` declaration");
    let body = &source[start..];
    let open = body.find('{').expect("the enum's opening brace") + 1;
    // The enum's own closing brace is the only `}` in column 0 of a rustfmt'ed
    // declaration; a struct-like variant's brace is indented.
    let end = body.find("\n}").expect("the enum's closing brace");
    let mut names = Vec::new();
    let mut depth = 0usize;
    let mut piece = String::new();
    for c in body[open..end].chars() {
        match c {
            '(' | '[' | '{' => {
                depth += 1;
                piece.push(c);
            }
            ')' | ']' | '}' => {
                depth = depth.saturating_sub(1);
                piece.push(c);
            }
            ',' if depth == 0 => {
                push_variant(&piece, &mut names);
                piece.clear();
            }
            _ => piece.push(c),
        }
    }
    push_variant(&piece, &mut names);
    names
}

/// The `ALL` array's literal member list, as text.
fn all_array_members() -> Vec<String> {
    let source = code_only(DELIVERY_RS);
    let source = source.as_str();
    let start = source
        .find("const ALL: [Self; ")
        .expect("the `ALL` declaration");
    let body = &source[start..];
    let end = body.find("];").expect("the array's closing bracket");
    body[..end]
        .split("Self::")
        .skip(1)
        .map(|piece| {
            piece
                .chars()
                .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
                .collect::<String>()
        })
        .collect()
}

/// A synthetic `delivery.rs` carrying one variant of every shape Rust permits,
/// plus the two prose channels that have inverted this file's guards. Kept
/// beside the parse it exercises rather than in a fixture file, because the
/// whole point is that reading it and reading the parser must be one act.
const SHAPES_FIXTURE: &str = r##"
//! Matrix
/* Slack */
const NOT_A_KIND: &str = "Telegram";
#[derive(Debug)]
enum DeliveryDestinationKind {
    Email,
    /// A doc comment naming Webhook twice: Webhook.
    Webhook,
    Discord(&'static str),
    Matrix { url: String },
    Signal = 7,
    #[allow(dead_code)]
    Xmpp,
    Zulip, Irc,
}

#[cfg(test)]
mod tests {
    use super::*;
    enum DeliveryDestinationKind {
        NeverSeen,
    }
}
"##;

#[test]
fn the_parse_sees_every_variant_shape_and_no_prose() {
    // The inverse of the derivation this file exists for, and the half that was
    // missing: a variant the parse cannot see is a destination kind that can
    // ship absent from `ALL`, unseen by the delivery-off predicate, with no
    // independence scenario, and with every test here green. Proved by review
    // on 2026-08-27 with `Matrix(&'static str)`.
    let kinds = declared_kinds_in(SHAPES_FIXTURE);
    assert_eq!(
        kinds,
        vec![
            "Email", "Webhook", "Discord", "Matrix", "Signal", "Xmpp", "Zulip", "Irc"
        ],
        "every variant shape must be seen — payload-carrying, struct-like, \
         discriminant, attributed, and two on one line — and nothing else"
    );
    // Prose channels: a module doc, a block comment, a string literal, and a
    // test-module enum must contribute nothing.
    for prose in ["Slack", "Telegram", "NeverSeen"] {
        assert!(
            !kinds.contains(&prose.to_owned()),
            "`{prose}` reached the kind list from a comment, a string literal or a test \
             module: {kinds:?}"
        );
    }
}

#[test]
fn a_forbidden_shape_cannot_be_smuggled_past_the_scan_as_prose() {
    // Both inversions this file has actually suffered, as one assertion. The
    // scan must see the CODE (`let (_, w, _) =`) and never the prose that
    // quotes the correct shape.
    const SMUGGLED: &str = r##"
// let (_, webhook_result, discord_result) = tokio::join!(a, b, c);
/// let (_, webhook_result, discord_result) = tokio::join!(a, b, c);
/*
   let (_, webhook_result, discord_result) = tokio::join!(a, b, c);
   /* nested: let (_, webhook_result, discord_result) = tokio::join!(a, b, c); */
*/
const DOCS: &str = "let (_, webhook_result, discord_result) = tokio::join!(a, b, c);";
const RAW: &str = r#"let (_, webhook_result, discord_result) = tokio::join!(a, b, c);"#;
const SEP: char = '"';
fn real() {
    let (_, webhook_result, _) = tokio::join!(a, b, c);
}
"##;
    let code = code_only(SMUGGLED);
    assert!(
        code.contains("let (_, webhook_result, _) = tokio::join!"),
        "the real call site must survive the scan: {code}"
    );
    assert!(
        !code.contains("webhook_result, discord_result"),
        "a correct destructure quoted in a comment, a doc comment, a nested block comment, \
         a string literal or a raw string must NOT survive the scan: {code}"
    );
    assert_eq!(
        code.matches("tokio::join!").count(),
        1,
        "exactly one `tokio::join!` survives — the code one: {code}"
    );
}

#[test]
fn the_declaration_is_parsed_and_is_not_empty() {
    // The derivation itself is fallible: a rename or a reformat that made the
    // parser return nothing would make every assertion below vacuously true.
    let kinds = declared_kinds();
    assert!(
        kinds.len() >= 3,
        "the kind list is parsed from `delivery.rs`, not hand-written; it should not be \
         short or empty: {kinds:?}"
    );
    assert!(kinds.contains(&"Discord".to_owned()));
}

#[test]
fn the_all_array_covers_every_declared_kind() {
    // `DeliveryDestinationKind::ALL` is what the deliverer's delivery-off
    // predicate iterates. A variant missing from it is a destination the
    // predicate cannot see — the defect a hand-spelled version of that
    // predicate had.
    let declared = declared_kinds();
    let listed = all_array_members();
    for kind in &declared {
        assert!(
            listed.contains(kind),
            "`DeliveryDestinationKind::{kind}` is declared but missing from `ALL`, so the \
             delivery-off predicate cannot see it; listed: {listed:?}"
        );
    }
    assert_eq!(
        listed.len(),
        declared.len(),
        "`ALL` lists {} members for {} declared kinds",
        listed.len(),
        declared.len()
    );
}

#[test]
fn every_declared_kind_has_an_independence_scenario_that_exists() {
    let declared = declared_kinds();
    for kind in &declared {
        let scenario = INDEPENDENCE_SCENARIOS
            .iter()
            .find(|(name, _)| name == kind)
            .map(|(_, scenario)| *scenario)
            .unwrap_or_else(|| {
                panic!(
                    "`DeliveryDestinationKind::{kind}` has no independence scenario in this \
                     register. A permanent failure of this destination must leave \
                     the others delivering; name the test that proves it."
                )
            });
        assert!(
            code_only(ON_CLOSE_TESTS_RS).contains(&format!("async fn {scenario}(")),
            "the independence scenario named for `{kind}` (`{scenario}`) does not exist as a \
             test function in `api_on_close_delivery.rs`"
        );
    }
    for (kind, _) in INDEPENDENCE_SCENARIOS {
        assert!(
            declared.contains(&kind.to_owned()),
            "this register names a scenario for `{kind}`, which is no longer a declared \
             destination kind"
        );
    }
}

#[test]
fn every_declared_kind_has_a_durability_scenario_that_exists() {
    // The register's second required column: a kind's
    // recovery policy is a claim about its dedupe basis, and this is what makes
    // a fourth destination unable to ship without stating one.
    let declared = declared_kinds();
    for kind in &declared {
        let scenario = DURABILITY_SCENARIOS
            .iter()
            .find(|(name, _)| name == kind)
            .map(|(_, scenario)| *scenario)
            .unwrap_or_else(|| {
                panic!(
                    "`DeliveryDestinationKind::{kind}` has no durability scenario in this \
                     register. A leg of this destination interrupted \
                     mid-flight is either retried on a stated dedupe basis or recorded and \
                     never doubled; name the test that proves which."
                )
            });
        assert!(
            code_only(DURABILITY_TESTS_RS).contains(&format!("async fn {scenario}(")),
            "the durability scenario named for `{kind}` (`{scenario}`) does not exist as a \
             test function in `api_delivery_durability.rs`"
        );
    }
    for (kind, _) in DURABILITY_SCENARIOS {
        assert!(
            declared.contains(&kind.to_owned()),
            "this register names a durability scenario for `{kind}`, which is no longer a \
             declared destination kind"
        );
    }
}

#[test]
fn the_deliverer_does_not_discard_a_fallible_legs_result() {
    // The seam this join pattern leaves open. `tokio::join!` runs every leg to
    // completion, but `let (_, webhook_result, _) = tokio::join!(…)` compiles,
    // reads naturally, and DROPS the third leg's `sqlx::Error` before the static
    // log in `spawn_for_closed_session` can ever see it. The behaviour is
    // asserted in `delivery.rs`'s own `mod discord_tests`
    // (`a_storage_error_in_either_fallible_leg_reaches_the_caller`); this
    // assertion is about the CALL SITE, which that unit test cannot reach.
    //
    // EVERY leg reports its outcome — the email fan-out
    // included, because each address's leg row has to move — so the permitted
    // discard count went from one to ZERO. A `_` anywhere in this destructure
    // is a leg whose rows never settle.
    let source = code_only(DELIVERY_RS);
    let source = source.as_str();
    let at = source
        .find("tokio::join!(")
        .expect("the deliverer's `tokio::join!`");
    let before = &source[..at];
    let open = before.rfind("let (").expect("its destructure") + "let (".len();
    let close = open + before[open..].find(')').expect("the destructure's close");
    let bindings: Vec<&str> = before[open..close].split(',').map(str::trim).collect();
    let discarded = bindings.iter().filter(|b| **b == "_").count();
    assert_eq!(
        discarded, 0,
        "NO joined leg may be discarded: every leg returns the outcomes its job rows are \
         settled from, and the fallible legs' storage errors must reach the caller; see \
         `combine_leg_storage_results`. Bindings were: {bindings:?}"
    );
    assert_eq!(
        bindings.len(),
        declared_kinds().len(),
        "one joined leg per declared destination kind; bindings were {bindings:?}"
    );
}
