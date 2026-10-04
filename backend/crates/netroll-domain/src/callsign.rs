// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
//! Callsign parsing and normalization. The account callsign is the operator's
//! assigned base call: portable designators (`/P`, `/M`, `DL/`) are
//! operational markers, not identity, so they are accepted on input and
//! stripped on normalization, which keeps `W1AW` and `W1AW/P` from being
//! reservable as two identities.

use thiserror::Error;

/// A validated, normalized base callsign (trimmed, ASCII-uppercased,
/// portable designators stripped).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Callsign(String);

impl Callsign {
    /// The normalized base call as a string slice.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Consumes the newtype, yielding the normalized base call.
    pub fn into_inner(self) -> String {
        self.0
    }
}

impl core::fmt::Display for Callsign {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(&self.0)
    }
}

/// Why a submitted callsign was rejected. Each variant's message is surfaced
/// verbatim as the problem+json `detail` (specific, per-reason).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum CallsignError {
    /// Input is empty or whitespace-only.
    #[error("callsign is empty")]
    Empty,
    /// Input contains a character outside ASCII letters, digits, and `/`.
    #[error("callsign may only contain letters, digits, and '/'")]
    IllegalCharacter,
    /// The base call has no digit separating prefix from suffix.
    #[error("callsign needs a digit separating its prefix and suffix")]
    MissingSeparatorDigit,
    /// Nothing follows the separator digit.
    #[error("callsign needs a suffix after its separator digit")]
    MissingSuffix,
    /// The suffix's final character is not a letter.
    #[error("callsign suffix must end in a letter")]
    SuffixMustEndInLetter,
    /// The prefix contains no letter.
    #[error("callsign prefix must contain a letter")]
    PrefixNeedsLetter,
    /// The base call exceeds seven characters.
    #[error("callsign base is too long (maximum 7 characters)")]
    BaseTooLong,
    /// A `/`-separated segment is empty (trailing or leading slash).
    #[error("callsign has an empty segment around a '/'")]
    EmptyDesignator,
    /// A portable designator segment exceeds four characters.
    #[error("portable designator is too long (maximum 4 characters)")]
    DesignatorTooLong,
    /// More than three `/`-separated segments.
    #[error("callsign has too many '/' segments (maximum 3)")]
    TooManySegments,
    /// No single segment unambiguously reads as the base call.
    #[error("could not identify a single base callsign among the segments")]
    AmbiguousBase,
}

/// Parses and normalizes a submitted callsign to its base call.
///
/// Accepts international forms and portable/compound designators
/// (`w1aw/p`, `DL/N1CCK`, `DL/W1AW/P`), stripping the designators so the
/// stored identity is the assigned base call. Rejections are typed
/// [`CallsignError`] variants, one per reason.
pub fn parse_callsign(input: &str) -> Result<Callsign, CallsignError> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return Err(CallsignError::Empty);
    }
    let upper = trimmed.to_ascii_uppercase();
    if upper
        .chars()
        .any(|c| !(c.is_ascii_uppercase() || c.is_ascii_digit() || c == '/'))
    {
        return Err(CallsignError::IllegalCharacter);
    }

    let segments: Vec<&str> = upper.split('/').collect();
    if segments.len() > 3 {
        return Err(CallsignError::TooManySegments);
    }
    if segments.iter().any(|s| s.is_empty()) {
        return Err(CallsignError::EmptyDesignator);
    }

    match segments.as_slice() {
        [base] => validate_base(base).map(|()| Callsign((*base).to_owned())),
        [a, b] => match (is_base(a), is_base(b)) {
            (true, false) => with_designators(a, &[b]),
            (false, true) => with_designators(b, &[a]),
            // Both or neither segment reads as the base call: there is no
            // single identity to reserve, so refuse rather than guess.
            _ => Err(CallsignError::AmbiguousBase),
        },
        [lead, base, trail] => {
            if !is_base(base) || is_base(lead) || is_base(trail) {
                return Err(CallsignError::AmbiguousBase);
            }
            with_designators(base, &[lead, trail])
        }
        _ => unreachable!("segment count bounded to 1..=3 above"),
    }
}

/// Accepts `base` once every designator segment fits the 1–4 alphanumeric
/// designator grammar (`/P`, `/MM`, `/QRP`, `/4`, `DL/`, `/KH6`).
fn with_designators(base: &str, designators: &[&str]) -> Result<Callsign, CallsignError> {
    for designator in designators {
        if designator.len() > 4 {
            return Err(CallsignError::DesignatorTooLong);
        }
    }
    validate_base(base).map(|()| Callsign(base.to_owned()))
}

fn is_base(segment: &str) -> bool {
    validate_base(segment).is_ok()
}

