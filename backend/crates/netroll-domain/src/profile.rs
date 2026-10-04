// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
//! Profile field validation, Gravatar derivation, and the profile-override
//! rule.
//!
//! The Maidenhead grid grammar is hand-rolled character logic on purpose —
//! same reasoning that kept `regex` out of this crate for callsigns.

use thiserror::Error;

/// A validated, canonicalized Maidenhead grid locator: uppercase field,
/// digits, optional lowercase subsquare, optional extended digits
/// (`FN31`, `FN31pr`, `FN31pr47`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Grid(String);

impl Grid {
    /// The canonical locator as a string slice.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Consumes the newtype, yielding the canonical locator.
    pub fn into_inner(self) -> String {
        self.0
    }
}

impl core::fmt::Display for Grid {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(&self.0)
    }
}

/// Why a submitted grid locator was rejected. Each variant's message is
/// surfaced verbatim as the problem+json `detail`: specific, per-reason.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum GridError {
    /// Input is empty or whitespace-only.
    #[error("grid is empty")]
    Empty,
    /// Input contains a character outside ASCII letters and digits.
    #[error("grid may only contain letters and digits")]
    IllegalCharacter,
    /// Length after trim is not 4, 6, or 8 characters.
    #[error("grid must be 4, 6, or 8 characters")]
    BadLength,
    /// First pair must be letters `A`–`R`.
    #[error("grid must start with two letters A through R")]
    BadField,
    /// Second pair must be digits.
    #[error("grid characters 3-4 must be digits")]
    BadSquare,
    /// Optional third pair must be letters `A`–`X`.
    #[error("grid characters 5-6 must be letters A through X")]
    BadSubsquare,
    /// Optional fourth pair must be digits.
    #[error("grid characters 7-8 must be digits")]
    BadExtended,
}

/// Names the character a guard rejected, as a noun phrase an operator can act
/// on. A rejected character is almost always invisible — NEL, VT and FF arrive
/// as real line separators in legacy-encoded pastes, and a bidi override is a
/// zero-width spoofing primitive — so "find and remove it" is only possible if
/// the message identifies WHICH character, not which class it belongs to.
///
/// The two a person can already see and name get their everyday word; anything
/// else is identified by code point, which is the only handle a text editor's
/// find box will take.
pub fn describe_illegal_character(offender: char) -> String {
    match offender {
        '\n' | '\r' => "a line break".to_owned(),
        '\t' => "a tab".to_owned(),
        _ => format!("a hidden character (U+{:04X})", offender as u32),
    }
}

/// Why a free-text profile field (display name, location, avatar URL) was
/// rejected. Messages are a lowercase `"<what would make it acceptable>"`
/// fragment: the composing callers (`NetDefinitionError`,
/// `DiscoveryQueryError`, `DeliveryConfigError`) prefix `"<field>: "` and the
/// frontend renders the result beside the named input, so this text reaches the
/// problem+json `detail` verbatim on those paths.
///
/// It does NOT reach the wire verbatim everywhere. The profile PUT and the
/// roster-entry edit render their `detail` in a SECTION-level alert with no
/// field to attach it to, so they compose a full sentence from this variant
/// instead (`netroll-app`'s `profile_field_message`). Changing the wording here
/// changes the composed paths; changing what the sentence paths say is that
/// function's job.
///
/// `TooLong` and `IllegalCharacter` CARRY the fact their message needs: the
/// bound that was exceeded, and the character that offended. Without
/// the payload no caller could say more than "value contains a control
/// character", which is a rule the reader has to map back to their own input —
/// and which could not be true of all three guards at once, since
/// [`parse_bounded_multiline_text`] accepts the newline the other two refuse.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum ProfileError {
    /// Value exceeds the field's maximum length after trimming; carries that
    /// field's own ceiling, which is the only thing that makes the message
    /// actionable and which only the guard's caller knows.
    #[error("must be {max_chars} characters or fewer")]
    TooLong {
        /// The field's post-trim character ceiling.
        max_chars: usize,
    },
    /// Value contains a control or bidi-control character; carries the
    /// offending code point so the message can name it.
    #[error("remove {}", describe_illegal_character(*.0))]
    IllegalCharacter(char),
    /// Avatar URL does not start with `https://` followed by a host.
    #[error("avatar URL must start with https://")]
    NotHttps,
    /// Avatar URL contains internal whitespace.
    #[error("avatar URL must not contain spaces")]
    WhitespaceInUrl,
}

