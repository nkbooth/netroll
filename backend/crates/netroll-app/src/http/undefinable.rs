// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
//! A three-state request field: absent, explicitly null, or valued.
//!
//! A bare `#[serde(default)] Option<T>` collapses two client intentions into
//! one, which on a PUT-replace endpoint silently erases fields another UI
//! populated. This keeps "leave what is stored" apart from "clear it".

use serde::{Deserialize, Deserializer};

/// A request field that distinguishes ABSENT from explicitly `null`.
///
/// Deserializes correctly only in combination with `#[serde(default)]` on the
/// field, which is what supplies the [`Undefinable::Missing`] arm — serde has
/// no other way to signal "this key was not on the wire".
///
/// ```ignore
/// #[derive(Deserialize)]
/// struct Body {
/// #[serde(default)]
/// grid: Undefinable<String>,
/// }
/// // {} -> Missing (keep whatever is stored)
/// // {"grid": null} -> Null (clear it)
/// // {"grid": "FN31"} -> Value (replace it)
/// ```
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Undefinable<T> {
    /// The key was not present in the request body at all.
    #[default]
    Missing,
    /// The key was present, valued `null`.
    Null,
    /// The key was present with a value.
    Value(T),
}

impl<T> Undefinable<T> {
    /// Whether the client SENT a value for this field. An explicit `null`
    /// answers `false` here — it is a submitted absence, not a submitted value.
    ///
    /// This is the predicate a staff-only-field check wants: it reproduces
    /// exactly what `Option::is_some` decided before the three-state split.
    pub fn has_value(&self) -> bool {
        matches!(self, Undefinable::Value(_))
    }

    /// Whether the client OMITTED this field entirely, leaving the handler to
    /// fall back to the stored value.
    pub fn is_missing(&self) -> bool {
        matches!(self, Undefinable::Missing)
    }

    /// The submitted value, or `None` when the field was absent or `null`.
    pub fn value(&self) -> Option<&T> {
        match self {
            Undefinable::Value(v) => Some(v),
            Undefinable::Missing | Undefinable::Null => None,
        }
    }
}

impl<'de, T> Deserialize<'de> for Undefinable<T>
where
    T: Deserialize<'de>,
{
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        // This is only reached when the key IS present, so `None` here can only
        // mean an explicit `null`. An absent key never calls `deserialize` at
        // all — `#[serde(default)]` supplies `Missing` instead.
        Option::<T>::deserialize(deserializer).map(|opt| match opt {
            Some(value) => Undefinable::Value(value),
            None => Undefinable::Null,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Deserialize;

    #[derive(Deserialize)]
    struct Body {
        #[serde(default)]
        grid: Undefinable<String>,
        #[serde(default)]
        traffic: Undefinable<i64>,
    }

    fn body(json: &str) -> Body {
        serde_json::from_str(json).expect("valid body")
    }

    #[test]
    fn an_absent_key_is_missing_not_null() {
        let b = body("{}");
        assert_eq!(b.grid, Undefinable::Missing);
        assert_eq!(b.traffic, Undefinable::Missing);
        assert!(b.grid.is_missing());
        assert!(!b.grid.has_value());
        assert_eq!(b.grid.value(), None);
    }

    #[test]
    fn an_explicit_null_is_null_not_missing() {
        let b = body(r#"{"grid": null, "traffic": null}"#);
        assert_eq!(b.grid, Undefinable::Null);
        assert_eq!(b.traffic, Undefinable::Null);
        assert!(!b.grid.is_missing(), "null was SENT — it is not absent");
        assert!(!b.grid.has_value(), "null carries no value");
        assert_eq!(b.grid.value(), None);
    }

    #[test]
    fn a_present_value_carries_it() {
        let b = body(r#"{"grid": "FN31", "traffic": 3}"#);
        assert_eq!(b.grid.value().map(String::as_str), Some("FN31"));
        assert_eq!(b.traffic.value().copied(), Some(3));
        assert!(b.grid.has_value());
        assert!(!b.grid.is_missing());
    }

    #[test]
    fn a_blank_string_is_a_value_not_an_absence() {
        // The detail modal clears a field by sending "" (PUT-replace). That is a
        // SUBMITTED value the parser turns into a clear — it must never be
        // confused with the field being absent, or a deliberate clear would
        // silently become a keep.
        let b = body(r#"{"grid": ""}"#);
        assert_eq!(b.grid, Undefinable::Value(String::new()));
        assert!(b.grid.has_value());
    }
}
