// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
//! Best-effort callbook lookup value types and pure field-mapping helpers. The
//! domain owns only the RESULT SHAPE and the interface; wire-format parsing
//! lives in the adapters crate. [`normalize`] and [`join_fields`] encode the
//! "trim, drop empties, join present parts" rule BOTH adapters share, kept here
//! so it is unit-tested without any I/O.

use thiserror::Error;

/// Which provider answered a lookup. Carried on [`CallsignRecord`] so the
/// merge (and any audit or debug signal) can tell QRZ hits from hamcall hits
/// without re-deriving it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LookupSource {
    /// The QRZ.com XML callbook answered.
    Qrz,
    /// The hamcall.dev FCC-ULS fallback answered.
    Hamcall,
}

/// A best-effort callbook result.
///
/// All descriptive fields are `Option<String>` RAW strings — the callbook is an
/// untrusted external source, so this type carries what the provider returned
/// WITHOUT applying NetRoll's `Name`/`Location` domain validation. The
/// autofill merge parses these into the domain newtypes at the check-in
/// boundary and applies
/// the profile-override and roster-memory merge. Mirrors the
/// `Option<String>` shape of `RememberedStation` for a clean merge.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CallsignRecord {
    /// The resolved/canonical callsign as returned (fallback: the queried call).
    pub callsign: String,
    /// The combined operator name, or `None` if the provider supplied none.
    pub name: Option<String>,
    /// A human-readable location (e.g. `"City, ST"`), or `None`.
    pub location: Option<String>,
    /// The Maidenhead grid, if the provider supplied one.
    pub grid: Option<String>,
    /// Which provider answered.
    pub source: LookupSource,
}

/// A provider-level failure. The [`crate::ports::LookupProvider`] port returns
/// this on any failure; the app-layer service maps every variant to "fall
/// through to the next provider" and ultimately to `None` for the caller —
/// best-effort, never blocks check-in.
///
/// Deliberately only two variants: the service does not act differently on
/// finer categories, so `Malformed`/`Unavailable`/timeout all collapse to the
/// same "try the next provider" behavior. [`LookupError::InvalidCredentials`]
/// is kept separate ONLY so the QRZ adapter can avoid a pointless re-login loop
/// and a future nudge can debug-signal "your QRZ credentials look wrong".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum LookupError {
    /// Network/timeout/upstream 5xx/rate-limited/size-cap/malformed response —
    /// anything transient or unclassifiable. Try the next provider.
    #[error("callbook provider is unavailable")]
    Unavailable,
    /// The provider explicitly rejected the QRZ username/password. Do not retry
    /// the login; fall through to the next provider.
    #[error("callbook provider rejected the supplied credentials")]
    InvalidCredentials,
}

/// Trims `value`, returning `None` when it is absent or empty/whitespace-only.
/// The single "is this field actually present?" rule the mapping helpers share.
pub fn normalize(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|trimmed| !trimmed.is_empty())
        .map(str::to_owned)
}

/// Joins two optional fragments with `sep`, trimming each and dropping
/// empty/whitespace-only parts. Returns the surviving part alone when only one
/// is present, and `None` when both are absent. This is the shared projection
/// rule for both a name (`"FRED" + " " + "LLOYD"`) and a location
/// (`"SCOTTSDALE" + ", " + "AZ"`).
pub fn join_fields(a: Option<&str>, b: Option<&str>, sep: &str) -> Option<String> {
    match (normalize(a), normalize(b)) {
        (Some(a), Some(b)) => Some(format!("{a}{sep}{b}")),
        (Some(only), None) | (None, Some(only)) => Some(only),
        (None, None) => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_trims_and_drops_empty() {
        assert_eq!(normalize(Some("  W1AW ")), Some("W1AW".to_owned()));
        assert_eq!(normalize(Some("   ")), None);
        assert_eq!(normalize(Some("")), None);
        assert_eq!(normalize(None), None);
    }

    #[test]
    fn join_fields_joins_both_present_with_separator() {
        assert_eq!(
            join_fields(Some("SCOTTSDALE"), Some("AZ"), ", "),
            Some("SCOTTSDALE, AZ".to_owned())
        );
        assert_eq!(
            join_fields(Some("FRED"), Some("LLOYD"), " "),
            Some("FRED LLOYD".to_owned())
        );
    }

    #[test]
    fn join_fields_returns_the_lone_present_part_without_separator() {
        assert_eq!(
            join_fields(Some("SCOTTSDALE"), None, ", "),
            Some("SCOTTSDALE".to_owned())
        );
        assert_eq!(join_fields(None, Some("AZ"), ", "), Some("AZ".to_owned()));
        assert_eq!(
            join_fields(Some("SCOTTSDALE"), Some("  "), ", "),
            Some("SCOTTSDALE".to_owned())
        );
    }

    #[test]
    fn join_fields_is_none_when_both_absent() {
        assert_eq!(join_fields(None, None, ", "), None);
        assert_eq!(join_fields(Some(" "), Some(""), ", "), None);
    }

    #[test]
    fn callsign_record_carries_its_source() {
        let rec = CallsignRecord {
            callsign: "AA7BQ".to_owned(),
            name: join_fields(Some("FRED"), Some("LLOYD"), " "),
            location: join_fields(Some("SCOTTSDALE"), Some("AZ"), ", "),
            grid: normalize(Some("DM32af")),
            source: LookupSource::Qrz,
        };
        assert_eq!(rec.name.as_deref(), Some("FRED LLOYD"));
        assert_eq!(rec.location.as_deref(), Some("SCOTTSDALE, AZ"));
        assert_eq!(rec.grid.as_deref(), Some("DM32af"));
        assert_eq!(rec.source, LookupSource::Qrz);
    }
}
