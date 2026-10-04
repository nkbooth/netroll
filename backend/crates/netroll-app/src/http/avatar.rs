// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
//! Avatar upload and removal (`/api/accounts/me/avatar`).
//!
//! One file per account, referenced as a same-origin path. The image type comes
//! from SNIFFED MAGIC BYTES, never the filename or `Content-Type`: both are
//! attacker-controlled, and an HTML or SVG payload would be stored XSS.

use axum::Router;
use axum::extract::{Extension, Multipart, State};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use netroll_domain::avatar::{MAX_AVATAR_BYTES, is_stored_avatar_path, validate_avatar};
use netroll_domain::model::account::ProfileFields;
use netroll_domain::ports::{AvatarStore, AvatarStoreError, BoxFuture};

use super::{AppState, account_body};
use crate::http::problem::ApiError;
use crate::middleware::session::CurrentAccount;

/// The fail-closed default store installed by [`AppState::new`]: every write
/// reports unavailable, so an instance with no store configured still boots and
/// serves everything else while avatar upload answers 503. Mirrors the
/// no-KEK credential-cipher posture.
pub struct UnavailableAvatarStore;

impl AvatarStore for UnavailableAvatarStore {
    fn put<'a>(
        &'a self,
        _file_name: &'a str,
        _bytes: &'a [u8],
    ) -> BoxFuture<'a, Result<(), AvatarStoreError>> {
        Box::pin(async { Err(AvatarStoreError("no avatar store configured".into())) })
    }

    fn delete<'a>(&'a self, _file_name: &'a str) -> BoxFuture<'a, Result<(), AvatarStoreError>> {
        Box::pin(async { Err(AvatarStoreError("no avatar store configured".into())) })
    }
}

/// The session-gated avatar routes. Mounted under the authenticated group, so
/// `CurrentAccount` is always present.
pub fn avatar_routes() -> Router<AppState> {
    Router::new().route(
        "/api/accounts/me/avatar",
        post(upload_avatar).delete(remove_avatar),
    )
}

/// Reads the first file part of a multipart body, refusing anything past the
/// size ceiling WHILE streaming.
///
/// The cap is enforced chunk-by-chunk rather than on the assembled body: the
/// point of a limit is to not buffer a hostile 4 GB upload in memory first.
async fn read_upload(mut multipart: Multipart) -> Result<Vec<u8>, ApiError> {
    while let Some(mut field) = multipart
        .next_field()
        .await
        .map_err(|_| ApiError::validation("That upload was not something the server could read — choose the image again and retry."))?
    {
        // Any file part is accepted; the field name is a client detail and the
        // bytes are validated regardless of what it is called.
        if field.file_name().is_none() && field.name() != Some("file") {
            continue;
        }
        let mut bytes: Vec<u8> = Vec::new();
        while let Some(chunk) = field
            .chunk()
            .await
            .map_err(|_| ApiError::validation("That upload did not finish — choose the image again and retry."))?
        {
            if bytes.len() + chunk.len() > MAX_AVATAR_BYTES {
                return Err(ApiError::AvatarInvalid(
                    netroll_domain::avatar::AvatarError::TooLarge.to_string(),
                ));
            }
            bytes.extend_from_slice(&chunk);
        }
        return Ok(bytes);
    }
    Err(ApiError::validation(
        "No image was attached — choose a file and try again.",
    ))
}

/// `POST /api/accounts/me/avatar` — stores an uploaded avatar for the session's
/// account and points the profile at it.
///
/// Order matters: the file is written BEFORE the profile row is updated, so a
/// storage failure never leaves the account pointing at bytes that do not
/// exist. The reverse order could render a broken image for every viewer.
async fn upload_avatar(
    State(state): State<AppState>,
    Extension(current): Extension<CurrentAccount>,
    multipart: Multipart,
) -> Result<Response, ApiError> {
    let bytes = read_upload(multipart).await?;
    let account_id = current.account_id.to_string();
    let validated =
        validate_avatar(&account_id, &bytes).map_err(|e| ApiError::AvatarInvalid(e.to_string()))?;

    let account = state
        .accounts
        .find_by_id(current.account_id)
        .await?
        .ok_or(ApiError::Unauthenticated)?;

    state
        .avatar_store
        .put(&validated.file_name, &bytes)
        .await
        .map_err(|e| {
            // The path/IO detail is an operator diagnostic, never a response body.
            tracing::error!(account_id = %current.account_id, error = %e, "avatar write failed");
            ApiError::AvatarStorageUnavailable
        })?;

    // A different sniffed type means a different extension, so the previous
    // file is NOT the one just written and would otherwise linger forever.
    if let Some(previous) = account.avatar_url.as_deref()
        && is_stored_avatar_path(previous)
        && previous != validated.stored_path()
        && let Some(stale) = previous.strip_prefix(netroll_domain::avatar::AVATAR_PATH_PREFIX)
        && let Err(err) = state.avatar_store.delete(stale).await
    {
        // Not fatal: the new avatar is live and correct. A leaked file is an
        // operator cleanup problem, not a reason to fail the user's upload.
        tracing::warn!(account_id = %current.account_id, error = %err, "stale avatar not removed");
    }

    let fields = ProfileFields {
        display_name: account.display_name.clone(),
        location: account.location.clone(),
        grid: account.grid.clone(),
        avatar_url: Some(validated.stored_path()),
    };
    let now = state.clock.now_epoch_millis();
    state
        .accounts
        .update_profile(current.account_id, &fields, now)
        .await?;
    // Account id + type only — never the filename (it embeds the account id,
    // which is fine, but the bytes and any EXIF are the user's).
    tracing::info!(
        account_id = %current.account_id,
        image_type = validated.image_type.content_type(),
        "avatar uploaded"
    );

    let updated = state
        .accounts
        .find_by_id(current.account_id)
        .await?
        .ok_or(ApiError::Unauthenticated)?;
    Ok(axum::Json(account_body(&state, &updated).await?).into_response())
}

/// `DELETE /api/accounts/me/avatar` — removes an uploaded avatar, falling the
/// account back to its Gravatar.
///
/// Only clears the profile reference when it points at a file THIS instance
/// stores: an account whose avatar is an external `https://` URL has nothing
/// here to delete, and silently blanking their URL would be a surprise.
async fn remove_avatar(
    State(state): State<AppState>,
    Extension(current): Extension<CurrentAccount>,
) -> Result<Response, ApiError> {
    let account = state
        .accounts
        .find_by_id(current.account_id)
        .await?
        .ok_or(ApiError::Unauthenticated)?;

    let stored = account
        .avatar_url
        .as_deref()
        .filter(|url| is_stored_avatar_path(url));

    if let Some(path) = stored {
        if let Some(file_name) = path.strip_prefix(netroll_domain::avatar::AVATAR_PATH_PREFIX)
            && let Err(err) = state.avatar_store.delete(file_name).await
        {
            tracing::warn!(account_id = %current.account_id, error = %err, "avatar file not removed");
        }
        let fields = ProfileFields {
            display_name: account.display_name.clone(),
            location: account.location.clone(),
            grid: account.grid.clone(),
            avatar_url: None,
        };
        let now = state.clock.now_epoch_millis();
        state
            .accounts
            .update_profile(current.account_id, &fields, now)
            .await?;
        tracing::info!(account_id = %current.account_id, "avatar removed");
    }

    let updated = state
        .accounts
        .find_by_id(current.account_id)
        .await?
        .ok_or(ApiError::Unauthenticated)?;
    Ok(axum::Json(account_body(&state, &updated).await?).into_response())
}
