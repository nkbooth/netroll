// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
//! `GET /healthz` answers from the database: 200 `ok` when it responds, 503
//! `unavailable` when it is refused or silent — and the silent case answers
//! inside a probe's own timeout, not after the pool's 30 s acquire default.

use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::body::Body;
use axum::http::{HeaderMap, Request, StatusCode, header};
use netroll_app::http::{AppState, api_router};
use netroll_domain::ports::{BoxFuture, MailError, Mailer};
use serde_json::Value;
use sqlx::PgPool;
use sqlx::postgres::PgPoolOptions;
use testcontainers_modules::postgres::Postgres;
use testcontainers_modules::testcontainers::runners::AsyncRunner;
use tokio::net::TcpListener;
use tower::ServiceExt;

/// Far below the pool's 30 s acquire default and above the handler's own
/// bound, so only a handler that bounds the ping itself passes.
const PROMPT: Duration = Duration::from_secs(10);

struct NullMailer;

impl Mailer for NullMailer {
    fn send_magic_link<'a>(
        &'a self,
        _: &'a str,
        _: &'a str,
    ) -> BoxFuture<'a, Result<(), MailError>> {
        Box::pin(async { Ok(()) })
    }
    fn send_email_change<'a>(
        &'a self,
        _: &'a str,
        _: &'a str,
    ) -> BoxFuture<'a, Result<(), MailError>> {
        Box::pin(async { Ok(()) })
    }
    fn send_email_change_notice<'a>(
        &'a self,
        _: &'a str,
        _: &'a str,
    ) -> BoxFuture<'a, Result<(), MailError>> {
        Box::pin(async { Ok(()) })
    }
}

async fn probe(pool: PgPool) -> (StatusCode, HeaderMap, Value, Duration) {
    let state = AppState::new(pool, Arc::new(NullMailer), "http://localhost".into());
    let started = Instant::now();
    let response = api_router(state)
        .oneshot(
            Request::builder()
                .uri("/healthz")
                .body(Body::empty())
                .expect("request builds"),
        )
        .await
        .expect("router answers");
    let elapsed = started.elapsed();
    let status = response.status();
    let headers = response.headers().clone();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("body reads");
    let body = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, headers, body, elapsed)
}

fn is_json(headers: &HeaderMap) -> bool {
    headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|ct| ct.starts_with("application/json"))
}

#[tokio::test]
async fn a_refused_database_answers_503_unavailable() {
    let refused = PgPoolOptions::new()
        .connect_lazy("postgres://x:x@127.0.0.1:1/x")
        .expect("a lazy pool parses its url without connecting");

    let (status, headers, body, elapsed) = probe(refused).await;

    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert!(is_json(&headers), "the 503 is JSON");
    assert_eq!(body["status"], "unavailable");
    assert!(elapsed < PROMPT, "answered in {elapsed:?}");
}

#[tokio::test]
async fn a_silent_database_answers_503_before_the_pool_gives_up() {
    // Accepts the TCP connection and never speaks, so the pool's own
    // acquire waits out its full default rather than failing fast.
    let black_hole = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let port = black_hole.local_addr().expect("addr").port();
    let held = tokio::spawn(async move {
        let mut sockets = Vec::new();
        while let Ok((socket, _)) = black_hole.accept().await {
            sockets.push(socket);
        }
    });
    let silent = PgPoolOptions::new()
        .connect_lazy(&format!("postgres://x:x@127.0.0.1:{port}/x"))
        .expect("a lazy pool parses its url without connecting");

    let (status, headers, body, elapsed) = probe(silent).await;
    held.abort();

    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert!(is_json(&headers), "the 503 is JSON");
    assert_eq!(body["status"], "unavailable");
    assert!(elapsed < PROMPT, "answered in {elapsed:?}");
}

#[tokio::test]
async fn a_live_database_answers_200_ok_uncached() {
    let container = Postgres::default().start().await.expect("start postgres");
    let host = container.get_host().await.expect("host");
    let port = container.get_host_port_ipv4(5432).await.expect("port");
    let pool = PgPoolOptions::new()
        .connect(&format!(
            "postgres://postgres:postgres@{host}:{port}/postgres"
        ))
        .await
        .expect("connect");

    let (status, headers, body, _) = probe(pool).await;

    assert_eq!(status, StatusCode::OK);
    assert!(is_json(&headers), "the 200 is JSON");
    assert_eq!(body["status"], "ok");
    assert_eq!(
        headers
            .get(header::CACHE_CONTROL)
            .and_then(|v| v.to_str().ok()),
        Some("no-store")
    );
}
