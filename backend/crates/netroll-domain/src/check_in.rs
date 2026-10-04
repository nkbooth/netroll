// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
//! Per-check-in staff fields: the signal report and the staying status, both
//! additive fields on `checkin.added` rather than a new event kind. The domain
//! carries validated values; the camelCase JSONB seam lives in the adapter.
//! Mode-shaping of the report INPUT is a frontend affordance only — the domain
//! stores a bounded string and enforces no mode-specific format.

use crate::profile::{
    ProfileError, is_bidi_control, parse_bounded_multiline_text, parse_bounded_text,
};

/// The upper bound on a stored signal report, in characters. Generous for
/// every real report family — `599` (RST), `-24 dB` (digital SNR), or
/// `full quieting` (FM qualitative) — while refusing free prose. Mode shaping
/// (RS/RST/dB) is a frontend input affordance; the domain only bounds length
/// and rejects control/bidi characters, reusing the shipped free-text guard.
const MAX_SIGNAL_REPORT_CHARS: usize = 16;

/// A validated, bounded signal report string (trimmed, control/bidi-free,
/// length-bounded). Carries the operator's entry verbatim — `59`, `599`,
/// `-06`, `full quieting` — with no mode-specific format enforcement.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SignalReport(String);

impl SignalReport {
    /// The stored report as a string slice.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl core::fmt::Display for SignalReport {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(&self.0)
    }
}

/// Parses an optional signal report: trims, rejects control/bidi characters,
/// bounds to [`MAX_SIGNAL_REPORT_CHARS`]. A blank-after-trim value is `None`
/// (matching the net-definition optional-text idiom), NOT an empty string.
/// Reuses the shipped [`parse_bounded_text`] guard — no mode-specific
/// format validation.
pub fn parse_signal_report(input: &str) -> Result<Option<SignalReport>, ProfileError> {
    let bounded = parse_bounded_text(input, MAX_SIGNAL_REPORT_CHARS)?;
    if bounded.is_empty() {
        return Ok(None);
    }
    Ok(Some(SignalReport(bounded)))
}

/// The upper bound on a stored per-check-in operator name, in characters.
/// A name is manual staff entry, not autofill; this bounds length and
/// rejects control/bidi characters, reusing
/// the shipped free-text guard. Matches the profile display-name bound (64) —
/// the same "a name is not prose" ceiling.
const MAX_NAME_CHARS: usize = 64;

/// The upper bound on a stored per-check-in location string, in characters.
/// Free-text manual entry ("Hartford, CT") — NOT a Maidenhead
/// grid (that grammar is `profile::parse_grid`). Matches the profile location
/// bound (128).
const MAX_LOCATION_CHARS: usize = 128;

/// A validated, bounded per-check-in operator name (trimmed, control/bidi-free,
/// length-bounded). Manual staff entry captured in the detail modal;
/// carries the entry verbatim with unicode welcome (names are not callsigns).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Name(String);

impl Name {
    /// The stored name as a string slice.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl core::fmt::Display for Name {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(&self.0)
    }
}

/// Parses an optional per-check-in name: trims, rejects control/bidi
/// characters, bounds to [`MAX_NAME_CHARS`]. A blank-after-trim value is `None`
/// (the optional-text idiom), NOT an empty string. Reuses [`parse_bounded_text`].
pub fn parse_name(input: &str) -> Result<Option<Name>, ProfileError> {
    let bounded = parse_bounded_text(input, MAX_NAME_CHARS)?;
    if bounded.is_empty() {
        return Ok(None);
    }
    Ok(Some(Name(bounded)))
}

/// A validated, bounded per-check-in location string (trimmed, control/bidi-free,
/// length-bounded). Free-text manual staff entry captured in the detail modal
/// — a place name, NOT a Maidenhead grid.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Location(String);

impl Location {
    /// The stored location as a string slice.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl core::fmt::Display for Location {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(&self.0)
    }
}

/// Parses an optional per-check-in location: trims, rejects control/bidi
/// characters, bounds to [`MAX_LOCATION_CHARS`]. A blank-after-trim value is
/// `None` (the optional-text idiom). Reuses [`parse_bounded_text`].
pub fn parse_location(input: &str) -> Result<Option<Location>, ProfileError> {
    let bounded = parse_bounded_text(input, MAX_LOCATION_CHARS)?;
    if bounded.is_empty() {
        return Ok(None);
    }
    Ok(Some(Location(bounded)))
}

