// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
//! Public-discovery query validation: raw wire strings in, a
//! [`DiscoveryQuery`] the adapter trusts out, and a field-level error whose
//! `Display` becomes the problem+json `detail`. Enum filters validate against
//! the SAME `enums.rs` taxonomy the definition uses, so what reaches a
//! `WHERE col = $n` is always in-vocabulary; free text is trimmed and bounded.

use thiserror::Error;

use crate::profile::{ProfileError, parse_bounded_text};

use super::connection::NetConnectionKindToken;
use super::enums::{Band, Mode, NetCategory, NetType};

/// Max characters (post-trim) for the free-text title substring filter —
/// mirrors the definition title bound (`MAX_TITLE_CHARS`).
const MAX_NAME_FILTER_CHARS: usize = 120;
/// Max characters (post-trim) for a free-text geography filter (country /
/// state / grid) — mirrors the definition geography bound
/// (`MAX_GEOGRAPHY_CHARS`).
const MAX_GEOGRAPHY_FILTER_CHARS: usize = 80;

/// How the upcoming list is ordered. `Time` (soonest
/// `scheduledStartAt` first) is the default when no sort is requested. The
/// variant list is the ONLY place the sortable-key vocabulary lives; an
/// unknown key is rejected at the boundary. What was ONCE in this list and is
/// not any more lives beside it in [`RetiredSort`] — same fact, other half.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DiscoverySort {
    /// Soonest scheduled start first (the default).
    #[default]
    Time,
    /// By net title, ascending.
    Name,
    /// By category token, ascending.
    Category,
    /// By net-type token, ascending.
    Type,
}

impl DiscoverySort {
    /// The stable wire/query token for this sort key. The adapter's ORDER BY
    /// switches on this exact token, so it is also the value the wire accepts.
    pub fn as_query_token(self) -> &'static str {
        match self {
            DiscoverySort::Time => "time",
            DiscoverySort::Name => "name",
            DiscoverySort::Category => "category",
            DiscoverySort::Type => "type",
        }
    }
}

impl TryFrom<&str> for DiscoverySort {
    type Error = ();

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        match value {
            "time" => Ok(DiscoverySort::Time),
            "name" => Ok(DiscoverySort::Name),
            "category" => Ok(DiscoverySort::Category),
            "type" => Ok(DiscoverySort::Type),
            _ => Err(()),
        }
    }
}

/// A sort key this endpoint ONCE offered and no longer does.
///
/// It is a closed, named set rather than a string comparison because it decides
/// which out-of-vocabulary token gets a `200` and which gets a `400`. A token in
/// here arrived on a URL that was valid when somebody shared it, so the request
/// degrades to the default ordering and the response SAYS SO. Every other
/// out-of-vocabulary token is a typo nobody could be holding a link to, and stays
/// a field-level rejection — falling back for those would turn a misspelling into
/// a silently wrong ordering.
///
/// `band` and `mode` are here because a net now has a SET of connections and so
/// a set of bands: `ORDER BY` over a set has no defensible answer, so the option
/// was removed rather than given an arbitrary one.
///
/// **Removing a token from this set re-breaks every link that still carries it.**
/// A token leaves only when nobody could still be holding such a URL — which is a
/// judgement about the outside world, not about this file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RetiredSort {
    /// `?sort=band`, retired.
    Band,
    /// `?sort=mode`, retired.
    Mode,
}

impl RetiredSort {
    /// The wire token this retired sort was requested by — the value the
    /// response states as the one it could not honour.
    pub fn as_query_token(self) -> &'static str {
        match self {
            RetiredSort::Band => "band",
            RetiredSort::Mode => "mode",
        }
    }
}

impl TryFrom<&str> for RetiredSort {
    type Error = ();

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        match value {
            "band" => Ok(RetiredSort::Band),
            "mode" => Ok(RetiredSort::Mode),
            _ => Err(()),
        }
    }
}

