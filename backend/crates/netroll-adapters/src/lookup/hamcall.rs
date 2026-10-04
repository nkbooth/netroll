// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
//! The US-only FCC-ULS JSON fallback: `GET https://hamcall.dev/<CALL>.json`,
//! unauthenticated. A 200 is a hit, a 404 a definitive miss, and anything else
//! degrades to [`LookupError::Unavailable`] so the service falls through. All
//! I/O goes through the shared SSRF-safe [`Egress`].

use std::sync::Arc;

use netroll_domain::egress::{Egress, EgressMethod, EgressRequest};
use netroll_domain::lookup::{CallsignRecord, LookupError, LookupSource, join_fields, normalize};
use netroll_domain::ports::{BoxFuture, LookupProvider};
use netroll_domain::qrz::QrzCredentials;
use serde::Deserialize;
use url::Url;

use super::egress_error_to_lookup_error;

/// The hamcall.dev callbook fallback provider.
pub struct HamcallLookupProvider {
    egress: Arc<dyn Egress + Send + Sync>,
}

/// The subset of the hamcall.dev JSON record NetRoll autofill needs. Every
/// field is `#[serde(default)]` optional so a missing key never fails the
/// parse, and unknown fields are ignored (serde's default) — the same
/// tolerant-of-extra-fields posture the QRZ XML parser keeps.
#[derive(Deserialize, Default)]
struct HamcallDto {
    #[serde(default)]
    callsign: Option<String>,
    #[serde(default)]
    first_name: Option<String>,
    #[serde(default)]
    last_name: Option<String>,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    city: Option<String>,
    #[serde(default)]
    state: Option<String>,
}

impl HamcallLookupProvider {
    /// Wires the provider onto the shared egress client.
    pub fn new(egress: Arc<dyn Egress + Send + Sync>) -> Self {
        Self { egress }
    }
}

/// Builds `https://hamcall.dev/<CALL>.json` with the callsign uppercased and
/// percent-encoded as a single path segment (a portable-indicator `/` in a
/// callsign must not split the path).
fn build_url(callsign: &str) -> Result<String, LookupError> {
    let mut url = Url::parse("https://hamcall.dev/").map_err(|_| LookupError::Unavailable)?;
    url.path_segments_mut()
        .map_err(|()| LookupError::Unavailable)?
        .push(&format!("{}.json", callsign.to_uppercase()));
    Ok(url.into())
}

/// Projects a decoded hamcall record onto the domain [`CallsignRecord`].
fn to_record(dto: &HamcallDto, queried: &str) -> CallsignRecord {
    CallsignRecord {
        callsign: normalize(dto.callsign.as_deref()).unwrap_or_else(|| queried.to_uppercase()),
        name: normalize(dto.name.as_deref())
            .or_else(|| join_fields(dto.first_name.as_deref(), dto.last_name.as_deref(), " ")),
        location: join_fields(dto.city.as_deref(), dto.state.as_deref(), ", "),
        grid: None,
        source: LookupSource::Hamcall,
    }
}

impl LookupProvider for HamcallLookupProvider {
    fn lookup<'a>(
        &'a self,
        callsign: &'a str,
        _credentials: Option<&'a QrzCredentials>,
    ) -> BoxFuture<'a, Result<Option<CallsignRecord>, LookupError>> {
        Box::pin(async move {
            let url = build_url(callsign)?;
            let response = self
                .egress
                .send(EgressRequest {
                    method: EgressMethod::Get,
                    url,
                    headers: Vec::new(),
                    body: None,
                })
                .await
                .map_err(|err| egress_error_to_lookup_error(&err))?;

            // 404 is a definitive miss; any other non-2xx is a provider failure.
            if response.status == 404 {
                return Ok(None);
            }
            if !(200..300).contains(&response.status) {
                return Err(LookupError::Unavailable);
            }

            // A 200 means the callsign is in the FCC extract — a hit. A body
            // that does not decode into the record shape (e.g. an empty/`null`
            // payload) carries no usable data, so treat it as a miss rather than
            // an error.
            match serde_json::from_slice::<HamcallDto>(&response.body) {
                Ok(dto) => Ok(Some(to_record(&dto, callsign))),
                Err(_) => Ok(None),
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::super::test_support::{FakeEgress, Reply};
    use super::*;
    use netroll_domain::egress::EgressError;

    const AA7BQ_JSON: &str = r#"{
        "callsign": "AA7BQ",
        "first_name": "FRED",
        "last_name": "LLOYD",
        "city": "SCOTTSDALE",
        "state": "AZ",
        "class": "E",
        "frn": "0001234567"
    }"#;

    #[tokio::test]
    async fn a_200_maps_the_json_record_and_hits_the_expected_url() {
        let egress = Arc::new(FakeEgress::ok_body(200, AA7BQ_JSON));
        let provider = HamcallLookupProvider::new(egress.clone());

        let record = provider
            .lookup("aa7bq", None)
            .await
            .expect("no error")
            .expect("a hit");

        assert_eq!(record.callsign, "AA7BQ");
        assert_eq!(record.name.as_deref(), Some("FRED LLOYD"));
        assert_eq!(record.location.as_deref(), Some("SCOTTSDALE, AZ"));
        assert_eq!(record.grid, None);
        assert_eq!(record.source, LookupSource::Hamcall);

        // Exactly one outbound GET, to the uppercased .json URL.
        let requests = egress.requests();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].method, EgressMethod::Get);
        assert_eq!(requests[0].url, "https://hamcall.dev/AA7BQ.json");
    }

    #[tokio::test]
    async fn prefers_the_combined_name_field_when_present() {
        let json =
            r#"{"callsign":"W1AW","name":"HIRAM PERCY MAXIM","city":"NEWINGTON","state":"CT"}"#;
        let provider = HamcallLookupProvider::new(Arc::new(FakeEgress::ok_body(200, json)));

        let record = provider.lookup("W1AW", None).await.unwrap().unwrap();

        assert_eq!(record.name.as_deref(), Some("HIRAM PERCY MAXIM"));
    }

    #[tokio::test]
    async fn a_404_is_a_definitive_miss() {
        let provider = HamcallLookupProvider::new(Arc::new(FakeEgress::ok_body(404, "not found")));

        let result = provider.lookup("ZZ9ZZ", None).await.expect("no error");

        assert_eq!(result, None);
    }

    #[tokio::test]
    async fn a_transport_error_degrades_to_unavailable() {
        let egress = Arc::new(FakeEgress::new(vec![Reply::Err(EgressError::Timeout)]));
        let provider = HamcallLookupProvider::new(egress);

        let err = provider
            .lookup("W1AW", None)
            .await
            .expect_err("a timeout must surface as a provider failure");

        assert_eq!(err, LookupError::Unavailable);
    }

    #[tokio::test]
    async fn a_500_degrades_to_unavailable() {
        let provider = HamcallLookupProvider::new(Arc::new(FakeEgress::ok_body(500, "boom")));

        let err = provider
            .lookup("W1AW", None)
            .await
            .expect_err("5xx is a failure");

        assert_eq!(err, LookupError::Unavailable);
    }
}
