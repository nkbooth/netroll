// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
//! Full-loop API tests for the public abuse-report endpoint:
//! real router, real Postgres (testcontainers), capturing fake mailer. Asserts
//! status codes and DB side-effects (the recorded ROW), never response prose
//! (house TDD rule). Reuses the bot-mitigation + IP-limiter hardening.

use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use netroll_app::bot_mitigation::BotMitigation;
use netroll_app::config::resolve_bot_mitigation_secret;
use netroll_app::http::{AppState, api_router};
use netroll_domain::bot_mitigation::issue_form_token;
use netroll_domain::ports::{BoxFuture, MailError, Mailer};
use serde_json::{Value, json};
use sqlx::PgPool;
use sqlx::postgres::PgPoolOptions;
use testcontainers_modules::postgres::Postgres;
use testcontainers_modules::testcontainers::ContainerAsync;
use testcontainers_modules::testcontainers::runners::AsyncRunner;
use tower::ServiceExt;

const BOT_SECRET_RAW: &str = "test-bot-mitigation-secret";
const BOT_SECRET_BYTES: &[u8] = b"test-bot-mitigation-secret";

#[derive(Default)]
struct NoopMailer;

impl Mailer for NoopMailer {
    fn send_magic_link<'a>(
        &'a self,
        _to: &'a str,
        _link: &'a str,
    ) -> BoxFuture<'a, Result<(), MailError>> {
        Box::pin(async { Ok(()) })
    }
    fn send_email_change<'a>(
        &'a self,
        _to: &'a str,
        _link: &'a str,
    ) -> BoxFuture<'a, Result<(), MailError>> {
        Box::pin(async { Ok(()) })
    }
    fn send_email_change_notice<'a>(
        &'a self,
        _to: &'a str,
        _n: &'a str,
    ) -> BoxFuture<'a, Result<(), MailError>> {
        Box::pin(async { Ok(()) })
    }
}

struct TestApp {
    _container: ContainerAsync<Postgres>,
    state: AppState,
    pool: PgPool,
}

impl TestApp {
    fn router(&self) -> Router {
        api_router(self.state.clone())
    }
    async fn report_rows(&self) -> i64 {
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM abuse_reports")
            .fetch_one(&self.pool)
            .await
            .expect("count reports")
    }
}

async fn test_app(customize: impl FnOnce(AppState) -> AppState) -> TestApp {
    let container = Postgres::default().start().await.expect("start postgres");
    let host = container.get_host().await.expect("host");
    let port = container.get_host_port_ipv4(5432).await.expect("port");
    let pool = PgPoolOptions::new()
        .connect(&format!(
            "postgres://postgres:postgres@{host}:{port}/postgres"
        ))
        .await
        .expect("connect");
    sqlx::migrate!("../../migrations")
        .run(&pool)
        .await
        .expect("migrate");
    let state = customize(AppState::new(
        pool.clone(),
        Arc::new(NoopMailer),
        "http://localhost:5173".into(),
    ));
    TestApp {
        _container: container,
        state,
        pool,
    }
}

/// POSTs JSON with an optional `X-Forwarded-For` (the IP rate-limit key).
async fn post_report(router: Router, body: Value, forwarded_for: Option<&str>) -> StatusCode {
    let mut builder = Request::builder()
        .method("POST")
        .uri("/api/abuse-reports")
        .header(header::CONTENT_TYPE, "application/json");
    if let Some(ip) = forwarded_for {
        builder = builder.header("x-forwarded-for", ip);
    }
    let request = builder.body(Body::from(body.to_string())).expect("build");
    router.oneshot(request).await.expect("route").status()
}

fn wall_now_millis() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("after epoch")
        .as_millis() as u64
}

fn human_form_token() -> String {
    issue_form_token(BOT_SECRET_BYTES, wall_now_millis() - 5_000)
}

#[tokio::test]
async fn a_valid_report_is_recorded_as_a_row() {
    let app = test_app(|s| s).await;
    let status = post_report(
        app.router(),
        json!({ "body": "someone is spamming net titles", "reporterContact": "W1RPT" }),
        Some("203.0.113.1"),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED);
    assert_eq!(app.report_rows().await, 1, "the report persists as a row");
}

