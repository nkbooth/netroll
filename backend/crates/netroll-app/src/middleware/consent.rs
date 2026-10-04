// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
//! The consent enforcement seam: an extractor a gated action declares as a
//! parameter, layered on `require_session`.
//!
//! Consent is read per-request from Postgres, so revocation takes effect at
//! once. It is never cached in the session row or a cookie.

use axum::extract::FromRequestParts;
use axum::http::request::Parts;
use netroll_domain::consent::{CURRENT_TERMS_VERSION, ConsentVerdict, consent_verdict};

use super::session::CurrentAccount;
use crate::http::AppState;
use crate::http::problem::ApiError;

/// The authenticated AND consented account for this request. Extraction
/// fails with `/errors/consent-required` (403) until consent to
/// [`CURRENT_TERMS_VERSION`] is recorded.
#[derive(Debug, Clone, Copy)]
pub struct ConsentedAccount(pub CurrentAccount);

impl FromRequestParts<AppState> for ConsentedAccount {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> Result<Self, ApiError> {
        // Session middleware runs first; a missing extension means the
        // route forgot `require_session`, which must read as unauthenticated,
        // never as consented.
        let current = parts
            .extensions
            .get::<CurrentAccount>()
            .copied()
            .ok_or(ApiError::Unauthenticated)?;

        let versions = state
            .consents
            .consented_versions(current.account_id)
            .await?;
        match consent_verdict(&versions, CURRENT_TERMS_VERSION) {
            ConsentVerdict::Consented => Ok(Self(current)),
            ConsentVerdict::ConsentRequired => Err(ApiError::ConsentRequired),
        }
    }
}
