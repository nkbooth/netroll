// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
//! Full-loop API tests for account self-deletion and the undelete window: real
//! router, real Postgres, an injectable fake clock so undelete-in-window versus
//! finalize-past-window are driven by time rather than by sleeping. Asserts
//! status codes, cookie clearing, and account-id identity across undelete
//! versus the fresh id after finalize — never message prose.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use axum::Router;
use axum::body::Body;
use axum::http::{HeaderMap, Request, StatusCode, header};
use netroll_app::http::{AppState, api_router};
use netroll_domain::consent::CURRENT_TERMS_VERSION;
use netroll_domain::deletion::DELETION_GRACE_MILLIS;
use netroll_domain::ports::{BoxFuture, Clock, MailError, Mailer};
use serde_json::{Value, json};
use sqlx::postgres::PgPoolOptions;
use testcontainers_modules::postgres::Postgres;
use testcontainers_modules::testcontainers::ContainerAsync;
use testcontainers_modules::testcontainers::runners::AsyncRunner;
use tower::ServiceExt;

/// Wall clock the test can wind forward — undelete and finalize-on-access
/// both key off `deletion_verdict(now)`, so time is a test input.
#[derive(Clone)]
struct FakeClock {
    millis: Arc<AtomicU64>,
}

impl FakeClock {
    fn new(start: u64) -> Self {
        Self {
            millis: Arc::new(AtomicU64::new(start)),
        }
    }
    fn set(&self, millis: u64) {
        self.millis.store(millis, Ordering::SeqCst);
    }
}

impl Clock for FakeClock {
    fn now_epoch_millis(&self) -> u64 {
        self.millis.load(Ordering::SeqCst)
    }
}

/// Records magic-link sends so the sign-in helper can extract the token.
#[derive(Default)]
struct CapturingMailer {
    magic_links: Mutex<Vec<String>>,
}

impl Mailer for CapturingMailer {
    fn send_magic_link<'a>(
        &'a self,
        _to: &'a str,
        link: &'a str,
    ) -> BoxFuture<'a, Result<(), MailError>> {
        Box::pin(async move {
            self.magic_links.lock().expect("lock").push(link.to_owned());
            Ok(())
        })
    }

    fn send_email_change<'a>(
        &'a self,
        _to: &'a str,
        _link: &'a str,
    ) -> BoxFuture<'a, Result<(), MailError>> {
        Box::pin(async move { Ok(()) })
    }

    fn send_email_change_notice<'a>(
        &'a self,
        _to: &'a str,
        _new_email: &'a str,
    ) -> BoxFuture<'a, Result<(), MailError>> {
        Box::pin(async move { Ok(()) })
    }
}

impl CapturingMailer {
    fn last_magic_link(&self) -> String {
        self.magic_links
            .lock()
            .expect("lock")
            .last()
            .expect("a magic link was sent")
            .clone()
    }
}

struct TestApp {
    _container: ContainerAsync<Postgres>,
    state: AppState,
    mailer: Arc<CapturingMailer>,
    clock: FakeClock,
}

impl TestApp {
    fn router(&self) -> Router {
        api_router(self.state.clone())
    }
}

const START_MILLIS: u64 = 1_800_000_000_000;

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
    let clock = FakeClock::new(START_MILLIS);
    let mut state = AppState::new(pool, mailer.clone(), "http://localhost:5173".into());
    state.clock = Arc::new(clock.clone());
    TestApp {
        _container: container,
        state,
        mailer,
        clock,
    }
}

async fn send_raw(
    router: Router,
    method: &str,
    uri: &str,
    body: Option<Value>,
    cookie: Option<&str>,
) -> (StatusCode, HeaderMap, Value) {
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
    let headers = response.headers().clone();
    let bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024)
        .await
        .expect("read body");
    let json = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).expect("body is JSON")
    };
    (status, headers, json)
}

async fn send_json(
    router: Router,
    method: &str,
    uri: &str,
    body: Option<Value>,
    cookie: Option<&str>,
) -> (StatusCode, Value) {
    let (status, _, json) = send_raw(router, method, uri, body, cookie).await;
    (status, json)
}

fn token_from_link(link: &str) -> String {
    link.split_once("token=")
        .expect("link carries a token")
        .1
        .to_owned()
}