/// Who created a roster entry: a `staff`/`self` provenance
/// discriminator set at `checkin.added` from the write path. `Staff` is an
/// operator adding a station (the shipped `LogCheckIn` path); `SelfService` is a
/// signed-in participant checking THEMSELVES in (the widened self path). An
/// additive field on the existing `checkin.added` transition, NOT a new kind.
///
/// Wire form is lowercase-kebab: `staff` / `self`. [`Default`] is
/// [`CheckInSource::Staff`] — a historical/omitted `source` folds to `staff`,
/// because no self entry existed before the field did (additive-compat).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CheckInSource {
    /// An operator added the station on someone's behalf (the staff hot path).
    #[default]
    Staff,
    /// A signed-in participant checked themselves in (the self path). The Rust
    /// variant avoids the `Self` keyword; its wire token is `self`.
    SelfService,
}

impl CheckInSource {
    /// The stable lowercase-kebab wire/storage token. The single source
    /// of truth the adapter persists and [`TryFrom`] parses back.
    pub fn as_str(self) -> &'static str {
        match self {
            CheckInSource::Staff => "staff",
            CheckInSource::SelfService => "self",
        }
    }
}

/// An unrecognized `source` wire token. Unlike [`StayingParseError`] it carries
/// no echoed input: `source` is SERVER-SET (never client-supplied on the wire),
/// so this is only reachable if a non-Rust writer stored an out-of-vocabulary
/// value — the adapter maps it to an opaque decode error, never a user message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckInSourceParseError;

impl core::fmt::Display for CheckInSourceParseError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str("unrecognized check-in source token")
    }
}

impl TryFrom<&str> for CheckInSource {
    type Error = CheckInSourceParseError;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        match value {
            "staff" => Ok(CheckInSource::Staff),
            "self" => Ok(CheckInSource::SelfService),
            _ => Err(CheckInSourceParseError),
        }
    }
}

/// Whether a checked-in station stays on frequency for comments/traffic, or
/// signs off after checking in. A binary per-check-in fact set by staff
/// at check-in time, and togglable by the participant themselves.
///
/// Wire form is lowercase-kebab: `staying-for-comments`, `in-and-out`.
/// [`Default`] is [`StayingStatus::InAndOut`] — the conservative "checked in,
/// not committed to staying" — so both an omitted request field and a
/// historical field-less event fold to a well-defined value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum StayingStatus {
    /// The station stays on frequency for comments/traffic.
    StayingForComments,
    /// The station checked in, then signs off — the conservative default.
    #[default]
    InAndOut,
}

impl StayingStatus {
    /// The stable lowercase-kebab wire/storage token. The single source
    /// of truth the adapter persists and [`TryFrom`] parses back.
    pub fn as_str(self) -> &'static str {
        match self {
            StayingStatus::StayingForComments => "staying-for-comments",
            StayingStatus::InAndOut => "in-and-out",
        }
    }
}

/// An unrecognized `staying` wire token (review finding: the API handler was
/// hand-echoing the raw caller-supplied token with only a length bound, no
/// control/bidi filtering — inconsistent with the `RoleInvalid`/
/// `SignalReportInvalid` convention of surfacing a typed domain error's
/// `Display`). Carries a bounded, control/bidi-sanitized echo of the invalid
/// input so the API can embed `e.to_string()` directly, like its siblings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StayingParseError {
    echoed: String,
}

/// The max characters of an unrecognized token echoed back in an error
/// message — generous for any plausible typo/garbage input, bounded against
/// abuse.
const MAX_ECHOED_STAYING_TOKEN_CHARS: usize = 64;

impl core::fmt::Display for StayingParseError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "unrecognized staying token: {}", self.echoed)
    }
}

impl TryFrom<&str> for StayingStatus {
    type Error = StayingParseError;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        match value {
            "staying-for-comments" => Ok(StayingStatus::StayingForComments),
            "in-and-out" => Ok(StayingStatus::InAndOut),
            _ => Err(StayingParseError {
                // Filter control/bidi characters (the same class `parse_bounded_text`
                // rejects) before bounding length, so a malicious token can never
                // reorder or hide bytes in the echoed error detail.
                echoed: value
                    .chars()
                    .filter(|c| !c.is_control() && !is_bidi_control(*c))
                    .take(MAX_ECHOED_STAYING_TOKEN_CHARS)
                    .collect(),
            }),
        }
    }
}

