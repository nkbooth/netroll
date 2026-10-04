// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
//! Net-definition field validation: raw inbound values in, a
//! [`NetDefinitionFields`] the adapter trusts out, and a field-level error
//! whose `Display` becomes the problem+json `detail`. Frequencies are
//! hand-rolled decimal-MHz to integer-Hz, never `f64` — the ham-frequency
//! rounding trap — and belong to a connection, not to the definition.

use thiserror::Error;

use crate::profile::{
    Grid, GridError, ProfileError, parse_bounded_multiline_text, parse_bounded_text, parse_grid,
};

use super::enums::{NetCategory, NetType, Visibility};

/// Hz resolution of a stored frequency: six decimal-MHz places (1 Hz).
const HZ_FRACTIONAL_DIGITS: usize = 6;
/// Hz per MHz.
const HZ_PER_MHZ: i64 = 1_000_000;

/// Lower amateur-frequency bound (135.7 kHz, the 2200m band).
const MIN_FREQUENCY_HZ: i64 = 135_700;
/// Upper amateur-frequency bound (250 GHz).
const MAX_FREQUENCY_HZ: i64 = 250_000_000_000;
/// Repeater offset magnitude bound (±100 MHz).
const MAX_OFFSET_HZ: i64 = 100_000_000;

/// Expected-duration bounds, in minutes.
const MIN_DURATION_MINUTES: i32 = 1;
const MAX_DURATION_MINUTES: i32 = 1440;

/// The net title's length bound, in characters, post-trim — the length every
/// stored `definition_snapshot.title` was validated against on write.
///
/// `pub` because the Discord announcement builder
/// (`netroll_app`'s `DISCORD_TITLE_CHARS`) DERIVES its own title cap from this
/// number, and a `const _: () = assert!(…)` there now makes that derivation a
/// build-time fact rather than a comment claiming a link between two literals.
pub const MAX_TITLE_CHARS: usize = 120;

// The remaining text-field length bounds (characters, post-trim). The
// single-line connection properties used to share a bound declared here, because
// three of them were mirrored into definition columns this module guarded; those
// columns are retired and the bound now lives with the connections
// (`net::connection::MAX_CONNECTION_TEXT_CHARS`).
const MAX_DESCRIPTION_CHARS: usize = 2000;
const MAX_GEOGRAPHY_CHARS: usize = 80;

/// Why a frequency or offset was rejected. Each variant's message is surfaced
/// (field-prefixed) as the problem+json `detail`.
///
/// Two callers, two fault sets. The decimal-MHz STRING parsers
/// ([`parse_frequency_hz`], [`parse_offset_hz`] — the session QSY command's
/// `operatingFrequency`) can raise every variant. The integer-Hz CHECKS
/// ([`check_frequency_hz`], [`check_offset_hz`] — a connection's
/// `plannedFrequencyHz`/`repeaterOffsetHz`, typed on the wire)
/// can raise only `Negative` and `OutOfRange`: there is no text to be empty,
/// non-numeric or too precise.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum FrequencyError {
    /// Input is empty or whitespace-only.
    #[error("must not be empty")]
    Empty,
    /// Input is not a decimal number.
    #[error("must be a decimal number of MHz")]
    NotNumeric,
    /// A frequency (unsigned) was given as negative.
    #[error("must not be negative")]
    Negative,
    /// Value is outside the admissible span.
    #[error("is outside the amateur frequency range")]
    OutOfRange,
    /// More fractional digits than 1 Hz resolution allows.
    #[error("is finer than 1 Hz resolution")]
    TooPrecise,
}

/// Why an expected-duration string was rejected.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum DurationError {
    /// Input is not a whole number of minutes.
    #[error("must be a whole number of minutes")]
    NotNumeric,
    /// Input is outside `1..=1440` minutes.
    #[error("must be between 1 and 1440 minutes")]
    OutOfRange,
}

/// Why a submitted net definition was rejected — one variant per field and
/// reason. `Display` is `"<field>: <reason>"` so the HTTP `detail` names the
/// offending field. Distinct fields yield distinct text.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum NetDefinitionError {
    /// Title was absent or blank (required).
    #[error("title: is required")]
    TitleRequired,
    /// Title failed the bounded-text guard.
    #[error("title: {0}")]
    Title(ProfileError),
    /// Description failed the bounded-text guard.
    #[error("description: {0}")]
    Description(ProfileError),
    /// Country failed the bounded-text guard.
    #[error("country: {0}")]
    Country(ProfileError),
    /// State failed the bounded-text guard.
    #[error("state: {0}")]
    State(ProfileError),
    /// Grid failed the Maidenhead grammar.
    #[error("grid: {0}")]
    Grid(GridError),
    /// Net category was absent (required).
    #[error("net category: is required")]
    NetCategoryRequired,
    /// Net category token is not a known category.
    #[error("net category: is not a recognized category")]
    NetCategoryUnknown,
    /// Net type was absent (required).
    #[error("net type: is required")]
    NetTypeRequired,
    /// Net type token is not a known type.
    #[error("net type: is not a recognized type")]
    NetTypeUnknown,
    /// Expected duration failed parsing/bounds.
    #[error("expected duration: {0}")]
    Duration(DurationError),
    /// Visibility token is not a known visibility.
    #[error("visibility: is not a recognized visibility")]
    VisibilityUnknown,
}

/// The unvalidated inbound net definition, every field as it arrives on the
/// wire (`Option<String>`). `parse_net_definition_fields` turns it into the
/// typed [`NetDefinitionFields`] the adapter trusts.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RawNetDefinition {
    /// Net title (required).
    pub title: Option<String>,
    /// Free-text description (optional).
    pub description: Option<String>,
    /// Country free text (optional).
    pub country: Option<String>,
    /// State/province free text (optional).
    pub state: Option<String>,
    /// Maidenhead grid (optional).
    pub grid: Option<String>,
    /// Net category token (required).
    pub net_category: Option<String>,
    /// Net type token (required).
    pub net_type: Option<String>,
    /// Expected duration in minutes (optional).
    pub expected_duration: Option<String>,
    /// Visibility token (optional; absent/blank defaults to `listed`).
    pub visibility: Option<String>,
}