/// A connection kind a visitor can filter discovery by.
///
/// A newtype over [`NetConnectionKindToken`] rather than a re-listing of it, so
/// the filter vocabulary IS the domain's kind vocabulary: a tenth kind reaches
/// this filter the moment the token enum names it, and nothing here can drift.
///
/// `other` is refused BY NAME, and the refusal is a decision rather than the
/// fallout of a short list: `other`
/// is instrumentation — a count of it says whether the closed set is cut wrong
/// — and a browsing category is exactly the "peer choice in the same list" that
/// item forbids. Offering it would also surface every `unclassified-*` row the
/// backfill minted as a public way to reach a net, and it answers
/// nobody's question: a visitor filters by kind because they HAVE an EchoLink
/// node or a DMR radio, and nobody has an "other". The cost is stated rather
/// than hidden — a net whose only way in is an owner-authored `other` is
/// findable by no kind filter — and the remedy is the one item 9 names: if
/// `other` grows to a meaningful share, the enum is cut wrong and gets re-cut.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ConnectionKindFilter(NetConnectionKindToken);

impl ConnectionKindFilter {
    /// The token this filter names.
    pub fn token(self) -> NetConnectionKindToken {
        self.0
    }

    /// The stable wire/storage spelling — the exact `net_connections.kind`
    /// column value the adapter binds.
    pub fn as_str(self) -> &'static str {
        self.0.as_str()
    }
}

impl TryFrom<&str> for ConnectionKindFilter {
    type Error = ();

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        match NetConnectionKindToken::try_from(value)? {
            NetConnectionKindToken::Other => Err(()),
            token => Ok(Self(token)),
        }
    }
}

/// Why a submitted discovery query was rejected — one variant per field and
/// reason. `Display` is `"<field>: <reason>"` so the HTTP `detail` names the
/// offending field; distinct fields yield distinct text.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum DiscoveryQueryError {
    /// Band filter token is not a known band.
    #[error("band: is not a recognized band")]
    BandUnknown,
    /// Mode filter token is not a known mode.
    #[error("mode: is not a recognized mode")]
    ModeUnknown,
    /// Connection-kind filter token is not a filterable kind.
    /// `other` lands here too, by decision — see [`ConnectionKindFilter`].
    #[error("kind: is not a recognized connection kind")]
    KindUnknown,
    /// Category filter token is not a known category.
    #[error("category: is not a recognized category")]
    CategoryUnknown,
    /// Net-type filter token is not a known type.
    #[error("type: is not a recognized type")]
    TypeUnknown,
    /// Sort key is not a known sortable field.
    #[error("sort: is not a recognized sort key")]
    SortUnknown,
    /// Free-text name filter failed the bounded-text guard.
    #[error("name: {0}")]
    Name(ProfileError),
    /// Free-text country filter failed the bounded-text guard.
    #[error("country: {0}")]
    Country(ProfileError),
    /// Free-text state filter failed the bounded-text guard.
    #[error("state: {0}")]
    State(ProfileError),
    /// Free-text grid filter failed the bounded-text guard.
    #[error("grid: {0}")]
    Grid(ProfileError),
}

/// The unvalidated inbound discovery query — every filter/sort as it arrives
/// on the wire (`Option<String>`). `parse_discovery_query` turns it into the
/// typed [`DiscoveryQuery`] the adapter trusts.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RawDiscoveryQuery {
    /// Case-insensitive title substring (optional).
    pub name: Option<String>,
    /// Band token (optional).
    pub band: Option<String>,
    /// Mode token (optional).
    pub mode: Option<String>,
    /// Connection-kind token (optional).
    pub kind: Option<String>,
    /// Country free text (optional).
    pub country: Option<String>,
    /// State/province free text (optional).
    pub state: Option<String>,
    /// Maidenhead grid free text (optional).
    pub grid: Option<String>,
    /// Category token (optional).
    pub net_category: Option<String>,
    /// Net-type token (optional).
    pub net_type: Option<String>,
    /// Sort key token (optional; absent/blank defaults to `time`).
    pub sort: Option<String>,
}

/// The validated discovery filters the adapter binds as `$n` parameters. Enum
/// filters are typed variants; free-text filters are trimmed strings. Every
/// `None` means "this dimension is not filtered".
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DiscoveryFilters {
    /// Case-insensitive title substring.
    pub name: Option<String>,
    /// Band.
    pub band: Option<Band>,
    /// Mode.
    pub mode: Option<Mode>,
    /// Connection kind — match-any over the net's connections,
    /// inside the same `EXISTS` as band and mode.
    pub kind: Option<ConnectionKindFilter>,
    /// Country.
    pub country: Option<String>,
    /// State/province.
    pub state: Option<String>,
    /// Maidenhead grid.
    pub grid: Option<String>,
    /// Category.
    pub net_category: Option<NetCategory>,
    /// Net type.
    pub net_type: Option<NetType>,
}