/// The traffic/emergency-net precedence of a check-in — a THREE-level
/// scheme, NOT the 4-level ARRL/NTS one: `Routine`,
/// `Priority`, `Emergency`. An additive edit-only field on `checkin.updated`
/// Set via the detail modal, never at check-in add.
///
/// Wire form is lowercase-kebab: `routine` / `priority` / `emergency`.
/// [`Default`] is [`Precedence::Routine`] — the conservative baseline every
/// entry carries from the moment of check-in (like [`StayingStatus::InAndOut`]),
/// so both an omitted request field and a historical field-less event fold to a
/// well-defined value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Precedence {
    /// Ordinary traffic — the conservative default.
    #[default]
    Routine,
    /// Time-sensitive traffic, worked ahead of routine.
    Priority,
    /// Life-and-property emergency traffic, worked first.
    Emergency,
}

impl Precedence {
    /// The stable lowercase-kebab wire/storage token. The single source
    /// of truth the adapter persists and [`TryFrom`] parses back.
    pub fn as_str(self) -> &'static str {
        match self {
            Precedence::Routine => "routine",
            Precedence::Priority => "priority",
            Precedence::Emergency => "emergency",
        }
    }

    /// The sort rank for precedence ordering: highest precedence
    /// worked first, so `Emergency` = 0, `Priority` = 1, `Routine` = 2. A stable
    /// sort on this rank yields the Emergency → Priority → Routine roster order.
    pub fn sort_rank(self) -> u8 {
        match self {
            Precedence::Emergency => 0,
            Precedence::Priority => 1,
            Precedence::Routine => 2,
        }
    }
}

/// An unrecognized `precedence` wire token — carries a bounded, control/bidi-
/// sanitized echo of the invalid input so the API can embed `e.to_string()`
/// directly, exactly like [`StayingParseError`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrecedenceParseError {
    echoed: String,
}

/// The max characters of an unrecognized precedence token echoed back in an
/// error message — generous for any plausible typo, bounded against abuse.
const MAX_ECHOED_PRECEDENCE_TOKEN_CHARS: usize = 64;

impl core::fmt::Display for PrecedenceParseError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "unrecognized precedence token: {}", self.echoed)
    }
}

impl TryFrom<&str> for Precedence {
    type Error = PrecedenceParseError;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        match value {
            "routine" => Ok(Precedence::Routine),
            "priority" => Ok(Precedence::Priority),
            "emergency" => Ok(Precedence::Emergency),
            _ => Err(PrecedenceParseError {
                // Filter control/bidi characters before bounding length, so a
                // malicious token can never reorder or hide bytes in the echo.
                echoed: value
                    .chars()
                    .filter(|c| !c.is_control() && !is_bidi_control(*c))
                    .take(MAX_ECHOED_PRECEDENCE_TOKEN_CHARS)
                    .collect(),
            }),
        }
    }
}

/// The upper bound on a declared traffic count — the number
/// of message-traffic pieces a station is carrying. Generous for any real net
/// while refusing an absurd value.
pub const MAX_TRAFFIC_COUNT: u16 = 999;

/// A validated, bounded, non-zero traffic count — an optional
/// edit-only field on `checkin.updated`. `None` (rather than `Some(0)`) means no
/// traffic declared, following the [`SignalReport`] optional-newtype idiom.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TrafficCount(u16);

impl TrafficCount {
    /// The stored count.
    pub fn get(self) -> u16 {
        self.0
    }
}

/// A traffic count outside the representable range.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TrafficCountError {
    /// A negative count — nonsensical for a message-piece tally.
    Negative,
    /// Above [`MAX_TRAFFIC_COUNT`].
    TooLarge,
}

impl core::fmt::Display for TrafficCountError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str("traffic count must be a whole number between 0 and 999")
    }
}