/// The validated write shape the storage adapter trusts for format (the same
/// "adapter trusts its caller" contract as
/// [`crate::model::account::ProfileFields`]). Every field is already parsed:
/// enums as their typed variants, grid canonical. The SCALAR fields only — a
/// definition's connections are parsed by `net::connection::parse_connection_set`
/// and written through their own path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NetDefinitionFields {
    /// Trimmed title.
    pub title: String,
    /// Trimmed description, or `None`.
    pub description: Option<String>,
    /// Country, or `None`.
    pub country: Option<String>,
    /// State/province, or `None`.
    pub state: Option<String>,
    /// Canonical Maidenhead grid, or `None`.
    pub grid: Option<Grid>,
    /// Net category.
    pub net_category: NetCategory,
    /// Net type.
    pub net_type: NetType,
    /// Expected duration in minutes, or `None`.
    pub expected_duration_minutes: Option<i32>,
    /// Visibility (defaults to `Listed` when unspecified).
    pub visibility: Visibility,
}

/// Parses a decimal-MHz string to exact integer Hz. `allow_sign` permits a
/// leading `+`/`-` (offsets); without it a `-` is a [`FrequencyError::Negative`].
fn decimal_mhz_to_hz(input: &str, allow_sign: bool) -> Result<i64, FrequencyError> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return Err(FrequencyError::Empty);
    }

    let (negative, digits) = match trimmed.strip_prefix('-') {
        Some(rest) if allow_sign => (true, rest),
        Some(_) => return Err(FrequencyError::Negative),
        None => (false, trimmed.strip_prefix('+').unwrap_or(trimmed)),
    };

    let (int_part, frac_part) = match digits.split_once('.') {
        // A second '.' in the remainder means more than one decimal point.
        Some((_, f)) if f.contains('.') => return Err(FrequencyError::NotNumeric),
        Some((i, f)) => (i, f),
        None => (digits, ""),
    };
    // At least one digit somewhere; both parts must be pure ASCII digits.
    if int_part.is_empty() && frac_part.is_empty() {
        return Err(FrequencyError::NotNumeric);
    }
    if !int_part.chars().all(|c| c.is_ascii_digit())
        || !frac_part.chars().all(|c| c.is_ascii_digit())
    {
        return Err(FrequencyError::NotNumeric);
    }
    if frac_part.len() > HZ_FRACTIONAL_DIGITS {
        return Err(FrequencyError::TooPrecise);
    }

    let whole: i64 = if int_part.is_empty() {
        0
    } else {
        int_part.parse().map_err(|_| FrequencyError::OutOfRange)?
    };
    // Right-pad the fractional part to 6 digits so it reads as whole Hz.
    let mut frac_hz_str = frac_part.to_owned();
    while frac_hz_str.len() < HZ_FRACTIONAL_DIGITS {
        frac_hz_str.push('0');
    }
    let frac_hz: i64 = frac_hz_str
        .parse()
        .map_err(|_| FrequencyError::OutOfRange)?;

    let magnitude = whole
        .checked_mul(HZ_PER_MHZ)
        .and_then(|hz| hz.checked_add(frac_hz))
        .ok_or(FrequencyError::OutOfRange)?;
    Ok(if negative { -magnitude } else { magnitude })
}

/// Decimal-MHz digits in the kHz group — the `.200` of `7.200`.
const KHZ_GROUP_DIGITS: usize = 3;

/// Decimal-MHz digits in the sub-kHz group — the `.1250` of `448.670.1250`,
/// i.e. the remaining Hz expressed in tenths of a Hz.
const SUB_KHZ_GROUP_DIGITS: usize = 4;

/// Formats integer Hz the way a frequency is written and read on the air:
/// always `X.XXX` (`7.200`, `14.275`, `146.520`), extended to `X.XXX.XXXX`
/// (`448.670.1250`) when the frequency carries precision below 1 kHz.
///
/// **Integer arithmetic only, never `f64`.** The obvious one-liner —
/// `format!("{:.3} MHz", hz as f64 / 1e6)` — rounds a 12.5 kHz-raster VHF/UHF
/// frequency to a DIFFERENT, WRONG frequency: `145_512_500` prints
/// `145.513 MHz`, and that number then reaches the CSV, the published webhook
/// label, the summary email and the check-in history. It also makes two ways in
/// 125 Hz apart render identically, which is the label-uniqueness failure
/// `connection_label` exists to prevent. A review found exactly
/// that, in the only `as f64` the backend had.
///
/// DISPLAY ONLY, and deliberately NOT the transport format: the second dot makes
/// it unparseable by [`parse_frequency_hz`]. Anything that has to round-trip
/// uses the ADIF `<FREQ>` renderer instead.
///
/// The hand-written twin of the frontend's `formatFrequencyMhz`
/// (`netsApi.ts`); the two are not shared and a disagreement between them is a
/// defect in whichever was edited alone. Both sides pin the same sub-kHz
/// literal.
pub fn format_frequency_mhz(hz: i64) -> String {
    // Sign is tracked separately from the whole part because a magnitude under
    // 1 MHz (the standard -0.600 MHz repeater offset) truncates to zero, which
    // would silently flip the offset's direction.
    let sign = if hz < 0 { "-" } else { "" };
    let magnitude = hz.unsigned_abs();
    let hz_per_mhz = HZ_PER_MHZ.unsigned_abs();
    let whole = magnitude / hz_per_mhz;
    let within_mhz = magnitude % hz_per_mhz;
    let khz = within_mhz / 1_000;
    let sub_khz_hz = within_mhz % 1_000;
    if sub_khz_hz == 0 {
        return format!("{sign}{whole}.{khz:0>width$}", width = KHZ_GROUP_DIGITS);
    }
    // Tenths of a Hz: 125 Hz reads as "1250", giving 448.670.1250.
    let sub_khz = sub_khz_hz * 10;
    format!(
        "{sign}{whole}.{khz:0>khz_width$}.{sub_khz:0>sub_width$}",
        khz_width = KHZ_GROUP_DIGITS,
        sub_width = SUB_KHZ_GROUP_DIGITS,
    )
}

