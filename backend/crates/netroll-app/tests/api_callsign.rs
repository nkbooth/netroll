// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
//! Full-loop API tests for callsign reservation & change: real
//! router, real Postgres (testcontainers), capturing fake mailer. This
//! route is the first production consumer of the `ConsentedAccount` guard.
//! Asserts status codes and problem+json `type` slugs, never
//! message strings (house TDD rule).

use std::sync::{Arc, Mutex};

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use netroll_app::http::{AppState, api_router};
use netroll_domain::auth::hash_token;
use netroll_domain::consent::{CURRENT_TERMS_VERSION, ConsentRecord};
use netroll_domain::ports::{BoxFuture, MailError, Mailer};
use serde_json::{Value, json};
use sqlx::PgPool;
use sqlx::postgres::PgPoolOptions;
use testcontainers_modules::postgres::Postgres;
use testcontainers_modules::testcontainers::ContainerAsync;
use testcontainers_modules::testcontainers::runners::AsyncRunner;
use tower::ServiceExt;
use uuid::Uuid;

/// Mirrors the private constant in `netroll_app::http` — the wire cookie
/// name is a fixed contract, not an implementation detail worth exposing
/// just for tests.
const SESSION_COOKIE: &str = "__Host-netroll-session";

/// Mailer that records sends instead of speaking SMTP.
#[derive(Default)]
struct CapturingMailer {
    sent: Mutex<Vec<(String, String)>>,
}

impl CapturingMailer {
    fn last_link(&self) -> String {
        self.sent
            .lock()
            .expect("mailer lock")
            .last()
            .expect("at least one mail sent")
            .1
            .clone()
    }
}

impl Mailer for CapturingMailer {
    fn send_magic_link<'a>(
        &'a self,
        to: &'a str,
        link: &'a str,
    ) -> BoxFuture<'a, Result<(), MailError>> {
        Box::pin(async move {
            self.sent
                .lock()
                .expect("mailer lock")
                .push((to.to_owned(), link.to_owned()));
            Ok(())
        })
    }

    fn send_email_change<'a>(
        &'a self,
        to: &'a str,
        link: &'a str,
    ) -> BoxFuture<'a, Result<(), MailError>> {
        Box::pin(async move {
            self.sent
                .lock()
                .expect("mailer lock")
                .push((to.to_owned(), link.to_owned()));
            Ok(())
        })
    }

    fn send_email_change_notice<'a>(
        &'a self,
        to: &'a str,
        new_email: &'a str,
    ) -> BoxFuture<'a, Result<(), MailError>> {
        Box::pin(async move {
            self.sent
                .lock()
                .expect("mailer lock")
                .push((to.to_owned(), new_email.to_owned()));
            Ok(())
        })
    }
}

struct TestApp {
    _container: ContainerAsync<Postgres>,
    state: AppState,
    pool: PgPool,
    mailer: Arc<CapturingMailer>,
}

impl TestApp {
    fn router(&self) -> Router {
        api_router(self.state.clone())
    }
}

async fn test_app() -> TestApp {
    let container = Postgres::default()
        .start()
        .await
        .expect("start postgres container");
    let host = container.get_host().await.expect("resolve container host");
    let port = container
        .get_host_port_ipv4(5432)
        .await
        .expect("resolve mapped postgres port");
    let pool = PgPoolOptions::new()
        .connect(&format!(
            "postgres://postgres:postgres@{host}:{port}/postgres"
        ))
        .await
        .expect("connect to containerized postgres");
    sqlx::migrate!("../../migrations")
        .run(&pool)
        .await
        .expect("run migrations");

    let mailer = Arc::new(CapturingMailer::default());
    let state = AppState::new(pool.clone(), mailer.clone(), "http://localhost:5173".into());
    TestApp {
        _container: container,
        state,
        pool,
        mailer,
    }
}

async fn send_json(
    router: Router,
    method: &str,
    uri: &str,
    body: Option<Value>,
    cookie: Option<&str>,
) -> (StatusCode, Option<String>, Value) {
    let mut builder = Request::builder().method(method).uri(uri);
    if body.is_some() {
        builder = builder.header(header::CONTENT_TYPE, "application/json");
    }
    if let Some(cookie) = cookie {
        builder = builder.header(header::COOKIE, cookie);
    }
    let request = builder
        .body(match body {
            Some(v) => Body::from(v.to_string()),
            None => Body::empty(),
        })
        .expect("build request");

    let response = router.oneshot(request).await.expect("route request");
    let status = response.status();
    let set_cookie = response
        .headers()
        .get(header::SET_COOKIE)
        .map(|v| v.to_str().expect("cookie header is ascii").to_owned());
    let bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024)
        .await
        .expect("read body");
    let json = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).expect("body is JSON")
    };
    (status, set_cookie, json)
}