/// Parses an optional traffic count from the wire number. Traffic
/// is inherently a count, so it crosses the wire as a JSON number,
/// not a string; `None` (absent) and an explicit `0` both mean "no traffic
/// declared" and yield `None` — never `Some(0)`. A negative or over-bound value
/// is rejected at the boundary.
pub fn parse_traffic_count(value: Option<i64>) -> Result<Option<TrafficCount>, TrafficCountError> {
    match value {
        None | Some(0) => Ok(None),
        Some(n) if n < 0 => Err(TrafficCountError::Negative),
        Some(n) if n > MAX_TRAFFIC_COUNT as i64 => Err(TrafficCountError::TooLarge),
        // In range 1..=MAX_TRAFFIC_COUNT, so the u16 conversion cannot truncate.
        Some(n) => Ok(Some(TrafficCount(n as u16))),
    }
}

/// The upper bound on a stored note, in characters. Generous for
/// round commentary — a per-station or net-level note is longer than a name or
/// location, but still bounded: free prose beyond this is refused. Reuses the
/// shipped free-text guard for the control/bidi rejection every text field shares.
pub const MAX_NOTE_CHARS: usize = 2000;

/// A validated, bounded note string (trimmed, control/bidi-free, length-bounded).
/// Shared by BOTH the per-station note (an additive field on `checkin.updated`)
/// and the net-level note (the `session.note-set` kind) — they have identical
/// validation needs. Carries the operator's commentary
/// verbatim, unicode-welcome (a note is prose, not a callsign).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Note(String);

impl Note {
    /// The stored note as a string slice.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl core::fmt::Display for Note {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(&self.0)
    }
}