/// Parses a planned operating frequency (unsigned decimal MHz) to exact Hz,
/// bounded to the amateur span. Store as `i64` — never a float.
pub fn parse_frequency_hz(input: &str) -> Result<i64, FrequencyError> {
    let hz = decimal_mhz_to_hz(input, false)?;
    check_frequency_hz(hz)
}

/// Parses a signed repeater offset (decimal MHz) to exact Hz, bounded to
/// ±100 MHz.
pub fn parse_offset_hz(input: &str) -> Result<i64, FrequencyError> {
    let hz = decimal_mhz_to_hz(input, true)?;
    check_offset_hz(hz)
}

/// Range-checks an exact-Hz planned frequency that arrived already typed —
/// the connection write body's `plannedFrequencyHz` — and hands
/// it back unchanged when it is admissible.
///
/// The range half of [`parse_frequency_hz`], shared with it so the string and
/// the integer paths cannot disagree about the amateur span. A negative value
/// is `Negative` before it is `OutOfRange`, keeping the friendlier of the two
/// messages where both would apply.
pub fn check_frequency_hz(hz: i64) -> Result<i64, FrequencyError> {
    if hz < 0 {
        return Err(FrequencyError::Negative);
    }
    if !(MIN_FREQUENCY_HZ..=MAX_FREQUENCY_HZ).contains(&hz) {
        return Err(FrequencyError::OutOfRange);
    }
    Ok(hz)
}

/// Range-checks an exact-Hz signed repeater offset that arrived already typed
/// — the connection write body's `repeaterOffsetHz` — bounded to
/// ±100 MHz. The range half of [`parse_offset_hz`], shared with it.
pub fn check_offset_hz(hz: i64) -> Result<i64, FrequencyError> {
    // `unsigned_abs`, not `abs`: the string parser could never build
    // `i64::MIN`, but a JSON integer can be exactly that, and `i64::MIN.abs()`
    // overflows.
    if hz.unsigned_abs() > MAX_OFFSET_HZ.unsigned_abs() {
        return Err(FrequencyError::OutOfRange);
    }
    Ok(hz)
}

/// Parses an expected duration as a positive whole number of minutes,
/// bounded to `1..=1440`.
pub fn parse_expected_duration_minutes(input: &str) -> Result<i32, DurationError> {
    let trimmed = input.trim();
    let minutes: i32 = trimmed.parse().map_err(|_| DurationError::NotNumeric)?;
    if !(MIN_DURATION_MINUTES..=MAX_DURATION_MINUTES).contains(&minutes) {
        return Err(DurationError::OutOfRange);
    }
    Ok(minutes)
}

/// `None`/blank-after-trim → `None`; otherwise parse the text and wrap any
/// error via `wrap`. Mirrors `parse_profile_field`'s clear-vs-validate rule.
fn optional_text(
    submitted: Option<&str>,
    max_chars: usize,
    wrap: impl Fn(ProfileError) -> NetDefinitionError,
) -> Result<Option<String>, NetDefinitionError> {
    match submitted {
        None => Ok(None),
        Some(s) if s.trim().is_empty() => Ok(None),
        Some(s) => parse_bounded_text(s, max_chars).map(Some).map_err(wrap),
    }
}

/// As `optional_text`, but for a field whose content is prose typed into a
/// textarea: line breaks are the field's own affordance, so it parses through
/// the multi-line guard instead of the single-line one. Every other optional
/// text field on a net definition stays single-line. The prose guard keeps the
/// author's paragraphs and folds any run past one blank line, so
/// "the field's own affordance" means structure, not unbounded empty page.
fn optional_multiline_text(
    submitted: Option<&str>,
    max_chars: usize,
    wrap: impl Fn(ProfileError) -> NetDefinitionError,
) -> Result<Option<String>, NetDefinitionError> {
    match submitted {
        None => Ok(None),
        // Checked on the RAW string: `\r` and `\n` are both ASCII whitespace,
        // so a value of nothing but line breaks folds to `None` exactly as it
        // did before this field went multi-line.
        Some(s) if s.trim().is_empty() => Ok(None),
        Some(s) => parse_bounded_multiline_text(s, max_chars)
            .map(Some)
            .map_err(wrap),
    }
}

