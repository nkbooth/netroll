// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
//! Public abuse-report submission: `POST /api/abuse-reports`.
//!
//! Unauthenticated, so it reuses the honeypot gate and an IP-keyed limiter. A
//! bot-shaped submission is dropped behind the same 202 a human gets and no id
//! is returned, so it is no enumeration oracle. Bodies never reach a log.

use axum::Router;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::routing::post;
use netroll_domain::bot_mitigation::BotVerdict;
use serde::Deserialize;

use super::problem::ApiError;
use super::{AppJson, AppState};

/// Upper bound on a report body — generous for a real complaint, but bounded so
/// an unbounded blob cannot flow into storage from the public write.
const MAX_REPORT_BODY_LEN: usize = 4_000;
/// Upper bound on the optional reporter contact string.
const MAX_REPORTER_CONTACT_LEN: usize = 254;
/// Upper bound on the optional context URL.
const MAX_CONTEXT_URL_LEN: usize = 2_048;

/// The public abuse-report route, merged OUTSIDE `require_session`.
pub fn public_abuse_report_routes() -> Router<AppState> {
    Router::new().route("/api/abuse-reports", post(create_abuse_report))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct AbuseReportRequest {
    body: String,
    #[serde(default)]
    reporter_contact: Option<String>,
    #[serde(default)]
    context_url: Option<String>,
    /// Bot-mitigation form token — mirrors the signup/net-create
    /// public writes. Absent when mitigation is off.
    #[serde(default)]
    form_token: Option<String>,
    /// Honeypot field — a hidden input a real user leaves empty.
    #[serde(default)]
    hp_field: Option<String>,
}

/// Derives the client-IP rate-limit key from the forwarded headers the tower
/// `SmartIpKeyExtractor` also honors (`X-Forwarded-For` / `X-Real-Ip`). This is
/// trustworthy because the app runs behind Caddy on the tailnet, not directly
/// internet-reachable (the `rate_limit` module's documented topology — the proxy
/// sets the header, and the app is never reached directly). A request with no
/// forwarded header shares a single `unknown` bucket, which in that topology
/// never happens in production. Testable via the bare `api_router`: a test sets
/// `X-Forwarded-For` to control the bucket.
///
/// Scans EVERY comma-separated hop within a header for the first non-blank
/// entry before falling through to the next header
/// name: a blank leading hop (`X-Forwarded-For: , 203.0.113.9`, which some
/// proxy chains produce) must not fall all the way to `X-Real-Ip` or
/// `unknown` when a real address is present later in the SAME header.
fn client_ip_key(headers: &HeaderMap) -> String {
    for header in ["x-forwarded-for", "x-real-ip"] {
        if let Some(value) = headers.get(header).and_then(|v| v.to_str().ok())
            && let Some(hop) = value.split(',').map(str::trim).find(|hop| !hop.is_empty())
        {
            return hop.to_owned();
        }
    }
    "unknown".to_owned()
}

/// Trims an optional field to `None` when blank, else validates its length.
fn bounded_optional(
    value: Option<String>,
    max: usize,
    field: &'static str,
) -> Result<Option<String>, ApiError> {
    match value {
        None => Ok(None),
        Some(s) if s.trim().is_empty() => Ok(None),
        Some(s) if s.trim().len() > max => Err(ApiError::validation(format!(
            "The {field} is longer than {max} characters — shorten it and try again."
        ))),
        Some(s) => Ok(Some(s.trim().to_owned())),
    }
}

async fn create_abuse_report(
    State(state): State<AppState>,
    headers: HeaderMap,
    AppJson(body): AppJson<AbuseReportRequest>,
) -> Result<StatusCode, ApiError> {
    // Rate-limit at the TOP, before any work — the same public-write spam
    // vector signup and net creation are hardened against. IP-keyed (the
    // reporter may have no account).
    let ip_key = client_ip_key(&headers);
    if let Err(retry_after_secs) = state.abuse_report_limiter.check(&ip_key) {
        return Err(ApiError::RateLimited { retry_after_secs });
    }

    // Validate BEFORE the bot check, so a malformed submission 400s identically
    // for bot and human (no distinguishable schema). Body is required + bounded.
    //
    // A PUBLIC, unauthenticated surface fronted by `ReportAbusePage.tsx`, so
    // its copy is user-facing — not admin-only, which the module doc above
    // contradicts. The three bounded rejections stated no bound, which is what
    // the reader needs in order to act. Whole-request sentence register: the
    // page renders one alert for the form, not a message beside a named input.
    let trimmed = body.body.trim();
    if trimmed.is_empty() {
        return Err(ApiError::validation(
            "The report is empty — describe what happened and try again.",
        ));
    }
    if trimmed.len() > MAX_REPORT_BODY_LEN {
        return Err(ApiError::validation(format!(
            "The report is longer than {MAX_REPORT_BODY_LEN} characters — shorten it and try again."
        )));
    }
    let report_body = trimmed.to_owned();
    let reporter_contact =
        bounded_optional(body.reporter_contact, MAX_REPORTER_CONTACT_LEN, "contact")?;
    let context_url = bounded_optional(body.context_url, MAX_CONTEXT_URL_LEN, "context link")?;

    let now = state.clock.now_epoch_millis();

    // Bot mitigation: a honeypot/timing failure returns the SAME
    // neutral 202 a legitimate report gets — recording NO row — so a bot cannot
    // distinguish a drop from success, and no enumeration oracle is introduced.
    // A no-op when mitigation is disabled.
    if state
        .bot_mitigation
        .verify(body.form_token.as_deref(), body.hp_field.as_deref(), now)
        == BotVerdict::Bot
    {
        // No PII/body in the log line; the reason stays vague.
        tracing::info!("abuse report dropped by bot mitigation");
        return Ok(StatusCode::ACCEPTED);
    }

    state
        .abuse_reports
        .record(
            &report_body,
            reporter_contact.as_deref(),
            context_url.as_deref(),
            now,
        )
        .await?;
    // Never reflect the free-text body/contact into logs.
    tracing::info!("abuse report recorded");

    // Neutral acknowledgement — no id returned (no confirmation oracle).
    Ok(StatusCode::ACCEPTED)
}