/// Signs a fresh email all the way in and records consent, since callsign
/// reservation is gated on `ConsentedAccount`. Returns the session cookie
/// pair.
async fn sign_in_and_consent(app: &TestApp, email: &str) -> String {
    let (status, _, _) = send_json(
        app.router(),
        "POST",
        "/api/magic-links",
        Some(json!({ "email": email })),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED);

    let token = app
        .mailer
        .last_link()
        .split_once("token=")
        .expect("link carries a token")
        .1
        .to_owned();
    let (status, set_cookie, _) = send_json(
        app.router(),
        "POST",
        "/api/sessions",
        Some(json!({ "token": token })),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    let cookie = set_cookie
        .expect("201 sets the session cookie")
        .split(';')
        .next()
        .expect("cookie pair")
        .to_owned();

    let (status, _, _) = send_json(
        app.router(),
        "POST",
        "/api/consents",
        Some(json!({ "termsVersion": CURRENT_TERMS_VERSION })),
        Some(&cookie),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);

    cookie
}

/// Fabricates a live session for `account_id` bypassing the normal
/// magic-link flow (needed to model states — like an unverified email —
/// that the real flow cannot produce). Returns the session cookie pair.
async fn mint_session_cookie(app: &TestApp, account_id: Uuid) -> String {
    let raw = *Uuid::now_v7().as_bytes();
    let mut raw32 = [0u8; 32];
    raw32[..16].copy_from_slice(&raw);
    raw32[16..].copy_from_slice(&raw);
    let hash = hash_token(&raw32);
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock after epoch")
        .as_millis() as u64;
    app.state
        .sessions
        .insert(account_id, hash, now, now + 60 * 60 * 1000)
        .await
        .expect("insert session directly");

    use base64::Engine;
    let wire = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(raw32);
    format!("{SESSION_COOKIE}={wire}")
}

#[tokio::test]
async fn setting_a_callsign_normalizes_and_the_change_persists() {
    let app = test_app().await;
    let cookie = sign_in_and_consent(&app, "op@example.com").await;

    let (status, _, body) = send_json(
        app.router(),
        "PUT",
        "/api/accounts/me/callsign",
        Some(json!({ "callsign": "w1aw/p" })),
        Some(&cookie),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["callsign"], "W1AW");

    let (_, _, me) = send_json(app.router(), "GET", "/api/accounts/me", None, Some(&cookie)).await;
    assert_eq!(me["callsign"], "W1AW", "a subsequent GET /me agrees");
}

#[tokio::test]
async fn malformed_callsign_is_a_400_with_a_reason_specific_detail() {
    let app = test_app().await;
    let cookie = sign_in_and_consent(&app, "op@example.com").await;

    let (status_a, _, problem_a) = send_json(
        app.router(),
        "PUT",
        "/api/accounts/me/callsign",
        Some(json!({ "callsign": "ABC" })), // no separator digit
        Some(&cookie),
    )
    .await;
    assert_eq!(status_a, StatusCode::BAD_REQUEST);
    assert_eq!(problem_a["type"], "/errors/callsign-invalid");
    let detail_a = problem_a["detail"]
        .as_str()
        .expect("detail is a non-empty string")
        .to_owned();
    assert!(!detail_a.is_empty());

    let (status_b, _, problem_b) = send_json(
        app.router(),
        "PUT",
        "/api/accounts/me/callsign",
        Some(json!({ "callsign": "" })), // empty
        Some(&cookie),
    )
    .await;
    assert_eq!(status_b, StatusCode::BAD_REQUEST);
    assert_eq!(problem_b["type"], "/errors/callsign-invalid");
    let detail_b = problem_b["detail"]
        .as_str()
        .expect("detail is a non-empty string")
        .to_owned();
    assert!(!detail_b.is_empty());

    assert_ne!(
        detail_a, detail_b,
        "distinct rejection reasons must carry distinct detail text"
    );
}

#[tokio::test]
async fn reserving_a_callsign_held_by_another_account_is_a_409_taken_problem() {
    let app = test_app().await;
    let cookie_a = sign_in_and_consent(&app, "a@example.com").await;
    let cookie_b = sign_in_and_consent(&app, "b@example.com").await;

    let (status, _, _) = send_json(
        app.router(),
        "PUT",
        "/api/accounts/me/callsign",
        Some(json!({ "callsign": "W1AW" })),
        Some(&cookie_a),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let (status, _, problem) = send_json(
        app.router(),
        "PUT",
        "/api/accounts/me/callsign",
        Some(json!({ "callsign": "W1AW" })),
        Some(&cookie_b),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(problem["type"], "/errors/callsign-taken");
}

#[tokio::test]
async fn changing_a_callsign_frees_it_for_another_account_through_the_real_api() {
    let app = test_app().await;
    let cookie_a = sign_in_and_consent(&app, "a@example.com").await;
    let cookie_b = sign_in_and_consent(&app, "b@example.com").await;

    let (status, _, _) = send_json(
        app.router(),
        "PUT",
        "/api/accounts/me/callsign",
        Some(json!({ "callsign": "W1AW" })),
        Some(&cookie_a),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let (status, _, body) = send_json(
        app.router(),
        "PUT",
        "/api/accounts/me/callsign",
        Some(json!({ "callsign": "K1ABC" })),
        Some(&cookie_a),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["callsign"], "K1ABC");

    let (status, _, body) = send_json(
        app.router(),
        "PUT",
        "/api/accounts/me/callsign",
        Some(json!({ "callsign": "W1AW" })),
        Some(&cookie_b),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "W1AW was freed by A's change and must be reusable by B"
    );
    assert_eq!(body["callsign"], "W1AW");
}

#[tokio::test]
async fn unauthenticated_put_is_401() {
    let app = test_app().await;

    let (status, _, problem) = send_json(
        app.router(),
        "PUT",
        "/api/accounts/me/callsign",
        Some(json!({ "callsign": "W1AW" })),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(problem["type"], "/errors/unauthenticated");
}

#[tokio::test]
async fn signed_in_but_unconsented_account_is_403_consent_required() {
    let app = test_app().await;

    // Sign in WITHOUT recording consent — this route is the first
    // production consumer of the `ConsentedAccount` extractor.
    let (status, _, _) = send_json(
        app.router(),
        "POST",
        "/api/magic-links",
        Some(json!({ "email": "op@example.com" })),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED);
    let token = app
        .mailer
        .last_link()
        .split_once("token=")
        .expect("link carries a token")
        .1
        .to_owned();
    let (status, set_cookie, _) = send_json(
        app.router(),
        "POST",
        "/api/sessions",
        Some(json!({ "token": token })),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    let cookie = set_cookie
        .expect("201 sets the session cookie")
        .split(';')
        .next()
        .expect("cookie pair")
        .to_owned();

    let (status, _, problem) = send_json(
        app.router(),
        "PUT",
        "/api/accounts/me/callsign",
        Some(json!({ "callsign": "W1AW" })),
        Some(&cookie),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(problem["type"], "/errors/consent-required");
}

#[tokio::test]
async fn unverified_email_is_403_email_unverified() {
    let app = test_app().await;

    // Test-only setup: every real magic-link account is verified by
    // construction, so this state is modeled by a direct row insert
    // (security review H-2's defensive posture).
    let account_id = Uuid::now_v7();
    sqlx::query("INSERT INTO accounts (id, email) VALUES ($1, $2)")
        .bind(account_id)
        .bind("unverified@example.com")
        .execute(&app.pool)
        .await
        .expect("seed unverified account");
    app.state
        .consents
        .record(
            account_id,
            &ConsentRecord {
                terms_version: CURRENT_TERMS_VERSION.into(),
                consented_at_millis: 0,
            },
        )
        .await
        .expect("record consent so only the email-verified gate is under test");

    let cookie = mint_session_cookie(&app, account_id).await;

    let (status, _, problem) = send_json(
        app.router(),
        "PUT",
        "/api/accounts/me/callsign",
        Some(json!({ "callsign": "W1AW" })),
        Some(&cookie),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(problem["type"], "/errors/email-unverified");
}

#[tokio::test]
async fn no_callsign_is_no_regression_on_me_or_sign_out() {
    let app = test_app().await;
    let cookie = sign_in_and_consent(&app, "op@example.com").await;

    let (status, _, me) =
        send_json(app.router(), "GET", "/api/accounts/me", None, Some(&cookie)).await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        me["callsign"].is_null(),
        "an account without a callsign still reads null, never an error"
    );

    let (status, _, _) = send_json(
        app.router(),
        "DELETE",
        "/api/sessions/current",
        None,
        Some(&cookie),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::NO_CONTENT,
        "no existing route gains a callsign requirement"
    );
}