/// Validates every field of a [`RawNetDefinition`], failing fast on the FIRST
/// invalid field so no row is ever written for a bad request. The
/// validation order is deterministic and documented: title, description, grid,
/// country, state, then category/type/duration, then visibility.
pub fn parse_net_definition_fields(
    raw: RawNetDefinition,
) -> Result<NetDefinitionFields, NetDefinitionError> {
    // Title (required, non-blank).
    let title = match raw.title.as_deref() {
        None => return Err(NetDefinitionError::TitleRequired),
        Some(s) if s.trim().is_empty() => return Err(NetDefinitionError::TitleRequired),
        Some(s) => parse_bounded_text(s, MAX_TITLE_CHARS).map_err(NetDefinitionError::Title)?,
    };

    let description = optional_multiline_text(
        raw.description.as_deref(),
        MAX_DESCRIPTION_CHARS,
        NetDefinitionError::Description,
    )?;

    let grid = match raw.grid.as_deref() {
        None => None,
        Some(s) if s.trim().is_empty() => None,
        Some(s) => Some(parse_grid(s).map_err(NetDefinitionError::Grid)?),
    };

    let country = optional_text(
        raw.country.as_deref(),
        MAX_GEOGRAPHY_CHARS,
        NetDefinitionError::Country,
    )?;
    let state = optional_text(
        raw.state.as_deref(),
        MAX_GEOGRAPHY_CHARS,
        NetDefinitionError::State,
    )?;

    // Net category (required enum).
    let net_category = match raw.net_category.as_deref() {
        None => return Err(NetDefinitionError::NetCategoryRequired),
        Some(s) if s.trim().is_empty() => return Err(NetDefinitionError::NetCategoryRequired),
        Some(s) => NetCategory::try_from(s).map_err(|()| NetDefinitionError::NetCategoryUnknown)?,
    };

    // Net type (required enum).
    let net_type = match raw.net_type.as_deref() {
        None => return Err(NetDefinitionError::NetTypeRequired),
        Some(s) if s.trim().is_empty() => return Err(NetDefinitionError::NetTypeRequired),
        Some(s) => NetType::try_from(s).map_err(|()| NetDefinitionError::NetTypeUnknown)?,
    };

    let expected_duration_minutes = match raw.expected_duration.as_deref() {
        None => None,
        Some(s) if s.trim().is_empty() => None,
        Some(s) => Some(parse_expected_duration_minutes(s).map_err(NetDefinitionError::Duration)?),
    };

    // Absent or blank-after-trim defaults to Listed; an
    // unknown token is a field-level rejection. Last in the fail-fast order.
    let visibility = match raw.visibility.as_deref() {
        None => Visibility::Listed,
        Some(s) if s.trim().is_empty() => Visibility::Listed,
        Some(s) => Visibility::try_from(s).map_err(|()| NetDefinitionError::VisibilityUnknown)?,
    };

    Ok(NetDefinitionFields {
        title,
        description,
        country,
        state,
        grid,
        net_category,
        net_type,
        expected_duration_minutes,
        visibility,
    })
}

/// Which editable fields a submitted [`NetDefinitionFields`] would change on
/// `existing`, by NAME — empty when the edit is a no-op.
///
/// Two jobs, both for the audit trail. First, the gate: a full-replace PUT
/// arrives on every save, including one where the operator changed nothing, and
/// recording those would bury the real edits under noise (the same
/// discrimination `DisableOutcome`/`newly_revoked` already make elsewhere).
/// Second, the content: "someone saved this net 40 times" is not an answer to
/// "who acted on this net?" — the field names make each row say what actually
/// moved.
///
/// Returns NAMES ONLY, never values. `audit_log.metadata` is an open `jsonb`
/// whose PII-free property is caller discipline rather than a type constraint,
/// and a title or description is user-authored free text that must not land
/// there.
///
/// `link_token`, `definition_version`, `owner_account_ids`, and the timestamps
/// are excluded: they are not editable through this surface (ownership changes
/// have their own endpoints and their own audit events). So is `connections`:
/// the connection list has its own endpoint, which audits `["connections"]` on
/// its own, and the scalar shape compared here cannot carry one.
///
/// Pure: no I/O, no clock.
pub fn changed_fields(
    existing: &crate::net::NetDefinition,
    submitted: &NetDefinitionFields,
) -> Vec<&'static str> {
    let mut changed = Vec::new();
    let mut note = |name: &'static str, differs: bool| {
        if differs {
            changed.push(name);
        }
    };

    note("title", existing.title != submitted.title);
    note("description", existing.description != submitted.description);
    note("country", existing.country != submitted.country);
    note("state", existing.state != submitted.state);
    note(
        "grid",
        existing.grid.as_deref() != submitted.grid.as_ref().map(|g| g.as_str()),
    );
    note(
        "netCategory",
        existing.net_category != submitted.net_category,
    );
    note("netType", existing.net_type != submitted.net_type);
    note(
        "expectedDurationMinutes",
        existing.expected_duration_minutes != submitted.expected_duration_minutes,
    );
    note("visibility", existing.visibility != submitted.visibility);

    changed
}

#[cfg(test)]
mod tests {
    use super::*;

    fn valid_raw() -> RawNetDefinition {
        RawNetDefinition {
            title: Some("Sunday Traffic Net".to_owned()),
            net_category: Some("traffic".to_owned()),
            net_type: Some("open".to_owned()),
            ..Default::default()
        }
    }

    /// A stored definition matching `valid_raw()`'s parsed fields exactly.
    fn definition_matching(fields: &NetDefinitionFields) -> crate::net::NetDefinition {
        crate::net::NetDefinition {
            id: uuid::Uuid::nil(),
            definition_version: 1,
            title: fields.title.clone(),
            description: fields.description.clone(),
            country: fields.country.clone(),
            state: fields.state.clone(),
            grid: fields.grid.as_ref().map(|g| g.as_str().to_owned()),
            connections: crate::net::connection::NetConnectionSet::new(vec![
                crate::net::connection::NetConnection {
                    id: uuid::Uuid::nil(),
                    position: 0,
                    kind: crate::net::connection::NetConnectionKind::Hf {
                        planned_frequency_hz: 14_230_000,
                        band: crate::net::enums::Band::TwentyMeters,
                        mode: crate::net::enums::Mode::Ssb,
                    },
                },
            ])
            .expect("one connection is a valid set"),
            net_category: fields.net_category,
            net_type: fields.net_type,
            expected_duration_minutes: fields.expected_duration_minutes,
            visibility: fields.visibility,
            link_token: "tok".to_owned(),
            owner_account_ids: Vec::new(),
            created_at_millis: 0,
            updated_at_millis: 0,
            archived_at_millis: None,
        }
    }