/// The validated discovery query: the filter set plus the requested ordering.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DiscoveryQuery {
    /// The filters to AND together (any `None` field is not filtered).
    pub filters: DiscoveryFilters,
    /// The requested ordering (defaults to `Time`).
    pub sort: DiscoverySort,
    /// The sort key the request asked for that this endpoint no longer offers,
    /// when the request asked for one. `Some` means `sort` above is
    /// a FALLBACK rather than what was requested, and the boundary is expected
    /// to say so; `None` means the ordering is the one that was asked for.
    pub sort_unavailable: Option<RetiredSort>,
}

/// `None`/blank-after-trim → `None`; otherwise parse the bounded text and wrap
/// any error via `wrap`. Mirrors `validation.rs`'s `optional_text`.
fn optional_text(
    submitted: Option<&str>,
    max_chars: usize,
    wrap: impl Fn(ProfileError) -> DiscoveryQueryError,
) -> Result<Option<String>, DiscoveryQueryError> {
    match submitted {
        None => Ok(None),
        Some(s) if s.trim().is_empty() => Ok(None),
        Some(s) => parse_bounded_text(s, max_chars).map(Some).map_err(wrap),
    }
}

/// `None`/blank-after-trim → `None`; otherwise validate the token against the
/// enum taxonomy, mapping an out-of-vocabulary token to `err`.
fn optional_enum<T>(
    submitted: Option<&str>,
    err: DiscoveryQueryError,
) -> Result<Option<T>, DiscoveryQueryError>
where
    for<'a> T: TryFrom<&'a str, Error = ()>,
{
    match submitted {
        None => Ok(None),
        Some(s) if s.trim().is_empty() => Ok(None),
        Some(s) => Ok(Some(T::try_from(s).map_err(|()| err)?)),
    }
}