#[tokio::test]
async fn an_empty_or_overlong_body_is_rejected_and_writes_no_row() {
    let app = test_app(|s| s).await;

    let empty = post_report(app.router(), json!({ "body": "   " }), Some("203.0.113.2")).await;
    assert_eq!(empty, StatusCode::BAD_REQUEST);

    let overlong = post_report(
        app.router(),
        json!({ "body": "x".repeat(5_000) }),
        Some("203.0.113.2"),
    )
    .await;
    assert_eq!(overlong, StatusCode::BAD_REQUEST);

    assert_eq!(
        app.report_rows().await,
        0,
        "a rejected report writes no row"
    );
}

#[tokio::test]
async fn a_filled_honeypot_is_dropped_with_a_neutral_202_and_no_row() {
    let app = test_app(|s| {
        let secret = resolve_bot_mitigation_secret(Some(BOT_SECRET_RAW.into()))
            .expect("resolves")
            .expect("enabled");
        s.with_bot_mitigation(BotMitigation::with_secret(secret))
    })
    .await;

    let status = post_report(
        app.router(),
        json!({ "body": "spam", "formToken": human_form_token(), "hpField": "http://bot.example" }),
        Some("203.0.113.3"),
    )
    .await;
    // Neutral, success-shaped 202 (no oracle), but NO row recorded.
    assert_eq!(status, StatusCode::ACCEPTED);
    assert_eq!(
        app.report_rows().await,
        0,
        "a bot-dropped report writes no row"
    );
}

#[tokio::test]
async fn a_valid_human_report_passes_bot_mitigation_and_records() {
    let app = test_app(|s| {
        let secret = resolve_bot_mitigation_secret(Some(BOT_SECRET_RAW.into()))
            .expect("resolves")
            .expect("enabled");
        s.with_bot_mitigation(BotMitigation::with_secret(secret))
    })
    .await;

    let status = post_report(
        app.router(),
        json!({ "body": "a genuine report", "formToken": human_form_token(), "hpField": "" }),
        Some("203.0.113.4"),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED);
    assert_eq!(app.report_rows().await, 1, "a human report persists");
}

#[tokio::test]
async fn reports_over_the_ip_rate_limit_are_429_and_a_different_ip_is_unaffected() {
    let app = test_app(|s| s).await;
    let noisy = "203.0.113.9";

    // The burst of 5 from one IP passes.
    for n in 0..5 {
        let status = post_report(
            app.router(),
            json!({ "body": format!("report {n}") }),
            Some(noisy),
        )
        .await;
        assert_eq!(status, StatusCode::ACCEPTED, "report {n} within the burst");
    }
    // The 6th from the SAME IP is throttled.
    let throttled = post_report(app.router(), json!({ "body": "over" }), Some(noisy)).await;
    assert_eq!(throttled, StatusCode::TOO_MANY_REQUESTS);

    // A different IP has its own bucket.
    let other = post_report(
        app.router(),
        json!({ "body": "from elsewhere" }),
        Some("198.51.100.1"),
    )
    .await;
    assert_eq!(other, StatusCode::ACCEPTED, "a different IP is unaffected");
}

#[tokio::test]
async fn a_blank_leading_hop_in_x_forwarded_for_still_keys_on_the_real_ip() {
    // Some proxy chains produce a blank leading hop
    // (`X-Forwarded-For: , 203.0.113.9`). The real address later in the SAME
    // header must still be found and given its OWN bucket — not fall through
    // to the shared `unknown` bucket, which would let this traffic exhaust a
    // bucket other, genuinely-headerless requests also share.
    let app = test_app(|s| s).await;
    let messy = ", 203.0.113.9";

    for n in 0..5 {
        let status = post_report(
            app.router(),
            json!({ "body": format!("report {n}") }),
            Some(messy),
        )
        .await;
        assert_eq!(status, StatusCode::ACCEPTED, "report {n} within the burst");
    }
    let throttled = post_report(app.router(), json!({ "body": "over" }), Some(messy)).await;
    assert_eq!(
        throttled,
        StatusCode::TOO_MANY_REQUESTS,
        "the real trailing IP has its own bucket, keyed correctly"
    );

    // A genuinely headerless request (the `unknown` bucket) is unaffected —
    // proving the messy header above was NOT keyed as `unknown` too.
    let headerless = post_report(app.router(), json!({ "body": "no header at all" }), None).await;
    assert_eq!(
        headerless,
        StatusCode::ACCEPTED,
        "the unknown bucket was never touched by the messy-header traffic"
    );
}