    #[test]
    fn a_resubmitted_identical_edit_changes_nothing() {
        // The gate: a full-replace PUT arrives on every save. Auditing a no-op
        // save would bury real edits in noise.
        let fields = parse_net_definition_fields(valid_raw()).expect("valid");
        let existing = definition_matching(&fields);
        assert!(changed_fields(&existing, &fields).is_empty());
    }

    #[test]
    fn a_changed_field_is_reported_by_name() {
        let fields = parse_net_definition_fields(valid_raw()).expect("valid");
        let mut existing = definition_matching(&fields);
        existing.title = "A Different Net".to_owned();
        assert_eq!(changed_fields(&existing, &fields), vec!["title"]);
    }

    #[test]
    fn several_changed_fields_are_all_reported() {
        let fields = parse_net_definition_fields(valid_raw()).expect("valid");
        let mut existing = definition_matching(&fields);
        existing.title = "Other".to_owned();
        existing.visibility = match fields.visibility {
            Visibility::Listed => Visibility::Unlisted,
            Visibility::Unlisted => Visibility::Listed,
        };
        let changed = changed_fields(&existing, &fields);
        assert!(changed.contains(&"title"));
        assert!(changed.contains(&"visibility"));
    }

    #[test]
    fn clearing_an_optional_field_counts_as_a_change() {
        // Setting a value back to None is an edit an admin must be able to see.
        let mut raw = valid_raw();
        raw.description = Some("Weekly rag chew".to_owned());
        let with_description = parse_net_definition_fields(raw).expect("valid");
        let existing = definition_matching(&with_description);

        let cleared = parse_net_definition_fields(valid_raw()).expect("valid");
        assert_eq!(changed_fields(&existing, &cleared), vec!["description"]);
    }

    #[test]
    fn changed_fields_reports_names_never_values() {
        // `audit_log.metadata` is an open jsonb; a title or description is
        // user-authored free text that must never reach it.
        let mut raw = valid_raw();
        raw.title = Some("Secret Net Name".to_owned());
        let fields = parse_net_definition_fields(raw).expect("valid");
        let existing = definition_matching(&parse_net_definition_fields(valid_raw()).expect("v"));
        let changed = changed_fields(&existing, &fields);

        assert_eq!(changed, vec!["title"]);
        assert!(
            !changed.iter().any(|n| n.contains("Secret")),
            "no field VALUE appears in the report: {changed:?}"
        );
    }

    #[test]
    fn frequency_parses_decimal_mhz_to_exact_hz() {
        assert_eq!(parse_frequency_hz("14.230"), Ok(14_230_000));
        assert_eq!(parse_frequency_hz("146.520"), Ok(146_520_000));
        // Sub-kHz precision preserved — the FT8 vector.
        assert_eq!(parse_frequency_hz("14.074100"), Ok(14_074_100));
        assert_eq!(parse_frequency_hz(" 7.200 "), Ok(7_200_000));
        // Whole MHz with no fractional part.
        assert_eq!(parse_frequency_hz("50"), Ok(50_000_000));
    }

    #[test]
    fn a_frequency_renders_every_digit_it_carries_and_rounds_none_of_them() {
        // The defect this function exists to refuse: `hz as f64 / 1e6` to three
        // places renders 145_512_500 as "145.513", a frequency 500 Hz away from
        // the one stored. The 12.5 kHz VHF/UHF raster makes that ordinary data,
        // not an edge case, and `parse_frequency_hz` accepts six fractional
        // digits so it can be stored.
        assert_eq!(format_frequency_mhz(145_512_500), "145.512.5000");
        // Whole-kHz frequencies keep the shipped three-place rendering.
        assert_eq!(format_frequency_mhz(14_230_000), "14.230");
        assert_eq!(format_frequency_mhz(7_200_000), "7.200");
        assert_eq!(format_frequency_mhz(146_520_000), "146.520");
        // The sub-kHz group is tenths of a Hz, so 125 Hz reads "1250".
        assert_eq!(format_frequency_mhz(448_670_125), "448.670.1250");
        // A magnitude under 1 MHz keeps its sign (the -0.600 repeater offset).
        assert_eq!(format_frequency_mhz(-600_000), "-0.600");
    }

    #[test]
    fn two_frequencies_125_hz_apart_do_not_render_the_same_string() {
        // Label uniqueness: a net may list two ways in a raster step apart, and
        // a renderer that rounds makes the roster say the same thing about both.
        assert_ne!(
            format_frequency_mhz(145_512_100),
            format_frequency_mhz(145_512_300)
        );
    }

    #[test]
    fn frequency_rejects_empty_nonnumeric_negative_and_out_of_range() {
        assert_eq!(parse_frequency_hz(""), Err(FrequencyError::Empty));
        assert_eq!(parse_frequency_hz("   "), Err(FrequencyError::Empty));
        assert_eq!(parse_frequency_hz("abc"), Err(FrequencyError::NotNumeric));
        assert_eq!(
            parse_frequency_hz("14.2.3"),
            Err(FrequencyError::NotNumeric)
        );
        assert_eq!(parse_frequency_hz("-14.230"), Err(FrequencyError::Negative));
        // Below the 2200m floor (135_700 Hz) and above the 250 GHz ceiling.
        assert_eq!(parse_frequency_hz("0.1"), Err(FrequencyError::OutOfRange));
        assert_eq!(
            parse_frequency_hz("300000"),
            Err(FrequencyError::OutOfRange)
        );
        // The floor itself is admissible: 0.1357 MHz == 135_700 Hz.
        assert_eq!(parse_frequency_hz("0.135700"), Ok(135_700));
    }

    #[test]
    fn frequency_rejects_sub_hz_precision() {
        // Seven fractional digits is finer than 1 Hz — refuse rather than
        // silently truncate (the rounding trap).
        assert_eq!(
            parse_frequency_hz("14.0741001"),
            Err(FrequencyError::TooPrecise)
        );
    }

