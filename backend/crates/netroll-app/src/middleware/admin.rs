// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
//! The platform-admin authorization gate, layered on `require_session`.
//!
//! The only admin-privilege gate in the codebase. Admin status is a pure
//! [`is_admin`] decision over the boot-configured allowlist — a separate
//! authorization axis from the per-net role hierarchy.

use axum::extract::FromRequestParts;
use axum::http::request::Parts;
use netroll_domain::admin::is_admin;

use super::session::CurrentAccount;
use crate::http::AppState;
use crate::http::problem::ApiError;

/// The authenticated AND platform-admin account for this request. Extraction
/// resolves the current account (via the session), then checks admin status:
/// a missing session reads as `Unauthenticated` (401); a signed-in non-admin is
/// `Forbidden` (403) — the codebase's established 401-before-403 ordering, the
/// same shape `ConsentedAccount` uses.
#[derive(Debug, Clone, Copy)]
pub struct AdminAccount(pub CurrentAccount);

impl FromRequestParts<AppState> for AdminAccount {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> Result<Self, ApiError> {
        // Session middleware runs first; a missing extension means the route
        // forgot `require_session`, which must read as unauthenticated (401),
        // never as authorized.
        let current = parts
            .extensions
            .get::<CurrentAccount>()
            .copied()
            .ok_or(ApiError::Unauthenticated)?;

        // The account must still exist to resolve its email; a vanished account
        // reads as unauthenticated, the `get_me` convention.
        let account = state
            .accounts
            .find_by_id(current.account_id)
            .await?
            .ok_or(ApiError::Unauthenticated)?;

        if is_admin(&account.email, &state.admin_allowlist) {
            Ok(Self(current))
        } else {
            // A signed-in non-admin: 403, never a 404/enumeration signal.
            Err(ApiError::Forbidden)
        }
    }
}
