// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
//! Full-loop API tests for the consent gate: real
//! router, real Postgres (testcontainers), capturing fake mailer. Asserts
//! status codes, problem+json `type` slugs, and storage effects — never
//! message strings.

use std::sync::{Arc, Mutex};

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use axum::routing::get;
use netroll_app::http::{AppState, api_router};
use netroll_app::middleware::consent::ConsentedAccount;
use netroll_app::middleware::session::require_session;
use netroll_domain::consent::{CURRENT_TERMS_VERSION, ConsentRecord};
use netroll_domain::ports::{BoxFuture, MailError, Mailer};
use serde_json::{Value, json};
use sqlx::PgPool;
use sqlx::postgres::PgPoolOptions;
use testcontainers_modules::postgres::Postgres;
use testcontainers_modules::testcontainers::ContainerAsync;
use testcontainers_modules::testcontainers::runners::AsyncRunner;
use tower::ServiceExt;

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

/// Signs a fresh email all the way in; returns the session cookie pair and
/// the 201 session body.
async fn sign_in(app: &TestApp, email: &str) -> (String, Value) {
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
    let (status, set_cookie, body) = send_json(
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
    (cookie, body)
}

#[tokio::test]
async fn me_reports_consent_required_until_recorded_then_never_again() {
    let app = test_app().await;
    let (cookie, session_body) = sign_in(&app, "op@example.com").await;

    // The 201 session body already carries consent status (the verify page
    // branches on it without a second round trip).
    assert_eq!(session_body["consentRequired"], true);
    assert_eq!(session_body["requiredTermsVersion"], CURRENT_TERMS_VERSION);

    // /me is reachable UNCONSENTED (gating it would deadlock the flow) and
    // reports the same status additively — the existing fields unchanged.
    let (status, _, me) =
        send_json(app.router(), "GET", "/api/accounts/me", None, Some(&cookie)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(me["email"], "op@example.com");
    assert_eq!(me["consentRequired"], true);
    assert_eq!(me["requiredTermsVersion"], CURRENT_TERMS_VERSION);

    // Record consent to the required version.
    let (status, _, _) = send_json(
        app.router(),
        "POST",
        "/api/consents",
        Some(json!({ "termsVersion": CURRENT_TERMS_VERSION })),
        Some(&cookie),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);

    // Consent recorded → the gate never shows again, including on a
    // brand-new sign-in of the same account.
    let (_, _, me) = send_json(app.router(), "GET", "/api/accounts/me", None, Some(&cookie)).await;
    assert_eq!(me["consentRequired"], false);

    let (_, second_body) = sign_in(&app, "op@example.com").await;
    assert_eq!(second_body["consentRequired"], false);
}

#[tokio::test]
async fn recording_consent_is_idempotent_and_server_timestamped() {
    let app = test_app().await;
    let (cookie, _) = sign_in(&app, "op@example.com").await;

    for _ in 0..2 {
        let (status, _, _) = send_json(
            app.router(),
            "POST",
            "/api/consents",
            Some(json!({ "termsVersion": CURRENT_TERMS_VERSION })),
            Some(&cookie),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED, "repeat POST stays a success");
    }

    let rows: Vec<(String, Option<chrono::DateTime<chrono::Utc>>)> =
        sqlx::query_as("SELECT terms_version, consented_at FROM account_consents")
            .fetch_all(&app.pool)
            .await
            .expect("read consents");
    assert_eq!(rows.len(), 1, "exactly one row despite the double POST");
    assert_eq!(rows[0].0, CURRENT_TERMS_VERSION);
    assert!(
        rows[0].1.is_some(),
        "consent carries a server-side timestamp"
    );
}

#[tokio::test]
async fn stale_terms_version_is_a_409_mismatch_problem() {
    let app = test_app().await;
    let (cookie, _) = sign_in(&app, "op@example.com").await;

    let (status, _, problem) = send_json(
        app.router(),
        "POST",
        "/api/consents",
        Some(json!({ "termsVersion": "2020-01-01" })),
        Some(&cookie),
    )
    .await;

    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(problem["type"], "/errors/consent-version-mismatch");

    let rows: i64 = sqlx::query_scalar("SELECT count(*) FROM account_consents")
        .fetch_one(&app.pool)
        .await
        .expect("count consents");
    assert_eq!(rows, 0, "a stale version must not record anything");
}

#[tokio::test]
async fn consent_to_an_older_version_still_gates_through_the_real_api() {
    let app = test_app().await;
    let (cookie, _) = sign_in(&app, "op@example.com").await;

    let account = app
        .state
        .accounts
        .find_by_email("op@example.com")
        .await
        .expect("look up account")
        .expect("account exists");

    // Record consent to a version that predates CURRENT_TERMS_VERSION —
    // stale acceptance history must not satisfy today's requirement.
    app.state
        .consents
        .record(
            account.id,
            &ConsentRecord {
                terms_version: "2025-01-01".into(),
                consented_at_millis: 0,
            },
        )
        .await
        .expect("seed a stale consent row");

    let (_, _, me) = send_json(app.router(), "GET", "/api/accounts/me", None, Some(&cookie)).await;
    assert_eq!(
        me["consentRequired"], true,
        "consent to an older version must not carry forward through the real API"
    );
}

#[tokio::test]
async fn consents_endpoint_requires_a_session() {
    let app = test_app().await;

    let (status, _, problem) = send_json(
        app.router(),
        "POST",
        "/api/consents",
        Some(json!({ "termsVersion": CURRENT_TERMS_VERSION })),
        None,
    )
    .await;

    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(problem["type"], "/errors/unauthenticated");
}

#[tokio::test]
async fn the_guard_rejects_unconsented_accounts_and_passes_consented_ones() {
    let app = test_app().await;
    let (cookie, _) = sign_in(&app, "op@example.com").await;

    // Test-only gated route: the extractor IS the enforcement seam later
    // stories attach; no production route consumes it yet.
    async fn gated(_account: ConsentedAccount) -> StatusCode {
        StatusCode::OK
    }
    let gated_router = || {
        Router::new()
            .route("/gated", get(gated))
            .route_layer(axum::middleware::from_fn_with_state(
                app.state.clone(),
                require_session,
            ))
            .with_state(app.state.clone())
    };

    let (status, _, problem) =
        send_json(gated_router(), "GET", "/gated", None, Some(&cookie)).await;
    assert_eq!(
        status,
        StatusCode::FORBIDDEN,
        "unconsented account must be refused at the HTTP layer"
    );
    assert_eq!(problem["type"], "/errors/consent-required");

    let (status, _, _) = send_json(
        app.router(),
        "POST",
        "/api/consents",
        Some(json!({ "termsVersion": CURRENT_TERMS_VERSION })),
        Some(&cookie),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);

    let (status, _, _) = send_json(gated_router(), "GET", "/gated", None, Some(&cookie)).await;
    assert_eq!(status, StatusCode::OK, "consented account passes through");
}

#[tokio::test]
async fn sign_out_stays_reachable_for_unconsented_accounts() {
    let app = test_app().await;
    let (cookie, _) = sign_in(&app, "op@example.com").await;

    // Gating sign-out would trap the user at the gate — regression contract.
    let (status, _, _) = send_json(
        app.router(),
        "DELETE",
        "/api/sessions/current",
        None,
        Some(&cookie),
    )
    .await;

    assert_eq!(status, StatusCode::NO_CONTENT);
}