    #[test]
    fn an_integer_hz_frequency_is_range_checked_without_a_string_in_sight() {
        // The connection write body carries Hz integers, so the
        // range half of the parser is reachable on its own. A negative integer
        // is `Negative` (the friendlier message) before it is `OutOfRange`.
        assert_eq!(check_frequency_hz(14_230_000), Ok(14_230_000));
        assert_eq!(check_frequency_hz(MIN_FREQUENCY_HZ), Ok(MIN_FREQUENCY_HZ));
        assert_eq!(check_frequency_hz(MAX_FREQUENCY_HZ), Ok(MAX_FREQUENCY_HZ));
        assert_eq!(
            check_frequency_hz(MAX_FREQUENCY_HZ + 1),
            Err(FrequencyError::OutOfRange)
        );
        assert_eq!(check_frequency_hz(0), Err(FrequencyError::OutOfRange));
        assert_eq!(
            check_frequency_hz(-14_230_000),
            Err(FrequencyError::Negative)
        );
    }

    #[test]
    fn an_integer_hz_offset_is_bounded_and_the_extreme_integer_does_not_panic() {
        assert_eq!(check_offset_hz(-600_000), Ok(-600_000));
        assert_eq!(check_offset_hz(MAX_OFFSET_HZ), Ok(MAX_OFFSET_HZ));
        assert_eq!(check_offset_hz(-MAX_OFFSET_HZ), Ok(-MAX_OFFSET_HZ));
        assert_eq!(
            check_offset_hz(MAX_OFFSET_HZ + 1),
            Err(FrequencyError::OutOfRange)
        );
        // A JSON integer can be exactly `i64::MIN`, whose `abs()` overflows;
        // the string parser could never produce it, so this is new ground.
        assert_eq!(check_offset_hz(i64::MIN), Err(FrequencyError::OutOfRange));
    }

    #[test]
    fn offset_is_signed_and_bounded() {
        assert_eq!(parse_offset_hz("-0.600"), Ok(-600_000));
        assert_eq!(parse_offset_hz("+5"), Ok(5_000_000));
        assert_eq!(parse_offset_hz("0.6"), Ok(600_000));
        // Beyond ±100 MHz.
        assert_eq!(parse_offset_hz("200"), Err(FrequencyError::OutOfRange));
        assert_eq!(parse_offset_hz("-200"), Err(FrequencyError::OutOfRange));
        assert_eq!(parse_offset_hz(""), Err(FrequencyError::Empty));
    }

    #[test]
    fn duration_is_a_positive_bounded_integer() {
        assert_eq!(parse_expected_duration_minutes("90"), Ok(90));
        assert_eq!(parse_expected_duration_minutes("1"), Ok(1));
        assert_eq!(parse_expected_duration_minutes("1440"), Ok(1440));
        assert_eq!(
            parse_expected_duration_minutes("0"),
            Err(DurationError::OutOfRange)
        );
        assert_eq!(
            parse_expected_duration_minutes("1441"),
            Err(DurationError::OutOfRange)
        );
        assert_eq!(
            parse_expected_duration_minutes("90.5"),
            Err(DurationError::NotNumeric)
        );
        assert_eq!(
            parse_expected_duration_minutes("-5"),
            Err(DurationError::OutOfRange)
        );
    }

    #[test]
    fn distinct_field_failures_carry_distinct_detail_text() {
        // Two different fields failing produce two different `detail`
        // strings — the field name leads (the grid-error test pattern).
        let bad_grid = parse_net_definition_fields(RawNetDefinition {
            grid: Some("nope".to_owned()),
            ..valid_raw()
        })
        .expect_err("malformed grid rejects");
        let bad_type = parse_net_definition_fields(RawNetDefinition {
            net_type: Some("rollcall".to_owned()),
            ..valid_raw()
        })
        .expect_err("unknown net type rejects");
        assert_ne!(bad_grid.to_string(), bad_type.to_string());
        assert!(bad_grid.to_string().starts_with("grid:"));
        assert!(bad_type.to_string().starts_with("net type:"));
    }

    #[test]
    fn aggregate_validation_fails_fast_on_grid_before_net_type() {
        // A raw shape with BOTH a bad grid and a bad net type surfaces the grid
        // error — the fail-fast order is deterministic and documented.
        let err = parse_net_definition_fields(RawNetDefinition {
            grid: Some("nope".to_owned()),
            net_type: Some("rollcall".to_owned()),
            ..valid_raw()
        })
        .expect_err("both fields invalid");
        assert!(
            err.to_string().starts_with("grid:"),
            "grid must be validated before net type, got {err}"
        );
    }

    #[test]
    fn required_fields_missing_are_field_specific_errors() {
        assert_eq!(
            parse_net_definition_fields(RawNetDefinition {
                title: None,
                ..valid_raw()
            }),
            Err(NetDefinitionError::TitleRequired)
        );
        assert_eq!(
            parse_net_definition_fields(RawNetDefinition {
                title: Some("   ".to_owned()),
                ..valid_raw()
            }),
            Err(NetDefinitionError::TitleRequired)
        );
        assert_eq!(
            parse_net_definition_fields(RawNetDefinition {
                net_type: Some("rollcall".to_owned()),
                ..valid_raw()
            }),
            Err(NetDefinitionError::NetTypeUnknown)
        );
    }