/// Validates a raw discovery query: enum filters against the `enums.rs`
/// taxonomy (unknown token → field-level rejection), free-text filters bounded
/// and trimmed, sort key against the known set (absent/blank defaults to
/// `Time`; a [`RetiredSort`] token falls back to `Time` and is recorded in
/// `sort_unavailable`; anything else is a rejection). Fails fast on the first
/// invalid field so the handler never issues a query with an out-of-vocabulary
/// bound value.
pub fn parse_discovery_query(
    raw: RawDiscoveryQuery,
) -> Result<DiscoveryQuery, DiscoveryQueryError> {
    let name = optional_text(
        raw.name.as_deref(),
        MAX_NAME_FILTER_CHARS,
        DiscoveryQueryError::Name,
    )?;
    let band = optional_enum::<Band>(raw.band.as_deref(), DiscoveryQueryError::BandUnknown)?;
    let mode = optional_enum::<Mode>(raw.mode.as_deref(), DiscoveryQueryError::ModeUnknown)?;
    let kind = optional_enum::<ConnectionKindFilter>(
        raw.kind.as_deref(),
        DiscoveryQueryError::KindUnknown,
    )?;
    let country = optional_text(
        raw.country.as_deref(),
        MAX_GEOGRAPHY_FILTER_CHARS,
        DiscoveryQueryError::Country,
    )?;
    let state = optional_text(
        raw.state.as_deref(),
        MAX_GEOGRAPHY_FILTER_CHARS,
        DiscoveryQueryError::State,
    )?;
    let grid = optional_text(
        raw.grid.as_deref(),
        MAX_GEOGRAPHY_FILTER_CHARS,
        DiscoveryQueryError::Grid,
    )?;
    let net_category = optional_enum::<NetCategory>(
        raw.net_category.as_deref(),
        DiscoveryQueryError::CategoryUnknown,
    )?;
    let net_type =
        optional_enum::<NetType>(raw.net_type.as_deref(), DiscoveryQueryError::TypeUnknown)?;

    // Three outcomes, not two. Absent or blank-after-trim
    // defaults to Time. A token in the RETIRED set was valid when somebody
    // shared the URL, so it falls back to Time and the refusal travels out in
    // `sort_unavailable` for the boundary to state — a shared link degrades
    // rather than breaking. Anything else is still a field-level rejection
    // (never silently ignored): nobody holds a link to a typo, and
    // swallowing one would answer a misspelling with a silently wrong ordering.
    let (sort, sort_unavailable) = match raw.sort.as_deref() {
        None => (DiscoverySort::Time, None),
        Some(s) if s.trim().is_empty() => (DiscoverySort::Time, None),
        Some(s) => match DiscoverySort::try_from(s) {
            Ok(sort) => (sort, None),
            Err(()) => match RetiredSort::try_from(s) {
                Ok(retired) => (DiscoverySort::Time, Some(retired)),
                Err(()) => return Err(DiscoveryQueryError::SortUnknown),
            },
        },
    };

    Ok(DiscoveryQuery {
        filters: DiscoveryFilters {
            name,
            band,
            mode,
            kind,
            country,
            state,
            grid,
            net_category,
            net_type,
        },
        sort,
        sort_unavailable,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_query_yields_no_filters_and_the_default_time_sort() {
        let query = parse_discovery_query(RawDiscoveryQuery::default()).expect("empty is valid");
        assert_eq!(query.filters, DiscoveryFilters::default());
        assert_eq!(query.sort, DiscoverySort::Time, "default sort is time");
    }

    #[test]
    fn valid_enum_filters_parse_to_typed_variants() {
        let query = parse_discovery_query(RawDiscoveryQuery {
            band: Some("20m".to_owned()),
            mode: Some("ssb".to_owned()),
            net_category: Some("traffic".to_owned()),
            net_type: Some("roll-call".to_owned()),
            ..Default::default()
        })
        .expect("valid enum filters");
        assert_eq!(query.filters.band, Some(Band::TwentyMeters));
        assert_eq!(query.filters.mode, Some(Mode::Ssb));
        assert_eq!(query.filters.net_category, Some(NetCategory::Traffic));
        assert_eq!(query.filters.net_type, Some(NetType::RollCall));
    }

    #[test]
    fn unknown_enum_tokens_are_field_specific_errors() {
        assert_eq!(
            parse_discovery_query(RawDiscoveryQuery {
                band: Some("21m".to_owned()),
                ..Default::default()
            }),
            Err(DiscoveryQueryError::BandUnknown)
        );
        assert_eq!(
            parse_discovery_query(RawDiscoveryQuery {
                mode: Some("phone".to_owned()),
                ..Default::default()
            }),
            Err(DiscoveryQueryError::ModeUnknown)
        );
        assert_eq!(
            parse_discovery_query(RawDiscoveryQuery {
                net_category: Some("nonsense".to_owned()),
                ..Default::default()
            }),
            Err(DiscoveryQueryError::CategoryUnknown)
        );
        assert_eq!(
            parse_discovery_query(RawDiscoveryQuery {
                net_type: Some("rollcall".to_owned()),
                ..Default::default()
            }),
            Err(DiscoveryQueryError::TypeUnknown)
        );
    }

    #[test]
    fn a_kind_filter_parses_to_the_token_it_names() {
        // Every filterable kind — the token enum minus
        // `other` — parses through the same `optional_enum` seam band and mode
        // use, and the parsed filter spells itself exactly as it was asked for.
        // Iterated over the domain's own tokens, never a hand-typed list.
        let query = parse_discovery_query(RawDiscoveryQuery {
            kind: Some("echolink".to_owned()),
            ..Default::default()
        })
        .expect("a real kind is a valid filter");
        assert_eq!(
            query.filters.kind.map(ConnectionKindFilter::token),
            Some(NetConnectionKindToken::EchoLink)
        );

        let filterable = [
            NetConnectionKindToken::Hf,
            NetConnectionKindToken::Repeater,
            NetConnectionKindToken::EchoLink,
            NetConnectionKindToken::AllStar,
            NetConnectionKindToken::Dmr,
            NetConnectionKindToken::DStar,
            NetConnectionKindToken::Ysf,
            NetConnectionKindToken::Urf,
        ];
        assert_eq!(filterable.len(), 8, "the eight the picker offers");
        for token in filterable {
            let query = parse_discovery_query(RawDiscoveryQuery {
                kind: Some(token.as_str().to_owned()),
                ..Default::default()
            })
            .unwrap_or_else(|e| panic!("{} must be filterable: {e}", token.as_str()));
            let filter = query.filters.kind.expect("a kind filter was applied");
            assert_eq!(filter.as_str(), token.as_str());
            assert_eq!(filter.token(), token);
        }
    }

    #[test]
    fn other_is_not_a_filterable_kind() {
        // The one product decision in the story, asserted BY NAME so it
        // cannot become the accidental fallout of a short list. `other` is a
        // real token (`NetConnectionKindToken::try_from` accepts it) and is
        // still refused as a filter.
        assert_eq!(
            NetConnectionKindToken::try_from("other"),
            Ok(NetConnectionKindToken::Other),
            "other IS a kind token — the refusal below is the filter's, not the vocabulary's"
        );
        assert_eq!(ConnectionKindFilter::try_from("other"), Err(()));
        assert_eq!(
            parse_discovery_query(RawDiscoveryQuery {
                kind: Some("other".to_owned()),
                ..Default::default()
            }),
            Err(DiscoveryQueryError::KindUnknown)
        );
    }

    #[test]
    fn an_unknown_kind_token_is_a_kind_specific_error() {
        for wrong in ["zzz", "EchoLink", "echo-link"] {
            assert_eq!(
                parse_discovery_query(RawDiscoveryQuery {
                    kind: Some(wrong.to_owned()),
                    ..Default::default()
                }),
                Err(DiscoveryQueryError::KindUnknown),
                "{wrong} is not a connection kind"
            );
        }
        assert!(
            DiscoveryQueryError::KindUnknown
                .to_string()
                .starts_with("kind:")
        );
    }

    #[test]
    fn blank_kind_is_not_a_filter() {
        for blank in ["", "   "] {
            let query = parse_discovery_query(RawDiscoveryQuery {
                kind: Some(blank.to_owned()),
                ..Default::default()
            })
            .expect("blank clears, not rejects");
            assert_eq!(query.filters.kind, None);
        }
    }

    #[test]
    fn kind_takes_no_part_in_the_retired_sort_fallback() {
        // REGRESSION PIN, expected GREEN on write and green
        // after. It refuses one specific wrong reading of "add a dimension":
        // `kind` was never a sort key, so `?sort=kind` is a rejection, not a
        // fallback, and `RetiredSort` stays a closed set of two.
        assert_eq!(
            parse_discovery_query(RawDiscoveryQuery {
                sort: Some("kind".to_owned()),
                ..Default::default()
            }),
            Err(DiscoveryQueryError::SortUnknown)
        );
        assert_eq!(RetiredSort::try_from("kind"), Err(()));
    }

    #[test]
    fn distinct_field_failures_carry_distinct_field_prefixed_detail() {
        let bad_band = DiscoveryQueryError::BandUnknown.to_string();
        let bad_mode = DiscoveryQueryError::ModeUnknown.to_string();
        assert_ne!(bad_band, bad_mode);
        assert!(bad_band.starts_with("band:"));
        assert!(bad_mode.starts_with("mode:"));
    }

    #[test]
    fn enum_filters_are_case_sensitive_kebab() {
        assert_eq!(
            parse_discovery_query(RawDiscoveryQuery {
                band: Some("20M".to_owned()),
                ..Default::default()
            }),
            Err(DiscoveryQueryError::BandUnknown),
            "uppercase band token is rejected (case-sensitive)"
        );
    }

    #[test]
    fn blank_enum_filters_are_not_filters() {
        let query = parse_discovery_query(RawDiscoveryQuery {
            band: Some("   ".to_owned()),
            mode: Some("".to_owned()),
            ..Default::default()
        })
        .expect("blank filters clear, not reject");
        assert_eq!(query.filters.band, None);
        assert_eq!(query.filters.mode, None);
    }

    #[test]
    fn free_text_filters_are_trimmed_and_passed_through() {
        let query = parse_discovery_query(RawDiscoveryQuery {
            name: Some("  Sunday  ".to_owned()),
            country: Some("USA".to_owned()),
            state: Some("CT".to_owned()),
            grid: Some("FN31".to_owned()),
            ..Default::default()
        })
        .expect("valid free text");
        assert_eq!(query.filters.name.as_deref(), Some("Sunday"), "trimmed");
        assert_eq!(query.filters.country.as_deref(), Some("USA"));
        assert_eq!(query.filters.state.as_deref(), Some("CT"));
        assert_eq!(query.filters.grid.as_deref(), Some("FN31"));
    }

    #[test]
    fn an_over_long_name_filter_is_a_name_error() {
        let err = parse_discovery_query(RawDiscoveryQuery {
            name: Some("x".repeat(MAX_NAME_FILTER_CHARS + 1)),
            ..Default::default()
        })
        .expect_err("over-long name rejects");
        assert!(err.to_string().starts_with("name:"));
    }

    #[test]
    fn a_retired_sort_token_parses_to_the_default_sort_and_records_what_it_refused() {
        // A live, publicly shareable `?sort=band` link must degrade,
        // not 400 — and the fallback must be STATED, so the query carries the
        // token it could not honour rather than silently substituting `time`.
        let query = parse_discovery_query(RawDiscoveryQuery {
            sort: Some("band".to_owned()),
            ..Default::default()
        })
        .expect("a retired sort token parses rather than rejecting");
        assert_eq!(query.sort, DiscoverySort::Time);
        assert_eq!(query.sort_unavailable, Some(RetiredSort::Band));

        let query = parse_discovery_query(RawDiscoveryQuery {
            sort: Some("mode".to_owned()),
            ..Default::default()
        })
        .expect("a retired sort token parses rather than rejecting");
        assert_eq!(query.sort, DiscoverySort::Time);
        assert_eq!(query.sort_unavailable, Some(RetiredSort::Mode));
    }

    #[test]
    fn a_sort_that_was_honoured_records_no_refusal() {
        // The refusal is recorded ONLY when one happened; an honoured sort and
        // an absent one both leave it `None`, so the HTTP echo cannot emit the
        // fallback key unconditionally.
        for raw in [
            RawDiscoveryQuery::default(),
            RawDiscoveryQuery {
                sort: Some("name".to_owned()),
                ..Default::default()
            },
            RawDiscoveryQuery {
                sort: Some("   ".to_owned()),
                ..Default::default()
            },
        ] {
            let query = parse_discovery_query(raw).expect("valid");
            assert_eq!(query.sort_unavailable, None);
        }
    }

    #[test]
    fn the_retired_vocabulary_is_a_closed_named_set_with_its_own_wire_tokens() {
        assert_eq!(RetiredSort::Band.as_query_token(), "band");
        assert_eq!(RetiredSort::Mode.as_query_token(), "mode");
        assert_eq!(RetiredSort::try_from("band"), Ok(RetiredSort::Band));
        assert_eq!(RetiredSort::try_from("mode"), Ok(RetiredSort::Mode));
        assert_eq!(RetiredSort::try_from("time"), Err(()));
        assert_eq!(RetiredSort::try_from("zzz"), Err(()));
    }

    #[test]
    fn an_out_of_vocabulary_sort_token_that_was_never_a_sort_key_is_still_a_rejection() {
        // REGRESSION PIN, expected GREEN on write and green
        // after the fix. It exists to REFUSE one specific implementation of the
        // `?sort=band` fallback: falling back for EVERY unrecognised token.
        // That reading of "fall back to the default sort" turns a typo into a
        // silently wrong ordering — the "wrong data with a 200" this fallback
        // exists to prevent, and a filter must never be silently ignored. Only
        // the CLOSED retired set falls back.
        for token in ["zzz", "Time", "bands", "Band"] {
            assert_eq!(
                parse_discovery_query(RawDiscoveryQuery {
                    sort: Some(token.to_owned()),
                    ..Default::default()
                }),
                Err(DiscoveryQueryError::SortUnknown),
                "{token} was never a sort key, so it is a field-level rejection, not a fallback"
            );
        }
    }

    #[test]
    fn sort_key_parses_and_defaults_and_rejects_unknown() {
        assert_eq!(
            parse_discovery_query(RawDiscoveryQuery {
                sort: Some("name".to_owned()),
                ..Default::default()
            })
            .expect("valid sort")
            .sort,
            DiscoverySort::Name
        );
        // Absent and blank both default to Time.
        assert_eq!(
            parse_discovery_query(RawDiscoveryQuery {
                sort: Some("  ".to_owned()),
                ..Default::default()
            })
            .expect("blank sort defaults")
            .sort,
            DiscoverySort::Time
        );
        assert_eq!(
            parse_discovery_query(RawDiscoveryQuery {
                sort: Some("nonsense".to_owned()),
                ..Default::default()
            }),
            Err(DiscoveryQueryError::SortUnknown)
        );
        // Case-sensitive: "Time" is not "time".
        assert_eq!(
            parse_discovery_query(RawDiscoveryQuery {
                sort: Some("Time".to_owned()),
                ..Default::default()
            }),
            Err(DiscoveryQueryError::SortUnknown)
        );
    }
}
