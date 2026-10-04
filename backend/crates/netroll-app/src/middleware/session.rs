// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
//! Session-cookie authentication: extracts the cookie, applies the domain
//! session verdict, and injects the authenticated account as an extension.

use axum::extract::{Request, State};
use axum::middleware::Next;
use axum::response::Response;
use axum_extra::extract::cookie::CookieJar;
use netroll_domain::auth::{SESSION_IDLE_MILLIS, SessionVerdict, session_verdict};
use uuid::Uuid;

use crate::http::problem::ApiError;
use crate::http::{AppState, SESSION_COOKIE, tokens};

/// The authenticated account for this request, injected by
/// [`require_session`].
#[derive(Debug, Clone, Copy)]
pub struct CurrentAccount {
    /// Account the session belongs to.
    pub account_id: Uuid,
    /// Hash of the presented session token (lets sign-out revoke exactly
    /// this session without re-reading the cookie).
    pub token_hash: [u8; 32],
}

/// Idle-cursor writes are throttled: `last_seen_at` is only persisted when
/// more than this stale, keeping authenticated reads write-free.
const TOUCH_THROTTLE_MILLIS: u64 = 60_000;

/// Rejects the request with `/errors/unauthenticated` unless a live session
/// cookie is presented; refreshes the sliding idle cursor.
pub async fn require_session(
    State(state): State<AppState>,
    jar: CookieJar,
    mut request: Request,
    next: Next,
) -> Result<Response, ApiError> {
    let cookie = jar.get(SESSION_COOKIE).ok_or(ApiError::Unauthenticated)?;
    let hash = tokens::decode_to_hash(cookie.value()).ok_or(ApiError::Unauthenticated)?;

    let row = state
        .sessions
        .find(hash)
        .await?
        .ok_or(ApiError::Unauthenticated)?;
    let now = state.clock.now_epoch_millis();
    if session_verdict(&row.state, SESSION_IDLE_MILLIS, now) == SessionVerdict::Rejected {
        return Err(ApiError::Unauthenticated);
    }

    // A disabled account is refused even if a session row
    // somehow survived the disable-time bulk revoke — belt-and-suspenders on the
    // authenticated path. One cheap PK-indexed read (documented added query);
    // distinctly 403 `/errors/account-disabled`, never a silent pass into a
    // broken state.
    if state.accounts.is_disabled(row.account_id).await? {
        return Err(ApiError::AccountDisabled);
    }

    if now.saturating_sub(row.state.last_seen_at_millis) > TOUCH_THROTTLE_MILLIS {
        state.sessions.touch(hash, now).await?;
    }

    request.extensions_mut().insert(CurrentAccount {
        account_id: row.account_id,
        token_hash: hash,
    });
    Ok(next.run(request).await)
}