/// Parses and canonicalizes a Maidenhead grid locator.
///
/// Accepts 4, 6, or 8 characters (field pair `A`–`R`, square digits,
/// optional subsquare `A`–`X`, optional extended digits) in any case,
/// trimmed; canonical form is uppercase field/square with lowercase
/// subsquare (`FN31pr47` — ham display convention). Rejections are typed
/// [`GridError`] variants, one per reason.
pub fn parse_grid(input: &str) -> Result<Grid, GridError> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return Err(GridError::Empty);
    }
    if !trimmed.chars().all(|c| c.is_ascii_alphanumeric()) {
        return Err(GridError::IllegalCharacter);
    }
    if !matches!(trimmed.len(), 4 | 6 | 8) {
        return Err(GridError::BadLength);
    }

    let bytes = trimmed.as_bytes();
    let mut canonical = String::with_capacity(bytes.len());

    // Field pair: letters A–R, canonically uppercase.
    for &b in &bytes[0..2] {
        let upper = b.to_ascii_uppercase();
        if !(b'A'..=b'R').contains(&upper) || !b.is_ascii_alphabetic() {
            return Err(GridError::BadField);
        }
        canonical.push(upper as char);
    }
    // Square pair: digits.
    for &b in &bytes[2..4] {
        if !b.is_ascii_digit() {
            return Err(GridError::BadSquare);
        }
        canonical.push(b as char);
    }
    // Optional subsquare pair: letters A–X, canonically lowercase.
    if bytes.len() >= 6 {
        for &b in &bytes[4..6] {
            let lower = b.to_ascii_lowercase();
            if !(b'a'..=b'x').contains(&lower) || !b.is_ascii_alphabetic() {
                return Err(GridError::BadSubsquare);
            }
            canonical.push(lower as char);
        }
    }
    // Optional extended pair: digits.
    if bytes.len() == 8 {
        for &b in &bytes[6..8] {
            if !b.is_ascii_digit() {
                return Err(GridError::BadExtended);
            }
            canonical.push(b as char);
        }
    }

    Ok(Grid(canonical))
}

/// True for Unicode bidirectional-control code points (explicit
/// embeddings/overrides/isolates, plus the LRM/RLM marks). These are not
/// `char::is_control` (they are category Cf, not Cc) but a display name or
/// location containing one can visually reorder/spoof the rest of the
/// string wherever it's rendered — the same class of attack known from
/// RTL-override filename spoofing. Free text otherwise stays unicode-
/// welcome (names are not callsigns); this blocks only the reordering
/// primitives, not scripts or combining marks.
pub(crate) fn is_bidi_control(c: char) -> bool {
    matches!(
        c,
        '\u{200E}' | '\u{200F}' | '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}'
    )
}

/// Trims and bounds a **single-line** free-text field: rejects every control
/// character — `\n` included — and anything longer than `max_chars` post-trim.
/// Unicode welcome. For prose that legitimately contains line breaks, use
/// [`parse_bounded_multiline_text`] instead.
///
/// `pub(crate)`: the net-definition text fields (title,
/// country/state, repeater texts) reuse this guard — past the three-strike
/// threshold, so shared rather than duplicated. `description` was one of them
/// until it moved to the prose guard; it is deliberately no longer
/// on this list, and widening this function to admit `\n` would silently
/// change the contract of every other field it serves.
pub(crate) fn parse_bounded_text(input: &str, max_chars: usize) -> Result<String, ProfileError> {
    let trimmed = input.trim();
    if let Some(offender) = trimmed
        .chars()
        .find(|c| c.is_control() || is_bidi_control(*c))
    {
        return Err(ProfileError::IllegalCharacter(offender));
    }
    if trimmed.chars().count() > max_chars {
        return Err(ProfileError::TooLong { max_chars });
    }
    Ok(trimmed.to_owned())
}