/// Signs `email` in (magic-link → session) at the current clock instant.
/// Returns the session cookie and the account body from the 201.
async fn sign_in(app: &TestApp, email: &str) -> (String, Value) {
    let (status, _) = send_json(
        app.router(),
        "POST",
        "/api/magic-links",
        Some(json!({ "email": email })),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED);

    let token = token_from_link(&app.mailer.last_magic_link());
    let (status, headers, account) = send_raw(
        app.router(),
        "POST",
        "/api/sessions",
        Some(json!({ "token": token })),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    let cookie = headers
        .get(header::SET_COOKIE)
        .expect("201 sets the session cookie")
        .to_str()
        .expect("cookie header is ascii")
        .split(';')
        .next()
        .expect("cookie pair")
        .to_owned();
    (cookie, account)
}

async fn record_consent(app: &TestApp, cookie: &str) {
    let (status, _) = send_json(
        app.router(),
        "POST",
        "/api/consents",
        Some(json!({ "termsVersion": CURRENT_TERMS_VERSION })),
        Some(cookie),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
}

const DELETE_URI: &str = "/api/accounts/me";

#[tokio::test]
async fn deleting_signs_out_everywhere_and_returns_204_with_a_cleared_cookie() {
    let app = test_app().await;
    let (cookie, _) = sign_in(&app, "op@example.com").await;

    let (status, headers, _) =
        send_raw(app.router(), "DELETE", DELETE_URI, None, Some(&cookie)).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let set_cookie = headers
        .get(header::SET_COOKIE)
        .expect("delete clears the session cookie")
        .to_str()
        .expect("ascii");
    assert!(
        set_cookie.contains("Max-Age=0"),
        "the session cookie is cleared (got {set_cookie})"
    );

    // The session that authorized the delete is revoked.
    let (status, _) = send_json(app.router(), "GET", "/api/accounts/me", None, Some(&cookie)).await;
    assert_eq!(
        status,
        StatusCode::UNAUTHORIZED,
        "the old session no longer authenticates"
    );
}

#[tokio::test]
async fn signing_back_in_within_the_window_restores_the_same_account() {
    let app = test_app().await;
    let (cookie, account) = sign_in(&app, "op@example.com").await;
    let original_id = account["id"].as_str().expect("id").to_owned();

    // Give the account distinguishing state.
    record_consent(&app, &cookie).await;
    let (status, _) = send_json(
        app.router(),
        "PUT",
        "/api/accounts/me/callsign",
        Some(json!({ "callsign": "w1aw" })),
        Some(&cookie),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let (status, _, _) = send_raw(app.router(), "DELETE", DELETE_URI, None, Some(&cookie)).await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    // Still inside the 15-minute window: sign back in → SAME account, intact.
    app.clock.set(START_MILLIS + DELETION_GRACE_MILLIS / 2);
    let (cookie_new, restored) = sign_in(&app, "op@example.com").await;
    assert_eq!(
        restored["id"].as_str(),
        Some(original_id.as_str()),
        "undelete restores the ORIGINAL account id"
    );
    assert_eq!(restored["callsign"], "W1AW", "callsign survives undelete");

    let (status, me) = send_json(
        app.router(),
        "GET",
        "/api/accounts/me",
        None,
        Some(&cookie_new),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(me["id"].as_str(), Some(original_id.as_str()));
    assert_eq!(me["callsign"], "W1AW");
}

#[tokio::test]
async fn signing_in_past_the_window_finalizes_and_mints_a_fresh_empty_account() {
    let app = test_app().await;
    let (cookie, account) = sign_in(&app, "op@example.com").await;
    let original_id = account["id"].as_str().expect("id").to_owned();

    record_consent(&app, &cookie).await;
    let (status, _) = send_json(
        app.router(),
        "PUT",
        "/api/accounts/me/callsign",
        Some(json!({ "callsign": "w1aw" })),
        Some(&cookie),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let (status, _, _) = send_raw(app.router(), "DELETE", DELETE_URI, None, Some(&cookie)).await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    // Past the window: a stale soft-deleted row is finalized on access and a
    // fresh empty account is minted — the past-window account is never
    // resurrected.
    app.clock.set(START_MILLIS + DELETION_GRACE_MILLIS + 1);
    let (_, reborn) = sign_in(&app, "op@example.com").await;
    assert_ne!(
        reborn["id"].as_str(),
        Some(original_id.as_str()),
        "a past-window account is finalized, not resurrected"
    );
    assert!(
        reborn["callsign"].is_null(),
        "the fresh account carries no callsign"
    );
    assert!(
        reborn["displayName"].is_null(),
        "the fresh account has no profile"
    );
    assert_eq!(
        reborn["consentRequired"], true,
        "the fresh account has not consented"
    );
}

#[tokio::test]
async fn an_unauthenticated_delete_is_401_unauthenticated() {
    let app = test_app().await;
    let (status, problem) = send_json(app.router(), "DELETE", DELETE_URI, None, None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(problem["type"], "/errors/unauthenticated");
}

#[tokio::test]
async fn a_plain_returning_user_sign_in_is_unaffected_by_finalize_on_access() {
    let app = test_app().await;
    let (_, first) = sign_in(&app, "op@example.com").await;
    let id = first["id"].as_str().expect("id").to_owned();

    // No deletion in play: a later sign-in reaches the SAME account, 201,
    // byte-for-byte the prior behavior (the finalize branch is inert).
    app.clock.set(START_MILLIS + 60_000);
    let (cookie, second) = sign_in(&app, "op@example.com").await;
    assert_eq!(
        second["id"].as_str(),
        Some(id.as_str()),
        "a returning user keeps the same account"
    );
    let (status, _) = send_json(app.router(), "GET", "/api/accounts/me", None, Some(&cookie)).await;
    assert_eq!(status, StatusCode::OK);
}