    #[test]
    fn happy_path_produces_the_typed_write_shape() {
        let fields = parse_net_definition_fields(RawNetDefinition {
            title: Some("  Sunday Traffic Net ".to_owned()),
            description: Some("Weekly NTS traffic".to_owned()),
            country: Some("USA".to_owned()),
            state: Some("CT".to_owned()),
            grid: Some("fn31pr".to_owned()),
            net_category: Some("traffic".to_owned()),
            net_type: Some("roll-call".to_owned()),
            expected_duration: Some("90".to_owned()),
            visibility: Some("unlisted".to_owned()),
        })
        .expect("valid definition");

        assert_eq!(fields.title, "Sunday Traffic Net", "trimmed");
        assert_eq!(fields.visibility, Visibility::Unlisted);
        assert_eq!(fields.net_category, NetCategory::Traffic);
        assert_eq!(fields.net_type, NetType::RollCall);
        assert_eq!(fields.grid.as_ref().map(|g| g.as_str()), Some("FN31pr"));
        assert_eq!(fields.expected_duration_minutes, Some(90));
    }

    #[test]
    fn optional_text_blank_or_absent_becomes_none() {
        let fields = parse_net_definition_fields(RawNetDefinition {
            description: Some("   ".to_owned()),
            country: None,
            ..valid_raw()
        })
        .expect("valid");
        assert_eq!(fields.description, None, "blank-after-trim clears");
        assert_eq!(fields.country, None);
        assert_eq!(fields.grid, None);
        assert_eq!(fields.expected_duration_minutes, None);
    }

    #[test]
    fn visibility_absent_or_blank_defaults_to_listed() {
        let absent = parse_net_definition_fields(RawNetDefinition {
            visibility: None,
            ..valid_raw()
        })
        .expect("valid");
        assert_eq!(absent.visibility, Visibility::Listed);

        let blank = parse_net_definition_fields(RawNetDefinition {
            visibility: Some("   ".to_owned()),
            ..valid_raw()
        })
        .expect("valid");
        assert_eq!(blank.visibility, Visibility::Listed);
    }

    #[test]
    fn explicit_unlisted_visibility_round_trips() {
        let fields = parse_net_definition_fields(RawNetDefinition {
            visibility: Some("unlisted".to_owned()),
            ..valid_raw()
        })
        .expect("valid");
        assert_eq!(fields.visibility, Visibility::Unlisted);
    }

    #[test]
    fn unknown_visibility_is_a_field_specific_error() {
        let err = parse_net_definition_fields(RawNetDefinition {
            visibility: Some("public".to_owned()),
            ..valid_raw()
        })
        .expect_err("unknown visibility rejects");
        assert_eq!(err, NetDefinitionError::VisibilityUnknown);
        assert!(err.to_string().starts_with("visibility:"));
        // Its detail differs from another field's error (distinct-detail).
        let bad_grid = parse_net_definition_fields(RawNetDefinition {
            grid: Some("nope".to_owned()),
            ..valid_raw()
        })
        .expect_err("malformed grid rejects");
        assert_ne!(err.to_string(), bad_grid.to_string());
    }

    #[test]
    fn title_over_the_length_bound_is_a_title_error() {
        let err = parse_net_definition_fields(RawNetDefinition {
            title: Some("x".repeat(121)),
            ..valid_raw()
        })
        .expect_err("over-long title");
        assert!(err.to_string().starts_with("title:"));
    }
    #[test]
    fn a_multi_line_description_is_accepted_and_crlf_normalizes_to_lf() {
        // The net-definition form hands the owner a `<textarea>`
        // (NetDefinitionFormPage.tsx) — a newline is the field's own
        // affordance, not a stray control character to refuse. A pasted
        // Windows-style CRLF description normalizes to plain `\n` so the
        // stored value is identical whatever line-ending convention the
        // client used, which is also what keeps `changed_fields` from
        // reporting a description change on every resubmitted save.
        let fields = parse_net_definition_fields(RawNetDefinition {
            description: Some("Net control opens at 0100Z.\r\n\r\nCheck-ins welcome.".to_owned()),
            ..valid_raw()
        })
        .expect("a multi-line description is valid");
        let description = fields.description.expect("description retained");
        assert_eq!(
            description, "Net control opens at 0100Z.\n\nCheck-ins welcome.",
            "CRLF normalizes to LF"
        );
        assert!(!description.contains('\r'), "no CR survives parsing");
    }

    #[test]
    fn a_lone_cr_in_a_description_also_normalizes_to_lf() {
        let fields = parse_net_definition_fields(RawNetDefinition {
            description: Some("line one\rline two".to_owned()),
            ..valid_raw()
        })
        .expect("a lone-CR description is valid");
        assert_eq!(
            fields.description.expect("description retained"),
            "line one\nline two"
        );
    }

    #[test]
    fn a_description_with_control_or_bidi_characters_is_still_rejected() {
        // Widening the field to prose admits `\n` and nothing else: every
        // other control character, and the bidi reordering primitives, stay
        // refused with the same field-level variant they carry today.
        // The variant now carries the offending code point, so each
        // input asserts WHICH character was named — the fact the message needs.
        for (bad, offender) in [
            ("bell\u{0007}here", '\u{0007}'),
            ("tab\there", '\t'),
            ("flip\u{202E}here", '\u{202E}'),
        ] {
            assert_eq!(
                parse_net_definition_fields(RawNetDefinition {
                    description: Some(bad.to_owned()),
                    ..valid_raw()
                }),
                Err(NetDefinitionError::Description(
                    ProfileError::IllegalCharacter(offender)
                )),
                "{bad:?} must still be refused"
            );
        }
    }