// A run of three or more line breaks is a paste artefact, not a paragraph: the
// author's own structure is one break or one blank line, and everything beyond
// that renders as empty page. Kept private and inline rather than exported —
// a second call site would be a way to reach the collapse while bypassing the
// guard that gives it its meaning. One pass, no regex: `netroll-domain` is a
// purity manifest where every dependency carries a justifying comment, and this
// loop is smaller than that argument would be.
fn collapse_blank_line_runs(input: &str) -> String {
    let mut collapsed = String::with_capacity(input.len());
    let mut consecutive_newlines = 0usize;
    for character in input.chars() {
        if character == '\n' {
            consecutive_newlines += 1;
            if consecutive_newlines <= 2 {
                collapsed.push(character);
            }
        } else {
            consecutive_newlines = 0;
            collapsed.push(character);
        }
    }
    collapsed
}

/// Trims and bounds a free-text field that is **prose**: normalizes CRLF and
/// lone-CR line endings to `\n`, collapses a run of three or more consecutive
/// newlines to two, rejects control characters EXCEPT the
/// embedded newline a multi-line value legitimately contains, rejects
/// bidi-control characters, and rejects anything longer than `max_chars`
/// post-trim. Unicode welcome.
///
/// The blank-line collapse is here rather than at the render layer because the
/// bound is defined as `max_chars` **after** newline normalisation — so a value
/// of nothing but newlines is legal, storable, and (under
/// `white-space: pre-line` on the public net page) renders as thousands of
/// empty line boxes. CSS does not collapse segment breaks under any `pre-*`
/// value, so the stored value is the only place the two can be made to agree.
/// A run of one or two newlines is the author's own paragraph structure and is
/// left exactly as written.
///
/// A sibling of [`parse_bounded_text`] rather than a replacement for it,
/// because the two guard different shapes of field. `parse_bounded_text`
/// guards single-line values (a name, a callsign-adjacent token, a repeater
/// text) where refusing every control character is correct. This one guards
/// prose captured in a multi-line textarea — a check-in note
/// (`CheckInDetailModal.tsx`/`NetNotePanel.tsx`) or a net
/// definition's description (`NetDefinitionFormPage.tsx`) —
/// where refusing `\n` refuses the very content the field exists to capture.
///
/// Normalizing on the way in (rather than at the HTTP layer) is load-bearing
/// for change detection, not cosmetic: callers such as `changed_fields`
/// compare stored against submitted **by value**, so a client sending `\r\n`
/// against a stored `\n` would otherwise report a change on every save with
/// nothing having changed.
///
/// The bound is a parameter so each field keeps its own ceiling — callers
/// pass their own constant and never inherit another field's.
/// Blank-after-trim folding is left to the caller, matching how
/// [`parse_bounded_text`] and the optional-text helpers already divide that
/// responsibility.
pub(crate) fn parse_bounded_multiline_text(
    input: &str,
    max_chars: usize,
) -> Result<String, ProfileError> {
    let line_endings_normalized = input.replace("\r\n", "\n").replace('\r', "\n");
    // Ordered deliberately between the two steps around it. AFTER
    // the CR pass, because `\r\n\r\n\r\n` holds no two ADJACENT `\n` to find
    // until the CRLF pairs are gone; BEFORE the bound, because the bound is
    // defined as "max_chars after newline normalisation" and this extends that
    // normalisation. Before the `trim()` rather than after is immaterial to the
    // result — trim strips leading and trailing newlines either way — and is
    // chosen so the character scan, the length count and the returned value all
    // operate on the one canonical string that will actually be stored.
    let normalized = collapse_blank_line_runs(&line_endings_normalized);
    let trimmed = normalized.trim();
    if let Some(offender) = trimmed
        .chars()
        .find(|c| (c.is_control() && *c != '\n') || is_bidi_control(*c))
    {
        return Err(ProfileError::IllegalCharacter(offender));
    }
    if trimmed.chars().count() > max_chars {
        return Err(ProfileError::TooLong { max_chars });
    }
    Ok(trimmed.to_owned())
}

