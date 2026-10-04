// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
//! `GET /healthz`: the liveness probe the container and the deploy poll.
//! Public through the proxy, so the body is one of two fixed shapes and
//! carries no version, build or database detail.

use std::time::Duration;

use axum::Json;
use axum::Router;
use axum::extract::State;
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use serde::Serialize;

use super::AppState;

/// A probe must hear back before its own timeout, and the pool's acquire
/// default is 30 s: unbounded, a silent database reads as a hung app.
const PING_TIMEOUT: Duration = Duration::from_secs(2);

#[derive(Serialize)]
struct HealthBody {
    status: &'static str,
}

/// The probe route. Mounted outside every governor and the session layer:
/// a liveness probe refused with 429 would restart a healthy container, and
/// one answer costs a single pooled `SELECT 1`.
pub fn health_routes() -> Router<AppState> {
    Router::new().route("/healthz", get(healthz))
}

async fn healthz(State(state): State<AppState>) -> Response {
    let reachable = matches!(
        tokio::time::timeout(PING_TIMEOUT, state.health.ping()).await,
        Ok(Ok(()))
    );
    let (status, body) = if reachable {
        (StatusCode::OK, HealthBody { status: "ok" })
    } else {
        (
            StatusCode::SERVICE_UNAVAILABLE,
            HealthBody {
                status: "unavailable",
            },
        )
    };
    (status, [(header::CACHE_CONTROL, "no-store")], Json(body)).into_response()
}
