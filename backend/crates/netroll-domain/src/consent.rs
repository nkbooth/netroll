// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
//! Pure consent decisions: the verdict against the required terms version
//! and the recording decision.
//!
//! Clock-injected and I/O-free like the rest of the domain; persistence of
//! accepted records lives in adapters, enforcement in the app layer.

use thiserror::Error;

/// The terms/privacy version the server currently requires (date-stamped).
///
/// The server is the sole authority on this value: clients learn it from
/// the API and echo it back, never hardcode it. Bumping it forces
/// re-consent structurally (security review L-4).
pub const CURRENT_TERMS_VERSION: &str = "2026-07-15";

/// Outcome of judging an account's recorded consents against the required
/// version.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConsentVerdict {
    /// The required version is among the recorded consents.
    Consented,
    /// No recorded consent covers the required version; gated actions must
    /// refuse until one is recorded.
    ConsentRequired,
}

/// Judges consent: only a recorded consent for exactly the required version
/// counts — consent to older versions does not carry forward.
pub fn consent_verdict(consented_versions: &[String], required_version: &str) -> ConsentVerdict {
    if consented_versions.iter().any(|v| v == required_version) {
        ConsentVerdict::Consented
    } else {
        ConsentVerdict::ConsentRequired
    }
}

/// Refusal to record consent for a version the server did not ask for — a
/// stale gate page after a version bump must not record consent to the old
/// terms.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("submitted terms version does not match the required version")]
pub struct ConsentVersionMismatch {
    /// The version the server currently requires.
    pub required: String,
    /// The version the client tried to consent to.
    pub submitted: String,
}

/// A consent ready to persist: the accepted version and the domain-decided
/// acceptance instant.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConsentRecord {
    /// Version being consented to (always the required version).
    pub terms_version: String,
    /// Instant of acceptance as epoch millis, drawn from the injected clock.
    pub consented_at_millis: u64,
}

/// Decides whether a submitted consent may be recorded: only an exact match
/// with the required version is accepted, stamped with the injected `now`.
pub fn record_consent(
    submitted_version: &str,
    required_version: &str,
    now_millis: u64,
) -> Result<ConsentRecord, ConsentVersionMismatch> {
    if submitted_version == required_version {
        Ok(ConsentRecord {
            terms_version: submitted_version.to_owned(),
            consented_at_millis: now_millis,
        })
    } else {
        Err(ConsentVersionMismatch {
            required: required_version.to_owned(),
            submitted: submitted_version.to_owned(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn versions(list: &[&str]) -> Vec<String> {
        list.iter().map(|v| (*v).to_owned()).collect()
    }

    #[test]
    fn required_version_present_is_consented() {
        let consented = versions(&["2025-01-01", "2026-07-15"]);
        assert_eq!(
            consent_verdict(&consented, "2026-07-15"),
            ConsentVerdict::Consented
        );
    }

    #[test]
    fn no_recorded_consents_requires_consent() {
        assert_eq!(
            consent_verdict(&[], "2026-07-15"),
            ConsentVerdict::ConsentRequired
        );
    }

    #[test]
    fn older_versions_only_still_requires_consent() {
        let consented = versions(&["2025-01-01", "2025-06-30"]);
        assert_eq!(
            consent_verdict(&consented, "2026-07-15"),
            ConsentVerdict::ConsentRequired
        );
    }

    #[test]
    fn matching_version_records_with_the_injected_timestamp() {
        let record = record_consent("2026-07-15", "2026-07-15", 1_234_567)
            .expect("matching version is accepted");
        assert_eq!(
            record,
            ConsentRecord {
                terms_version: "2026-07-15".into(),
                consented_at_millis: 1_234_567,
            }
        );
    }

    #[test]
    fn mismatched_version_is_a_typed_refusal_never_a_record() {
        let err = record_consent("2025-01-01", "2026-07-15", 1_234_567)
            .expect_err("stale version must not record");
        assert_eq!(
            err,
            ConsentVersionMismatch {
                required: "2026-07-15".into(),
                submitted: "2025-01-01".into(),
            }
        );
    }
}