/// Parses a display name: trimmed, ≤ 64 chars, no control characters,
/// unicode allowed (names are not callsigns).
pub fn parse_display_name(input: &str) -> Result<String, ProfileError> {
    parse_bounded_text(input, 64)
}

/// Parses a free-text location: trimmed, ≤ 128 chars, no control
/// characters.
pub fn parse_location(input: &str) -> Result<String, ProfileError> {
    parse_bounded_text(input, 128)
}

const MAX_AVATAR_URL_CHARS: usize = 512;

/// Parses an avatar reference: either a user-supplied `https://` URL
/// (trimmed, non-empty remainder, ≤ 512 chars, no whitespace or control
/// characters) or a path to an avatar this instance stores itself
/// (`/avatars/<file>`, see [`crate::avatar`]).
///
/// The stored-path case exists because the upload endpoint writes that path
/// into the same `avatar_url` column, and the profile PUT round-trips whatever
/// the client currently holds — rejecting our own path would make "save
/// display name" fail for every account with an uploaded avatar. Only the
/// exact one-file-under-the-prefix shape is accepted, so this cannot be used
/// to point an account at an arbitrary local path.
///
/// Deliberately NOT the `url` crate: this value only ever lands in an
/// `<img src>` React attribute, so a scheme prefix + character checks are
/// the whole threat model — a full URL parser adds a dependency for zero
/// additional safety here.
pub fn parse_avatar_url(input: &str) -> Result<String, ProfileError> {
    let trimmed = input.trim();
    if let Some(offender) = trimmed
        .chars()
        .find(|c| c.is_control() || is_bidi_control(*c))
    {
        return Err(ProfileError::IllegalCharacter(offender));
    }
    if trimmed.chars().any(char::is_whitespace) {
        return Err(ProfileError::WhitespaceInUrl);
    }
    if crate::avatar::is_stored_avatar_path(trimmed) {
        if trimmed.chars().count() > MAX_AVATAR_URL_CHARS {
            return Err(ProfileError::TooLong {
                max_chars: MAX_AVATAR_URL_CHARS,
            });
        }
        return Ok(trimmed.to_owned());
    }
    // Scheme is case-insensitive per RFC 3986 — `HTTPS://…` is exactly as
    // valid as `https://…`; only the scheme comparison is case-folded, the
    // stored value (including its host/path case) is left untouched.
    if trimmed.len() < 8 || !trimmed.as_bytes()[..8].eq_ignore_ascii_case(b"https://") {
        return Err(ProfileError::NotHttps);
    }
    let remainder = &trimmed[8..];
    if remainder.is_empty() {
        return Err(ProfileError::NotHttps);
    }
    if trimmed.chars().count() > MAX_AVATAR_URL_CHARS {
        return Err(ProfileError::TooLong {
            max_chars: MAX_AVATAR_URL_CHARS,
        });
    }
    Ok(trimmed.to_owned())
}

/// Derives the Gravatar URL for an already-normalized email.
///
/// Takes the email exactly as stored — accounts persist it trimmed and
/// lowercased on the way in, and re-normalizing here would silently paper over a
/// caller passing raw input. Gravatar's current contract (verified
/// 2026-07-15) is SHA-256 hex; `?d=mp` yields the "mystery person"
/// placeholder instead of a 404 for emails with no Gravatar.
pub fn gravatar_url(normalized_email: &str) -> String {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(normalized_email.as_bytes());
    let mut hex = String::with_capacity(64);
    for b in digest {
        hex.push_str(&format!("{b:02x}"));
    }
    format!("https://gravatar.com/avatar/{hex}?d=mp")
}