/// Parses an optional note: normalizes CRLF/lone-CR line endings to `\n` (so a
/// pasted note round-trips the same regardless of the client's line-ending
/// convention), trims, rejects control characters EXCEPT the embedded newline
/// a multi-line note legitimately contains, rejects bidi-control characters,
/// and bounds to [`MAX_NOTE_CHARS`]. A blank-after-trim value is `None` (the
/// optional-text idiom, like [`parse_name`]/[`parse_location`]), NOT an empty
/// string.
///
/// A run of three or more consecutive newlines collapses to one blank line.
/// That is inherited from the shared prose guard **deliberately**, not by
/// accident of code sharing: a note is the same kind of pasted
/// prose a description is, and the two fields are better off agreeing. Because
/// this guard also runs on the READ path — `session_events.rs` re-parses
/// `checkin.updated.notes` and `session.note-set.note` on every decode — notes
/// already in the event log are projected collapsed from that point on. The
/// stored bytes are untouched, so the log stays append-only and no backfill is
/// needed; the collapse only ever removes newlines, so it cannot turn a note
/// that decoded before into a decode error.
///
/// Delegates to [`parse_bounded_multiline_text`], the prose guard, NOT to
/// [`parse_bounded_text`], the single-line one. The distinction is the shape
/// of the field:
/// the other bounded-text values here (name/location/report) are single-line,
/// where rejecting every control character is correct, but a note is prose
/// captured in a multi-line textarea
/// (`CheckInDetailModal.tsx`/`NetNotePanel.tsx`) — rejecting `\n` would
/// refuse the very round commentary the field exists to capture.
pub fn parse_note(input: &str) -> Result<Option<Note>, ProfileError> {
    let trimmed = parse_bounded_multiline_text(input, MAX_NOTE_CHARS)?;
    if trimmed.is_empty() {
        return Ok(None);
    }
    Ok(Some(Note(trimmed)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn check_in_source_round_trips_through_its_kebab_wire_form_and_defaults_to_staff() {
        // The staff-vs-self provenance discriminator. Wire tokens
        // are `staff`/`self`; Default is Staff so a historical/omitted `source`
        // folds to `staff` (additive-compat — no self entry predates the field).
        for source in [CheckInSource::Staff, CheckInSource::SelfService] {
            let token = source.as_str();
            assert_eq!(
                CheckInSource::try_from(token).expect("known token parses"),
                source
            );
        }
        assert_eq!(CheckInSource::Staff.as_str(), "staff");
        assert_eq!(CheckInSource::SelfService.as_str(), "self");
        assert_eq!(CheckInSource::default(), CheckInSource::Staff);
    }

    #[test]
    fn an_unknown_check_in_source_token_fails_to_parse() {
        assert!(CheckInSource::try_from("operator").is_err());
        assert!(CheckInSource::try_from("").is_err());
        assert!(CheckInSource::try_from("Self").is_err());
    }

    #[test]
    fn staying_status_round_trips_through_its_kebab_wire_form() {
        for status in [StayingStatus::StayingForComments, StayingStatus::InAndOut] {
            let token = status.as_str();
            assert_eq!(
                StayingStatus::try_from(token).expect("known token parses"),
                status
            );
        }
        assert_eq!(
            StayingStatus::StayingForComments.as_str(),
            "staying-for-comments"
        );
        assert_eq!(StayingStatus::InAndOut.as_str(), "in-and-out");
    }

    #[test]
    fn staying_status_defaults_to_in_and_out() {
        // The default is well-defined for both an omitted request field and a
        // historical field-less event fold.
        assert_eq!(StayingStatus::default(), StayingStatus::InAndOut);
    }

    #[test]
    fn an_unknown_staying_token_fails_to_parse() {
        assert!(StayingStatus::try_from("staying").is_err());
        assert!(StayingStatus::try_from("").is_err());
        assert!(StayingStatus::try_from("StayingForComments").is_err());
    }

    #[test]
    fn a_report_is_trimmed_and_preserved_verbatim() {
        let report = parse_signal_report("  599 ")
            .expect("valid report")
            .expect("non-blank yields Some");
        assert_eq!(report.as_str(), "599");
    }

    #[test]
    fn a_blank_or_whitespace_report_is_none_not_empty_string() {
        assert_eq!(parse_signal_report("").expect("blank ok"), None);
        assert_eq!(parse_signal_report("   ").expect("whitespace ok"), None);
    }

    #[test]
    fn a_report_over_the_bound_is_rejected() {
        // 17 chars post-trim exceeds MAX_SIGNAL_REPORT_CHARS (16).
        let too_long = "x".repeat(MAX_SIGNAL_REPORT_CHARS + 1);
        assert_eq!(
            parse_signal_report(&too_long),
            Err(ProfileError::TooLong {
                max_chars: MAX_SIGNAL_REPORT_CHARS
            })
        );
        // Exactly at the bound is accepted.
        let at_bound = "y".repeat(MAX_SIGNAL_REPORT_CHARS);
        assert_eq!(
            parse_signal_report(&at_bound)
                .expect("at-bound ok")
                .expect("non-blank")
                .as_str(),
            at_bound
        );
    }

    #[test]
    fn a_report_with_control_or_bidi_characters_is_rejected() {
        assert_eq!(
            parse_signal_report("5\u{0007}9"),
            Err(ProfileError::IllegalCharacter('\u{0007}'))
        );
        // A bidi override — the RTL-spoof class parse_bounded_text blocks.
        assert_eq!(
            parse_signal_report("59\u{202E}"),
            Err(ProfileError::IllegalCharacter('\u{202E}'))
        );
    }

    #[test]
    fn a_name_is_trimmed_bounded_and_unicode_welcome() {
        let name = parse_name("  José Ñ ").expect("valid").expect("non-blank");
        assert_eq!(name.as_str(), "José Ñ");
        assert_eq!(parse_name("").expect("blank ok"), None);
        assert_eq!(parse_name("   ").expect("whitespace ok"), None);
        assert_eq!(
            parse_name(&"x".repeat(MAX_NAME_CHARS + 1)),
            Err(ProfileError::TooLong {
                max_chars: MAX_NAME_CHARS
            })
        );
        assert_eq!(
            parse_name("Ma\u{0007}ria"),
            Err(ProfileError::IllegalCharacter('\u{0007}'))
        );
        // A bidi override is rejected (the RTL-spoof class).
        assert_eq!(
            parse_name("Ma\u{202E}ria"),
            Err(ProfileError::IllegalCharacter('\u{202E}'))
        );
    }

    #[test]
    fn a_location_is_trimmed_bounded_and_free_text() {
        let loc = parse_location(" Hartford, CT ")
            .expect("valid")
            .expect("non-blank");
        assert_eq!(loc.as_str(), "Hartford, CT");
        assert_eq!(parse_location("").expect("blank ok"), None);
        assert_eq!(
            parse_location(&"x".repeat(MAX_LOCATION_CHARS + 1)),
            Err(ProfileError::TooLong {
                max_chars: MAX_LOCATION_CHARS
            })
        );
        assert_eq!(
            parse_location("Hart\u{0000}ford"),
            Err(ProfileError::IllegalCharacter('\u{0000}'))
        );
    }

    #[test]
    fn a_free_form_qualitative_report_is_accepted() {
        // FM full-quieting-style report fits the bound and is kept verbatim.
        let report = parse_signal_report("full quieting")
            .expect("valid")
            .expect("non-blank");
        assert_eq!(report.as_str(), "full quieting");
    }

    #[test]
    fn precedence_round_trips_through_its_kebab_wire_form() {
        for precedence in [
            Precedence::Routine,
            Precedence::Priority,
            Precedence::Emergency,
        ] {
            let token = precedence.as_str();
            assert_eq!(
                Precedence::try_from(token).expect("known token parses"),
                precedence
            );
        }
        assert_eq!(Precedence::Routine.as_str(), "routine");
        assert_eq!(Precedence::Priority.as_str(), "priority");
        assert_eq!(Precedence::Emergency.as_str(), "emergency");
    }

    #[test]
    fn precedence_defaults_to_routine() {
        // The conservative baseline every entry carries from check-in.
        assert_eq!(Precedence::default(), Precedence::Routine);
    }

    #[test]
    fn an_unknown_precedence_token_fails_to_parse() {
        assert!(Precedence::try_from("urgent").is_err());
        assert!(Precedence::try_from("").is_err());
        assert!(Precedence::try_from("Routine").is_err());
    }

    #[test]
    fn an_oversized_or_hostile_precedence_token_is_sanitized_and_bounded_in_the_echo() {
        // Copy StayingParseError's control/bidi-sanitized, length-bounded echo.
        let hostile = format!("\u{202E}{}", "x".repeat(10_000));
        let err = Precedence::try_from(hostile.as_str()).expect_err("not a precedence");
        let rendered = err.to_string();
        assert!(!rendered.contains('\u{202E}'), "bidi control filtered out");
        // The echoed token is bounded well under the raw input length.
        assert!(rendered.len() < hostile.len());
    }

    #[test]
    fn precedence_sort_rank_orders_emergency_before_priority_before_routine() {
        // Highest precedence worked first: Emergency = 0, Priority = 1, Routine = 2.
        assert!(Precedence::Emergency.sort_rank() < Precedence::Priority.sort_rank());
        assert!(Precedence::Priority.sort_rank() < Precedence::Routine.sort_rank());
    }

    #[test]
    fn a_note_is_trimmed_bounded_and_blank_folds_to_none() {
        // A round/station note: trimmed, kept verbatim, unicode-welcome, and a
        // blank-after-trim value is None (the optional-text idiom), NOT "".
        let note = parse_note("  QSY to 20m per net control  ")
            .expect("valid")
            .expect("non-blank");
        assert_eq!(note.as_str(), "QSY to 20m per net control");
        assert_eq!(parse_note("").expect("blank ok"), None);
        assert_eq!(parse_note("   ").expect("whitespace ok"), None);
    }

    #[test]
    fn a_note_over_the_generous_bound_is_rejected_but_prose_length_is_allowed() {
        // Notes are longer than a name/location but still bounded — refuse free
        // prose beyond MAX_NOTE_CHARS while accepting a full round comment.
        let at_bound = "x".repeat(MAX_NOTE_CHARS);
        assert_eq!(
            parse_note(&at_bound)
                .expect("at-bound ok")
                .expect("non-blank")
                .as_str()
                .chars()
                .count(),
            MAX_NOTE_CHARS
        );
        assert_eq!(
            parse_note(&"y".repeat(MAX_NOTE_CHARS + 1)),
            Err(ProfileError::TooLong {
                max_chars: MAX_NOTE_CHARS
            })
        );
    }

    #[test]
    fn a_note_with_control_or_bidi_characters_is_rejected() {
        assert_eq!(
            parse_note("bad\u{0007}note"),
            Err(ProfileError::IllegalCharacter('\u{0007}'))
        );
        // A bidi override — the RTL-spoof class the prose guard
        // parse_bounded_multiline_text blocks. Widening this field to accept
        // `\n` did not disarm that half of the guard.
        assert_eq!(
            parse_note("note\u{202E}flip"),
            Err(ProfileError::IllegalCharacter('\u{202E}'))
        );
    }

    #[test]
    fn a_multi_line_note_is_accepted_and_crlf_normalizes_to_lf() {
        // Round commentary is typed into a multi-line textarea
        // (CheckInDetailModal.tsx / NetNotePanel.tsx) — a newline is NOT a
        // stray control character to refuse, it is the field's own affordance.
        // Review finding: delegating to parse_bounded_text's blanket
        // `is_control()` reject (which covers `\n`/`\r`/`\t`, Cc category)
        // would refuse every multi-line note the UI is built to capture.
        let note = parse_note("QSY to 20m\r\nStandby for traffic")
            .expect("valid")
            .expect("non-blank");
        // A pasted Windows-style CRLF note normalizes to a plain `\n` so a
        // note round-trips identically regardless of the client's line
        // ending convention.
        assert_eq!(note.as_str(), "QSY to 20m\nStandby for traffic");
    }

    #[test]
    fn a_lone_cr_in_a_note_also_normalizes_to_lf() {
        let note = parse_note("line one\rline two")
            .expect("valid")
            .expect("non-blank");
        assert_eq!(note.as_str(), "line one\nline two");
    }

    #[test]
    fn a_note_collapses_blank_line_runs_the_same_way_a_description_does() {
        // A deliberate behaviour change to an already-shipped field, not a
        // side effect inherited quietly from the
        // description fix: `parse_note` delegates to the shared prose guard, so
        // the collapse reaches per-station check-in notes and the net-level
        // session note as well, and that is the intended blast radius rather
        // than an accident of code sharing.
        //
        // It applies on the READ path too. `parse_note` is called again when a
        // `checkin.updated` or `session.note-set` event is decoded out of the
        // Postgres log (`pg/session_events.rs:483`, `:522`), so notes already
        // persisted with a blank-line run decode collapsed from now on. The
        // stored bytes are untouched — the log stays append-only — but the
        // value the fold projects changes, which is why notes need no backfill.
        let collapsed = parse_note("QSY to 20m\n\n\n\nStandby for traffic")
            .expect("valid")
            .expect("non-blank");
        assert_eq!(collapsed.as_str(), "QSY to 20m\n\nStandby for traffic");

        // The author's own paragraph structure is still theirs.
        let kept = parse_note("QSY to 20m\n\nStandby for traffic")
            .expect("valid")
            .expect("non-blank");
        assert_eq!(kept.as_str(), "QSY to 20m\n\nStandby for traffic");

        // And a note over MAX_NOTE_CHARS only because of a newline run is
        // accepted, the same ordering the description gets.
        let over_bound_only_by_newlines = format!("a{}b", "\n".repeat(MAX_NOTE_CHARS));
        let bounded = parse_note(&over_bound_only_by_newlines)
            .expect("collapse precedes the bound")
            .expect("non-blank");
        assert_eq!(bounded.as_str(), "a\n\nb");
    }

    #[test]
    fn a_traffic_count_is_bounded_with_blank_and_zero_folding_to_none() {
        // Absent (None) and an explicit zero both mean "no traffic declared".
        assert_eq!(parse_traffic_count(None).expect("absent ok"), None);
        assert_eq!(parse_traffic_count(Some(0)).expect("zero ok"), None);
        // A real count is preserved.
        assert_eq!(
            parse_traffic_count(Some(3))
                .expect("valid")
                .expect("non-zero")
                .get(),
            3
        );
        // At the bound is accepted; over the bound is rejected.
        assert_eq!(
            parse_traffic_count(Some(MAX_TRAFFIC_COUNT as i64))
                .expect("at-bound ok")
                .expect("non-zero")
                .get(),
            MAX_TRAFFIC_COUNT
        );
        assert!(parse_traffic_count(Some(MAX_TRAFFIC_COUNT as i64 + 1)).is_err());
        // A negative count is nonsensical — rejected, never silently coerced.
        assert!(parse_traffic_count(Some(-1)).is_err());
    }
    #[test]
    fn widening_a_prose_field_leaves_the_single_line_name_guard_untouched() {
        // Confinement, counter-direction: `parse_name` reuses the shared
        // single-line bounded-text guard. Widening the net definition's
        // `description` must not reach it; a newline in a per-check-in name
        // must stay a rejection. Green before and after — it fails only if the
        // widening lands in `parse_bounded_text` itself.
        assert_eq!(
            parse_name("Jane\nDoe"),
            Err(ProfileError::IllegalCharacter('\n')),
            "a per-check-in name is single-line"
        );
    }
}