/// Base grammar: `prefix` (1–3 alphanumerics, at least one letter) + one
/// separator digit + `suffix` (1–4 alphanumerics ending in a letter),
/// 3–7 characters total. Tries every digit as the separator; on total
/// failure, a suffix-shaped complaint (from a split whose prefix was valid)
/// beats a prefix-shaped one, as it names the likelier mistake.
fn validate_base(base: &str) -> Result<(), CallsignError> {
    if base.len() > 7 {
        return Err(CallsignError::BaseTooLong);
    }

    let mut saw_digit = false;
    let mut suffix_error: Option<CallsignError> = None;
    for (i, c) in base.char_indices() {
        if !c.is_ascii_digit() {
            continue;
        }
        saw_digit = true;

        let prefix = &base[..i];
        let suffix = &base[i + 1..];
        let prefix_ok =
            (1..=3).contains(&prefix.len()) && prefix.bytes().any(|b| b.is_ascii_uppercase());
        if !prefix_ok {
            continue;
        }

        let verdict = if suffix.is_empty() {
            Err(CallsignError::MissingSuffix)
        } else if suffix.len() > 4 {
            Err(CallsignError::BaseTooLong)
        } else if !suffix
            .bytes()
            .next_back()
            .is_some_and(|b| b.is_ascii_uppercase())
        {
            Err(CallsignError::SuffixMustEndInLetter)
        } else {
            Ok(())
        };
        match verdict {
            Ok(()) => return Ok(()),
            Err(e) => {
                suffix_error.get_or_insert(e);
            }
        }
    }

    if !saw_digit {
        return Err(CallsignError::MissingSeparatorDigit);
    }
    Err(suffix_error.unwrap_or(CallsignError::PrefixNeedsLetter))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parsed(input: &str) -> String {
        parse_callsign(input)
            .unwrap_or_else(|e| panic!("expected {input:?} to parse, got {e:?}"))
            .into_inner()
    }

    #[test]
    fn valid_calls_normalize_to_uppercase_trimmed_base() {
        assert_eq!(parsed("w1aw"), "W1AW");
        assert_eq!(parsed("  n1cck "), "N1CCK");
        assert_eq!(parsed("2E0abc"), "2E0ABC");
        assert_eq!(parsed("9A1A"), "9A1A");
        assert_eq!(parsed("VE3XYZ"), "VE3XYZ");
        assert_eq!(parsed("K1A"), "K1A");
    }

    #[test]
    fn portable_designators_are_accepted_and_stripped_to_the_base() {
        assert_eq!(parsed("w1aw/p"), "W1AW");
        assert_eq!(parsed("G4ABC/M"), "G4ABC");
        assert_eq!(parsed("W1AW/4"), "W1AW");
        assert_eq!(parsed("K1ABC/MM"), "K1ABC");
        assert_eq!(parsed("DL/N1CCK"), "N1CCK");
        assert_eq!(parsed("DL/W1AW/P"), "W1AW");
    }

    #[test]
    fn empty_and_whitespace_only_are_rejected_as_empty() {
        assert_eq!(parse_callsign(""), Err(CallsignError::Empty));
        assert_eq!(parse_callsign("   "), Err(CallsignError::Empty));
    }

    #[test]
    fn base_grammar_violations_map_to_specific_variants() {
        assert_eq!(
            parse_callsign("ABC"),
            Err(CallsignError::MissingSeparatorDigit)
        );
        assert_eq!(parse_callsign("W1"), Err(CallsignError::MissingSuffix));
        assert_eq!(
            parse_callsign("KA1ABC5"),
            Err(CallsignError::SuffixMustEndInLetter)
        );
        assert_eq!(
            parse_callsign("1234A"),
            Err(CallsignError::PrefixNeedsLetter)
        );
        assert_eq!(parse_callsign("W1ABCDEF"), Err(CallsignError::BaseTooLong));
    }

    #[test]
    fn designator_violations_map_to_specific_variants() {
        assert_eq!(parse_callsign("W1AW/"), Err(CallsignError::EmptyDesignator));
        assert_eq!(
            parse_callsign("W1AW/PORTA"),
            Err(CallsignError::DesignatorTooLong)
        );
    }

    #[test]
    fn illegal_characters_are_rejected() {
        assert_eq!(
            parse_callsign("W 1AW"),
            Err(CallsignError::IllegalCharacter)
        );
        assert_eq!(
            parse_callsign("W1AW!"),
            Err(CallsignError::IllegalCharacter)
        );
        assert_eq!(
            parse_callsign("WØ1AW"),
            Err(CallsignError::IllegalCharacter)
        );
    }

    #[test]
    fn compound_forms_with_no_or_multiple_base_candidates_are_rejected() {
        // Neither segment matches the base grammar.
        assert_eq!(parse_callsign("DL/P"), Err(CallsignError::AmbiguousBase));
        // Both segments match the base grammar.
        assert_eq!(
            parse_callsign("W1AW/K1ABC"),
            Err(CallsignError::AmbiguousBase)
        );
        assert_eq!(
            parse_callsign("DL/W1AW/P/QRP"),
            Err(CallsignError::TooManySegments)
        );
    }

    #[test]
    fn parse_is_idempotent_on_its_own_output() {
        for input in ["w1aw/p", "DL/N1CCK", "  2e0abc ", "9A1A", "DL/W1AW/P"] {
            let normalized = parsed(input);
            assert_eq!(
                parsed(&normalized),
                normalized,
                "re-parsing the normalized form of {input:?} must be stable"
            );
        }
    }
}
