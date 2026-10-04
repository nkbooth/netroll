// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
//! The signed-in caller's own personal data as one JSON document.
//!
//! Session-gated but not consent-gated: exporting your own data is a right you
//! keep while refusing the terms. Write-only secrets never appear in any form —
//! each surfaces as an existence boolean, never as a value.

use axum::extract::State;
use axum::http::header;
use axum::response::{IntoResponse, Response};
use axum::{Extension, Json};
use serde::Serialize;
use uuid::Uuid;

use crate::middleware::session::CurrentAccount;

use super::favorites::{FavoriteNetBody, favorite_net_body};
use super::problem::ApiError;
use super::{AppState, rfc3339};

/// The whole export document (camelCase). Nests the several unrelated
/// personal-data categories a flat CSV/ADIF could not.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ExportBody {
    /// RFC 3339 instant the export was produced.
    exported_at: String,
    account: ExportAccount,
    favorites: Vec<FavoriteNetBody>,
    owned_net_delivery_configs: Vec<OwnedNetDeliveryConfig>,
    self_check_ins: Vec<SelfCheckInBody>,
}

/// The account's own profile block. `qrzCredentialsConfigured` is the ONLY QRZ
/// signal — never the stored callsign/password, plaintext or ciphertext.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ExportAccount {
    id: Uuid,
    email: String,
    email_verified_at: Option<String>,
    callsign: Option<String>,
    display_name: Option<String>,
    location: Option<String>,
    grid: Option<String>,
    avatar_url: Option<String>,
    /// Whether QRZ credentials are stored — boolean-only, matching their
    /// write-only posture. Never the credential itself.
    qrz_credentials_configured: bool,
}

/// One owned net's delivery config. Both `*Configured` flags are boolean-only —
/// neither the webhook secret nor the Discord webhook URL is ever carried.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct OwnedNetDeliveryConfig {
    net_definition_id: Uuid,
    net_title: String,
    delivery_emails: Vec<String>,
    /// Whether a webhook (and thus its HMAC secret) is configured — never the
    /// secret or the URL.
    webhook_configured: bool,
    /// Whether a Discord announcement destination is configured — never the
    /// URL. A configured integration invisible to the data-rights surface would
    /// be a real gap, so its existence is reported.
    ///
    /// **Derived from the URL, not from a secret, and deliberately NOT the same
    /// expression as its sibling.** `webhook_configured` reads
    /// `webhook_secret_set` because a generic webhook always mints an HMAC
    /// secret; Discord mints none, so a net whose only destination is Discord
    /// has `webhook_secret_set == false` and copying the neighbouring expression
    /// would report a Discord-only net as unconfigured.
    discord_configured: bool,
}

/// One self-check-in in the account's history.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct SelfCheckInBody {
    net_session_id: Uuid,
    net_title: String,
    callsign: String,
    signal_report: Option<String>,
    staying: &'static str,
    name: Option<String>,
    location: Option<String>,
    /// WHICH way in the account came in on, as its LABEL, or `null`
    /// when nobody recorded one. Personal data the account holder logged about
    /// itself, so it belongs in its own copy of its own data — this export is
    /// the surface that already tells the truth about check-ins the profile
    /// widget cannot render.
    via: Option<String>,
    checked_in_at: String,
}

/// `GET /api/accounts/me/export` — the caller's own personal-data export.
/// Session-gated (`CurrentAccount`), NOT consent-gated. Composes several
/// per-account-bounded repo reads into one JSON document and serves it as an
/// attachment (so the frontend's plain `<a download href>` works identically to
/// session export). The only failure mode is the existing 401.
pub async fn export_account_data(
    State(state): State<AppState>,
    Extension(current): Extension<CurrentAccount>,
) -> Result<Response, ApiError> {
    // Account no longer exists ⇒ 401 (the `get_me`/`delete_account` convention).
    let account = state
        .accounts
        .find_by_id(current.account_id)
        .await?
        .ok_or(ApiError::Unauthenticated)?;

    // Existence booleans only — reading existence is not decryption.
    let qrz_credentials_configured = state.qrz_credentials.is_set(account.id).await?;

    let favorites = state
        .favorites
        .list_for_account(account.id)
        .await?
        .into_iter()
        .map(favorite_net_body)
        .collect();

    // Bounded per-net delivery-config reads over the account's own active owned
    // nets (an accepted small N+1 — no per-net owner cap exists yet, the same
    // class already accepted for the session export). Uses the redacted `get()` ONLY.
    let owned = state
        .net_definitions
        .owned_active_definitions(account.id)
        .await?;
    let mut owned_net_delivery_configs = Vec::with_capacity(owned.len());
    for (net_definition_id, net_title) in owned {
        let config = state.delivery_configs.get(net_definition_id).await?;
        let (delivery_emails, webhook_configured, discord_configured) = match config {
            Some(config) => (
                config.emails,
                config.webhook_secret_set,
                config.discord_webhook_url.is_some(),
            ),
            None => (Vec::new(), false, false),
        };
        owned_net_delivery_configs.push(OwnedNetDeliveryConfig {
            net_definition_id,
            net_title,
            delivery_emails,
            webhook_configured,
            discord_configured,
        });
    }

    let self_check_ins = state
        .session_events
        .self_check_ins(account.id)
        .await?
        .into_iter()
        .map(|entry| SelfCheckInBody {
            net_session_id: entry.session_id,
            net_title: entry.net_title,
            callsign: entry.callsign.as_str().to_owned(),
            signal_report: entry.signal_report.as_ref().map(|r| r.as_str().to_owned()),
            staying: entry.staying.as_str(),
            name: entry.name.as_ref().map(|n| n.as_str().to_owned()),
            location: entry.location.as_ref().map(|l| l.as_str().to_owned()),
            via: entry.via,
            checked_in_at: rfc3339(entry.checked_in_at_millis),
        })
        .collect();

    let body = ExportBody {
        exported_at: rfc3339(state.clock.now_epoch_millis()),
        account: ExportAccount {
            id: account.id,
            email: account.email.clone(),
            email_verified_at: account.email_verified_at_millis.map(rfc3339),
            callsign: account.callsign.clone(),
            display_name: account.display_name.clone(),
            location: account.location.clone(),
            grid: account.grid.clone(),
            avatar_url: account.avatar_url.clone(),
            qrz_credentials_configured,
        },
        favorites,
        owned_net_delivery_configs,
        self_check_ins,
    };

    // A static, non-user-controlled filename — no header-injection surface.
    Ok((
        [(
            header::CONTENT_DISPOSITION,
            "attachment; filename=\"netroll-data-export.json\"",
        )],
        Json(body),
    )
        .into_response())
}