    #[test]
    fn a_description_over_its_own_bound_is_rejected_and_the_bound_is_not_the_notes_bound() {
        // Expressed in MAX_DESCRIPTION_CHARS, never the literal it happens to
        // hold today: MAX_NOTE_CHARS carries the same number, so a test
        // written against the value would stay green if description silently
        // started deriving its bound from the note's.
        let at_bound: String = (0..MAX_DESCRIPTION_CHARS)
            .map(|i| {
                if i % 40 == 39 && i != MAX_DESCRIPTION_CHARS - 1 {
                    '\n'
                } else {
                    'x'
                }
            })
            .collect();
        let fields = parse_net_definition_fields(RawNetDefinition {
            description: Some(at_bound.clone()),
            ..valid_raw()
        })
        .expect("a description at its own bound is valid");
        let stored = fields.description.expect("description retained");
        assert!(stored.contains('\n'), "the bound counts a multi-line value");
        assert_eq!(stored.chars().count(), MAX_DESCRIPTION_CHARS);

        assert_eq!(
            parse_net_definition_fields(RawNetDefinition {
                description: Some(format!("{at_bound}x")),
                ..valid_raw()
            }),
            Err(NetDefinitionError::Description(ProfileError::TooLong {
                max_chars: MAX_DESCRIPTION_CHARS
            })),
            "one character past MAX_DESCRIPTION_CHARS is refused"
        );
    }

    #[test]
    fn a_description_with_a_long_run_of_blank_lines_stores_as_one_blank_line() {
        // At the real call site. `"a" + "\n"×1998 + "b"` is
        // exactly MAX_DESCRIPTION_CHARS, was accepted verbatim before this
        // story, and renders ~1998 empty line boxes on the link-reachable
        // public net page (`PublicNetPage.tsx`, `white-space: pre-line`).
        // Asserting the STORED string, not its length: a length assertion
        // cannot distinguish one blank line from none.
        let hazard = format!("a{}b", "\n".repeat(1998));
        assert_eq!(hazard.chars().count(), MAX_DESCRIPTION_CHARS);
        let fields = parse_net_definition_fields(RawNetDefinition {
            description: Some(hazard),
            ..valid_raw()
        })
        .expect("the hazard input is accepted, not rejected");
        assert_eq!(
            fields.description.expect("description retained"),
            "a\n\nb",
            "the run folds to exactly one blank line"
        );
    }

    #[test]
    fn a_description_over_its_bound_only_because_of_a_newline_run_is_accepted() {
        // Ordering at the real call site: this value is
        // MAX_DESCRIPTION_CHARS + 2 characters raw and 4 after the collapse.
        // Expressed in the constant, never the literal 2000 — MAX_NOTE_CHARS
        // carries the same number, so a literal here would be unfalsifiable.
        let over_bound_only_by_newlines = format!("a{}b", "\n".repeat(MAX_DESCRIPTION_CHARS));
        assert!(over_bound_only_by_newlines.chars().count() > MAX_DESCRIPTION_CHARS);
        let fields = parse_net_definition_fields(RawNetDefinition {
            description: Some(over_bound_only_by_newlines),
            ..valid_raw()
        })
        .expect("collapsing happens before the bound is applied");
        assert_eq!(fields.description.expect("description retained"), "a\n\nb");
    }

    #[test]
    fn collapsing_blank_line_runs_leaves_the_single_line_guards_untouched() {
        // Confinement. The collapse belongs to the PROSE guard
        // and nowhere else. A single-line field handed a run of blank lines
        // must behave exactly as it did at e43b739 — refused on the newline,
        // never collapsed and never made acceptable. `title` reaches
        // `parse_bounded_text` directly; `country` reaches it through
        // `optional_text`, the helper `description` was moved OFF.
        // This test is GREEN before the change and must stay green after.
        let run = "\n".repeat(5);
        assert_eq!(
            parse_net_definition_fields(RawNetDefinition {
                title: Some(format!("Sunday{run}Traffic Net")),
                ..valid_raw()
            }),
            Err(NetDefinitionError::Title(ProfileError::IllegalCharacter(
                '\n'
            ))),
            "a single-line title never reaches a collapse"
        );
        assert_eq!(
            parse_net_definition_fields(RawNetDefinition {
                country: Some(format!("United{run}States")),
                ..valid_raw()
            }),
            Err(NetDefinitionError::Country(ProfileError::IllegalCharacter(
                '\n'
            ))),
            "nor does an optional single-line field"
        );
    }

    #[test]
    fn widening_description_to_prose_leaves_the_single_line_title_guard_untouched() {
        // Confinement, counter-direction: `title` reaches
        // `parse_bounded_text` directly, sharing that guard with the sixteen
        // other single-line fields it serves (dated snapshot: 2026-08-28), and
        // a newline in it must stay a rejection. This test is GREEN before the
        // change and must stay green after — it fails only if the fix is
        // applied to the shared single-line parser instead of to a prose
        // sibling.
        assert_eq!(
            parse_net_definition_fields(RawNetDefinition {
                title: Some("Sunday\nTraffic Net".to_owned()),
                ..valid_raw()
            }),
            Err(NetDefinitionError::Title(ProfileError::IllegalCharacter(
                '\n'
            ))),
            "title is single-line and stays single-line"
        );
    }

    #[test]
    fn widening_description_to_prose_leaves_the_shared_optional_text_guard_untouched() {
        // Confinement on the guard path `description` was actually moved
        // OFF. `title` above is a direct `parse_bounded_text` call; these two
        // fields reach it through `optional_text`, the helper `description`
        // used to share. Without this test, re-pointing `optional_text` at
        // `parse_bounded_multiline_text` widens both and the suite stays
        // green — the wrong implementation this exists to catch. (Four more
        // fields sat on this path until they retired with the flat
        // connection columns; their single-line guard now lives with the
        // connections.)
        for (label, raw, expected) in [
            (
                "country",
                RawNetDefinition {
                    country: Some("United\nStates".to_owned()),
                    ..valid_raw()
                },
                NetDefinitionError::Country(ProfileError::IllegalCharacter('\n')),
            ),
            (
                "state",
                RawNetDefinition {
                    state: Some("New\nYork".to_owned()),
                    ..valid_raw()
                },
                NetDefinitionError::State(ProfileError::IllegalCharacter('\n')),
            ),
        ] {
            assert_eq!(
                parse_net_definition_fields(raw),
                Err(expected),
                "{label} is single-line and stays single-line"
            );
        }
    }
}