/// Precedence: the profile value wins over a looked-up value.
///
/// The persisted override contract. It applies when staff log a check-in for
/// an operator with a profile, and when callbook lookup autofills
/// name/location — in both, a populated profile field beats the lookup.
pub fn prefer_profile_value<'a>(
    profile: Option<&'a str>,
    looked_up: Option<&'a str>,
) -> Option<&'a str> {
    profile.or(looked_up)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn canonical(input: &str) -> String {
        parse_grid(input)
            .unwrap_or_else(|e| panic!("expected {input:?} to parse, got {e:?}"))
            .into_inner()
    }

    #[test]
    fn valid_grids_canonicalize_case_and_trim() {
        assert_eq!(canonical("FN31"), "FN31");
        assert_eq!(canonical("fn31"), "FN31");
        assert_eq!(canonical("FN31pr"), "FN31pr");
        assert_eq!(canonical("FN31PR"), "FN31pr");
        assert_eq!(canonical("fn31pr47"), "FN31pr47");
        assert_eq!(canonical("  FN31 "), "FN31");
    }

    #[test]
    fn wrong_lengths_are_bad_length() {
        assert_eq!(parse_grid("FN3"), Err(GridError::BadLength));
        assert_eq!(parse_grid("FN31p"), Err(GridError::BadLength));
        assert_eq!(parse_grid("FN31pr4"), Err(GridError::BadLength));
    }

    #[test]
    fn empty_and_whitespace_only_are_empty() {
        assert_eq!(parse_grid(""), Err(GridError::Empty));
        assert_eq!(parse_grid("   "), Err(GridError::Empty));
    }

    #[test]
    fn field_pair_violations_are_bad_field() {
        // Beyond A–R.
        assert_eq!(parse_grid("SS11"), Err(GridError::BadField));
        // Must be letters.
        assert_eq!(parse_grid("F131"), Err(GridError::BadField));
        assert_eq!(parse_grid("11FN"), Err(GridError::BadField));
    }

    #[test]
    fn square_pair_must_be_digits() {
        assert_eq!(parse_grid("FNAA"), Err(GridError::BadSquare));
    }

    #[test]
    fn subsquare_beyond_a_through_x_is_bad_subsquare() {
        assert_eq!(parse_grid("FN31zz"), Err(GridError::BadSubsquare));
    }

    #[test]
    fn extended_pair_must_be_digits() {
        assert_eq!(parse_grid("FN31prXY"), Err(GridError::BadExtended));
    }

    #[test]
    fn illegal_characters_are_rejected() {
        assert_eq!(parse_grid("FN-31"), Err(GridError::IllegalCharacter));
        assert_eq!(parse_grid("FN 31"), Err(GridError::IllegalCharacter));
    }

    #[test]
    fn grid_parse_is_idempotent_on_its_own_output() {
        for input in ["fn31", "FN31PR", "fn31pr47"] {
            let normalized = canonical(input);
            assert_eq!(canonical(&normalized), normalized);
        }
    }

    #[test]
    fn display_name_trims_bounds_and_accepts_unicode() {
        assert_eq!(parse_display_name("  Maria "), Ok("Maria".to_owned()));
        assert_eq!(
            parse_display_name("José ÑØ1X"),
            Ok("José ÑØ1X".to_owned()),
            "names are not callsigns — unicode is welcome"
        );
        assert_eq!(parse_display_name(&"x".repeat(64)), Ok("x".repeat(64)));
        assert_eq!(
            parse_display_name(&"x".repeat(65)),
            Err(ProfileError::TooLong { max_chars: 64 })
        );
        // 64 chars post-trim still fits even when the raw input is longer.
        assert_eq!(
            parse_display_name(&format!("  {}  ", "x".repeat(64))),
            Ok("x".repeat(64))
        );
        assert_eq!(
            parse_display_name("Mar\u{0007}ia"),
            Err(ProfileError::IllegalCharacter('\u{0007}'))
        );
        assert_eq!(
            parse_display_name("Mar\nia"),
            Err(ProfileError::IllegalCharacter('\n'))
        );
    }

    #[test]
    fn display_name_rejects_bidi_override_characters() {
        // U+202E (RIGHT-TO-LEFT OVERRIDE) is category Cf, not Cc, so a bare
        // `is_control` check would miss it — this is the same spoofing
        // primitive behind RTL-override filename attacks.
        assert_eq!(
            parse_display_name("Mar\u{202E}ia"),
            Err(ProfileError::IllegalCharacter('\u{202E}'))
        );
        assert_eq!(
            parse_display_name("\u{200E}Maria\u{200F}"),
            Err(ProfileError::IllegalCharacter('\u{200E}'))
        );
    }

    #[test]
    fn an_illegal_character_rejection_carries_the_offending_code_point() {
        // NEL/VT/FF arrive in legacy-encoded pastes as real
        // line separators. The operator can only remove one they can identify,
        // so the error has to CARRY it — asserting on the payload, never on the
        // rendered sentence.
        assert_eq!(
            parse_display_name("a\u{0085}b"),
            Err(ProfileError::IllegalCharacter('\u{0085}'))
        );
        assert_eq!(
            parse_bounded_multiline_text("a\u{000B}b", 64),
            Err(ProfileError::IllegalCharacter('\u{000B}'))
        );
        assert_eq!(
            parse_avatar_url("https://example.com/a\u{000C}b.png"),
            Err(ProfileError::IllegalCharacter('\u{000C}'))
        );
        assert_eq!(
            parse_display_name("Mar\u{202E}ia"),
            Err(ProfileError::IllegalCharacter('\u{202E}')),
            "a bidi override is Cf, not Cc, and must still be nameable"
        );
    }

    #[test]
    fn the_single_line_and_multiline_guards_are_distinguishable_on_a_newline() {
        // ONE variant serves three guards, one of which
        // accepts `\n`. The rejections must not be interchangeable, or no single
        // sentence can be true of all three.
        let single_line_newline = parse_bounded_text("a\nb", 64);
        assert_eq!(
            single_line_newline,
            Err(ProfileError::IllegalCharacter('\n'))
        );
        assert_eq!(
            parse_bounded_multiline_text("a\nb", 64),
            Ok("a\nb".to_owned()),
            "the prose guard accepts the newline the single-line guard refuses"
        );
        assert_ne!(
            single_line_newline,
            parse_bounded_text("a\u{000B}b", 64),
            "a newline rejection is not interchangeable with a vertical-tab one"
        );
        assert_ne!(
            single_line_newline,
            parse_bounded_multiline_text("a\u{000B}b", 64),
            "nor with the prose guard's vertical-tab rejection"
        );
    }

    #[test]
    fn a_run_of_three_or_more_newlines_collapses_to_one_blank_line() {
        // The BOUNDARY is the criterion, not the hazard input:
        // a single newline and a single blank line are the author's own
        // paragraph structure and must survive byte-identically, while any
        // longer run folds to exactly one blank line. A test that only proved
        // the 1998-newline case could not tell this apart from an
        // implementation that collapses every run to one `\n` and destroys the
        // paragraphs the prose guard exists to keep.
        for (input, expected, why) in [
            ("a\nb", "a\nb", "a single line break is untouched"),
            (
                "a\n\nb",
                "a\n\nb",
                "one blank line is a paragraph break, kept",
            ),
            ("a\n\n\nb", "a\n\nb", "three is the first run that folds"),
            ("a\n\n\n\n\nb", "a\n\nb", "longer runs fold to the same one"),
        ] {
            assert_eq!(
                parse_bounded_multiline_text(input, 64),
                Ok(expected.to_owned()),
                "{why}"
            );
        }
        // The hazard input itself: exactly MAX-sized, all newlines, and it is
        // the demonstration rather than the proof.
        let hazard = format!("a{}b", "\n".repeat(1998));
        assert_eq!(
            parse_bounded_multiline_text(&hazard, 2000),
            Ok("a\n\nb".to_owned())
        );
    }

    #[test]
    fn newline_runs_collapse_after_cr_normalisation_not_before() {
        // First half, asserted as BEHAVIOUR rather than read
        // off the source order: in `"a\r\n\r\n…b"` no two `\n` are adjacent
        // until the CRLF pass has run, so a collapse placed before that pass
        // would find no run at all and leave ten newlines standing.
        let crlf = format!("a{}b", "\r\n".repeat(5));
        assert_eq!(
            parse_bounded_multiline_text(&crlf, 64),
            Ok("a\n\nb".to_owned())
        );
        let lone_cr = format!("a{}b", "\r".repeat(5));
        assert_eq!(
            parse_bounded_multiline_text(&lone_cr, 64),
            Ok("a\n\nb".to_owned())
        );
    }

    #[test]
    fn the_collapse_is_applied_before_the_length_bound() {
        // Second half. 2002 characters raw, 4 after the
        // collapse: the bound is defined as "max_chars AFTER normalisation",
        // so extending that normalisation has to happen before the count or
        // the sentence stops being true. A collapse placed after the bound
        // returns TooLong here.
        let over_bound_only_by_newlines = format!("a{}b", "\n".repeat(2000));
        assert_eq!(
            parse_bounded_multiline_text(&over_bound_only_by_newlines, 2000),
            Ok("a\n\nb".to_owned())
        );
    }

    proptest::proptest! {
        /// Invariant: no value the prose guard accepts can contain a run of
        /// three consecutive newlines, whatever mixture of breaks and text
        /// went in.
        #[test]
        fn no_parsed_prose_value_ever_contains_three_consecutive_newlines(
            raw in proptest::collection::vec(
                proptest::prelude::prop_oneof!["\n", "\r\n", "\r", "x", " "],
                0..64,
            ),
        ) {
            let joined: String = raw.concat();
            if let Ok(parsed) = parse_bounded_multiline_text(&joined, 2000) {
                proptest::prop_assert!(!parsed.contains("\n\n\n"), "{parsed:?}");
            }
        }
    }

    #[test]
    fn a_too_long_rejection_carries_the_bound_it_exceeded() {
        // "what would make it acceptable" is the bound, and the
        // shared guard is the only place that knows it.
        assert_eq!(
            parse_display_name(&"x".repeat(65)),
            Err(ProfileError::TooLong { max_chars: 64 })
        );
        assert_eq!(
            parse_location(&"x".repeat(129)),
            Err(ProfileError::TooLong { max_chars: 128 })
        );
        assert_ne!(
            parse_display_name(&"x".repeat(65)),
            parse_location(&"x".repeat(129)),
            "two fields with different ceilings must not answer identically"
        );
    }

    #[test]
    fn location_trims_bounds_and_stays_free_text() {
        assert_eq!(
            parse_location(" Hartford, CT "),
            Ok("Hartford, CT".to_owned())
        );
        assert_eq!(parse_location(&"x".repeat(128)), Ok("x".repeat(128)));
        assert_eq!(
            parse_location(&"x".repeat(129)),
            Err(ProfileError::TooLong { max_chars: 128 })
        );
        assert_eq!(
            parse_location("Hart\u{0000}ford"),
            Err(ProfileError::IllegalCharacter('\u{0000}'))
        );
        assert_eq!(
            parse_location("Hart\u{202E}ford"),
            Err(ProfileError::IllegalCharacter('\u{202E}'))
        );
    }

    #[test]
    fn avatar_url_requires_https_with_a_remainder() {
        assert_eq!(
            parse_avatar_url(" https://example.com/me.png "),
            Ok("https://example.com/me.png".to_owned())
        );
        assert_eq!(
            parse_avatar_url("http://example.com/me.png"),
            Err(ProfileError::NotHttps)
        );
        assert_eq!(
            parse_avatar_url("javascript:alert(1)"),
            Err(ProfileError::NotHttps)
        );
        assert_eq!(
            parse_avatar_url("data:image/png;base64,AAAA"),
            Err(ProfileError::NotHttps)
        );
        assert_eq!(
            parse_avatar_url("example.com/me.png"),
            Err(ProfileError::NotHttps)
        );
        assert_eq!(parse_avatar_url("https://"), Err(ProfileError::NotHttps));
    }

    #[test]
    fn avatar_url_accepts_a_path_this_instance_stores_itself() {
        // The upload endpoint writes this shape into the same column, and the
        // profile PUT round-trips whatever the client holds — rejecting it
        // would break "save display name" for anyone with an uploaded avatar.
        assert_eq!(
            parse_avatar_url("/avatars/019f-abc.png"),
            Ok("/avatars/019f-abc.png".to_owned())
        );
        assert_eq!(
            parse_avatar_url(" /avatars/019f-abc.webp "),
            Ok("/avatars/019f-abc.webp".to_owned())
        );
    }

    #[test]
    fn avatar_url_still_rejects_other_local_paths_and_traversals() {
        // Only the exact one-file-under-the-prefix shape is ours. Anything
        // else stays an https-only field, so a crafted profile write cannot
        // point an account at an arbitrary local file.
        for rejected in [
            "/avatars/../../etc/passwd",
            "/avatars/nested/dir.png",
            "/avatars/",
            "/etc/passwd",
            "/",
            "//evil.example.com/a.png",
        ] {
            assert_eq!(
                parse_avatar_url(rejected),
                Err(ProfileError::NotHttps),
                "{rejected} must not be accepted"
            );
        }
    }

    #[test]
    fn avatar_url_scheme_match_is_case_insensitive() {
        // RFC 3986: the scheme is case-insensitive. Only the scheme
        // comparison folds case — the rest of the stored value is
        // untouched, so the host/path case a user typed is preserved.
        assert_eq!(
            parse_avatar_url("HTTPS://example.com/me.png"),
            Ok("HTTPS://example.com/me.png".to_owned())
        );
        assert_eq!(
            parse_avatar_url("HttpS://Example.com/Me.png"),
            Ok("HttpS://Example.com/Me.png".to_owned())
        );
    }

    #[test]
    fn avatar_url_rejects_bidi_override_characters() {
        assert_eq!(
            parse_avatar_url("https://example.com/\u{202E}me.png"),
            Err(ProfileError::IllegalCharacter('\u{202E}'))
        );
    }

    #[test]
    fn avatar_url_rejects_length_whitespace_and_control_characters() {
        let long = format!("https://example.com/{}", "a".repeat(512));
        assert_eq!(
            parse_avatar_url(&long),
            Err(ProfileError::TooLong { max_chars: 512 })
        );
        assert_eq!(
            parse_avatar_url("https://example.com/a b.png"),
            Err(ProfileError::WhitespaceInUrl)
        );
        assert_eq!(
            parse_avatar_url("https://example.com/a\u{0007}.png"),
            Err(ProfileError::IllegalCharacter('\u{0007}'))
        );
    }

    #[test]
    fn gravatar_url_is_sha256_hex_of_the_stored_email() {
        // Expected hex computed with the same sha2 dependency, so the test
        // pins the URL shape and hex encoding rather than re-deriving both
        // sides from the function under test.
        use sha2::{Digest, Sha256};
        let email = "op@example.com";
        let expected_hex: String = Sha256::digest(email.as_bytes())
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        assert_eq!(
            gravatar_url(email),
            format!("https://gravatar.com/avatar/{expected_hex}?d=mp")
        );
        // Known full-URL vector: SHA-256("op@example.com").
        assert_eq!(
            gravatar_url(email),
            "https://gravatar.com/avatar/3d1832bf4b7de99f5b04a00c14b543740c80792908d458daa4de697a6d536034?d=mp"
        );
    }

    #[test]
    fn prefer_profile_value_truth_table() {
        // Profile wins when present; looked-up fills the gap; both-empty
        // stays empty. Consumed by staff check-in logging and lookup autofill.
        assert_eq!(
            prefer_profile_value(Some("Maria"), Some("MARIA LOOKUP")),
            Some("Maria")
        );
        assert_eq!(prefer_profile_value(Some("Maria"), None), Some("Maria"));
        assert_eq!(
            prefer_profile_value(None, Some("MARIA LOOKUP")),
            Some("MARIA LOOKUP")
        );
        assert_eq!(prefer_profile_value(None, None), None);
    }
}
