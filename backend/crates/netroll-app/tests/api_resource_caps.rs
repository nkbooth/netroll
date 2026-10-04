// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
//! Full-loop API tests for resource caps, the net-creation rate
//! limiter, and bot mitigation on net creation: real router, real Postgres
//! (testcontainers), capturing fake mailer. Asserts status codes, problem+json
//! `type` slugs, and DB side-effects (row counts) — never message prose
//! (house TDD rule).

use std::sync::{Arc, Mutex};

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use netroll_app::bot_mitigation::BotMitigation;
use netroll_app::config::resolve_bot_mitigation_secret;
use netroll_app::http::{AppState, api_router};
use netroll_domain::bot_mitigation::issue_form_token;
use netroll_domain::consent::CURRENT_TERMS_VERSION;
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
                .expect("lock")
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
                .expect("lock")
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
                .expect("lock")
                .push((to.to_owned(), new_email.to_owned()));
            Ok(())
        })
    }
}

struct TestApp {
    _container: ContainerAsync<Postgres>,
    state: AppState,
    mailer: Arc<CapturingMailer>,
    pool: PgPool,
}

impl TestApp {
    fn router(&self) -> Router {
        api_router(self.state.clone())
    }

    /// Number of `net_definitions` rows (any archival state) — the witness that
    /// a rejected/dropped create wrote NO row.
    async fn net_rows(&self) -> i64 {
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM net_definitions")
            .fetch_one(&self.pool)
            .await
            .expect("count nets")
    }

    /// Number of owner rows for a net — the witness that a refused add-owner
    /// wrote NO row.
    async fn owner_rows(&self, net_id: &str) -> i64 {
        sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(*) FROM net_definition_owners WHERE net_definition_id = $1::uuid",
        )
        .bind(net_id)
        .fetch_one(&self.pool)
        .await
        .expect("count owners")
    }
}

async fn test_app_configured(customize: impl FnOnce(AppState) -> AppState) -> TestApp {
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

    let mailer = Arc::new(CapturingMailer::default());
    let state = customize(AppState::new(
        pool.clone(),
        mailer.clone(),
        "http://localhost:5173".into(),
    ));
    TestApp {
        _container: container,
        state,
        mailer,
        pool,
    }
}

async fn send_json(
    router: Router,
    method: &str,
    uri: &str,
    body: Option<Value>,
    cookie: Option<&str>,
) -> (StatusCode, Value) {
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
    let bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024)
        .await
        .expect("body");
    let json = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).expect("body is JSON")
    };
    (status, json)
}

/// Signs in, consents, and reserves a callsign — the full gate net creation
/// requires. Returns the session cookie.
async fn sign_in_consent_callsign(app: &TestApp, email: &str, callsign: &str) -> String {
    // Carry a valid human form token so the sign-in succeeds even when the
    // instance has bot mitigation enabled (ignored when disabled).
    let (status, _) = send_json(
        app.router(),
        "POST",
        "/api/magic-links",
        Some(json!({ "email": email, "formToken": human_form_token(), "hpField": "" })),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED);
    let token = app
        .mailer
        .last_link()
        .split_once("token=")
        .expect("token")
        .1
        .to_owned();

    let request = Request::builder()
        .method("POST")
        .uri("/api/sessions")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(json!({ "token": token }).to_string()))
        .expect("build request");
    let response = app.router().oneshot(request).await.expect("route");
    assert_eq!(response.status(), StatusCode::CREATED);
    let cookie = response
        .headers()
        .get(header::SET_COOKIE)
        .expect("cookie")
        .to_str()
        .expect("ascii")
        .split(';')
        .next()
        .expect("pair")
        .to_owned();

    let (status, _) = send_json(
        app.router(),
        "POST",
        "/api/consents",
        Some(json!({ "termsVersion": CURRENT_TERMS_VERSION })),
        Some(&cookie),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);

    let (status, _) = send_json(
        app.router(),
        "PUT",
        "/api/accounts/me/callsign",
        Some(json!({ "callsign": callsign })),
        Some(&cookie),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    cookie
}

fn definition_json(title: &str) -> Value {
    json!({
        "title": title,
        "connections": [
            { "kind": "hf", "plannedFrequencyHz": 14_230_000, "band": "20m", "mode": "ssb" }
        ],
        "netCategory": "traffic",
        "netType": "open",
    })
}

/// Current wall-clock millis, matching the `SystemClock` the handler reads.
fn wall_now_millis() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("after epoch")
        .as_millis() as u64
}

fn human_form_token() -> String {
    issue_form_token(BOT_SECRET_BYTES, wall_now_millis() - 5_000)
}

// ---- Max nets per user ----------------------------------------------

#[tokio::test]
async fn creating_past_the_net_cap_is_a_409_with_no_row_and_archiving_frees_a_slot() {
    // Non-default cap of 2 (enforced value is the configured one).
    let app = test_app_configured(|s| s.with_resource_caps(2, 5)).await;
    let cookie = sign_in_consent_callsign(&app, "capped@example.com", "W1CAP").await;

    // Two creates succeed (at the cap).
    let mut ids = Vec::new();
    for n in 0..2 {
        let (status, body) = send_json(
            app.router(),
            "POST",
            "/api/net-definitions",
            Some(definition_json(&format!("Net {n}"))),
            Some(&cookie),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED, "create {n} within cap");
        ids.push(body["id"].as_str().expect("id").to_owned());
    }
    assert_eq!(app.net_rows().await, 2);

    // The third create is refused with the specific 409 and writes NO row.
    let (status, problem) = send_json(
        app.router(),
        "POST",
        "/api/net-definitions",
        Some(definition_json("Net over cap")),
        Some(&cookie),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(problem["type"], "/errors/max-nets-per-user-reached");
    assert_eq!(app.net_rows().await, 2, "no row written past the cap");

    // Archiving one active net frees a slot: the next create succeeds.
    let (status, _) = send_json(
        app.router(),
        "DELETE",
        &format!("/api/net-definitions/{}", ids[0]),
        None,
        Some(&cookie),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    let (status, _) = send_json(
        app.router(),
        "POST",
        "/api/net-definitions",
        Some(definition_json("Net after archive")),
        Some(&cookie),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::CREATED,
        "archiving a net frees a cap slot"
    );
}

// ---- Net-creation rate limiter → 429 -----------------------------------

#[tokio::test]
async fn net_creation_past_the_burst_is_a_429_with_retry_after() {
    // A high cap so the LIMITER (burst 12), not the cap, is what fires.
    let app = test_app_configured(|s| s.with_resource_caps(100, 5)).await;
    let cookie = sign_in_consent_callsign(&app, "flood@example.com", "W1FLD").await;

    // The burst of 12 passes.
    for n in 0..12 {
        let request = Request::builder()
            .method("POST")
            .uri("/api/net-definitions")
            .header(header::CONTENT_TYPE, "application/json")
            .header(header::COOKIE, &cookie)
            .body(Body::from(
                definition_json(&format!("Burst {n}")).to_string(),
            ))
            .expect("build");
        let status = app.router().oneshot(request).await.expect("route").status();
        assert_eq!(status, StatusCode::CREATED, "create {n} within the burst");
    }

    // The next create is throttled: 429 + Retry-After.
    let request = Request::builder()
        .method("POST")
        .uri("/api/net-definitions")
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::COOKIE, &cookie)
        .body(Body::from(definition_json("Over burst").to_string()))
        .expect("build");
    let response = app.router().oneshot(request).await.expect("route");
    assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
    assert!(
        response.headers().get(header::RETRY_AFTER).is_some(),
        "429 must carry Retry-After"
    );
    let bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024)
        .await
        .expect("body");
    let problem: Value = serde_json::from_slice(&bytes).expect("json");
    assert_eq!(problem["type"], "/errors/rate-limited");
}

// ---- Max owners per net ---------------------------------------------

#[tokio::test]
async fn adding_past_the_owner_cap_is_a_409_and_a_reexisting_owner_is_idempotent() {
    // Owner cap of 2: the creator plus one more, then the third distinct
    // add is refused.
    let app = test_app_configured(|s| s.with_resource_caps(100, 2)).await;
    let owner = sign_in_consent_callsign(&app, "owner@example.com", "W1OWN").await;
    // Two more accounts holding callsigns (an add-owner target must resolve).
    sign_in_consent_callsign(&app, "co1@example.com", "W1AAA").await;
    sign_in_consent_callsign(&app, "co2@example.com", "W1BBB").await;

    let (status, body) = send_json(
        app.router(),
        "POST",
        "/api/net-definitions",
        Some(definition_json("Co-owned")),
        Some(&owner),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    let net_id = body["id"].as_str().expect("id").to_owned();
    assert_eq!(
        app.owner_rows(&net_id).await,
        1,
        "creator is the sole owner"
    );

    // Add a second owner — reaches the cap of 2.
    let (status, _) = send_json(
        app.router(),
        "POST",
        &format!("/api/net-definitions/{net_id}/owners"),
        Some(json!({ "callsign": "W1AAA" })),
        Some(&owner),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(app.owner_rows(&net_id).await, 2);

    // A third DISTINCT owner is refused with the specific 409 and no row.
    let (status, problem) = send_json(
        app.router(),
        "POST",
        &format!("/api/net-definitions/{net_id}/owners"),
        Some(json!({ "callsign": "W1BBB" })),
        Some(&owner),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(problem["type"], "/errors/max-owners-per-net-reached");
    assert_eq!(
        app.owner_rows(&net_id).await,
        2,
        "no owner row past the cap"
    );

    // Re-adding an EXISTING owner at the cap is a 200 no-op (adds nothing).
    let (status, _) = send_json(
        app.router(),
        "POST",
        &format!("/api/net-definitions/{net_id}/owners"),
        Some(json!({ "callsign": "W1AAA" })),
        Some(&owner),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "re-add of an existing owner is allowed"
    );
    assert_eq!(
        app.owner_rows(&net_id).await,
        2,
        "idempotent re-add adds no row"
    );
}

// ---- Bot mitigation on net creation ---------------------------------

async fn bot_mitigated_caps_app() -> TestApp {
    test_app_configured(|state| {
        let secret = resolve_bot_mitigation_secret(Some(BOT_SECRET_RAW.into()))
            .expect("a long-enough secret resolves")
            .expect("a set secret enables mitigation");
        state
            .with_resource_caps(100, 5)
            .with_bot_mitigation(BotMitigation::with_secret(secret))
    })
    .await
}

#[tokio::test]
async fn bot_mitigation_drops_a_filled_honeypot_net_create_with_no_row() {
    let app = bot_mitigated_caps_app().await;
    let cookie = sign_in_consent_callsign(&app, "botnet@example.com", "W1BOT").await;

    let mut body = definition_json("Spam net");
    body["formToken"] = json!(human_form_token());
    body["hpField"] = json!("http://spam.example");
    let (status, resp) = send_json(
        app.router(),
        "POST",
        "/api/net-definitions",
        Some(body),
        Some(&cookie),
    )
    .await;

    // Success-shaped 201 (same schema — carries an id), but NO row persisted.
    assert_eq!(status, StatusCode::CREATED);
    assert!(resp["id"].is_string(), "response is success-shaped");
    assert_eq!(app.net_rows().await, 0, "a dropped create writes no row");
}

#[tokio::test]
async fn bot_mitigation_lets_a_valid_human_net_create_through() {
    let app = bot_mitigated_caps_app().await;
    let cookie = sign_in_consent_callsign(&app, "humannet@example.com", "W1HUM").await;

    let mut body = definition_json("Real net");
    body["formToken"] = json!(human_form_token());
    body["hpField"] = json!("");
    let (status, resp) = send_json(
        app.router(),
        "POST",
        "/api/net-definitions",
        Some(body),
        Some(&cookie),
    )
    .await;

    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(app.net_rows().await, 1, "a human create persists a row");
    // The persisted net is retrievable by its real id (proving it is not the
    // fabricated body).
    let id = resp["id"].as_str().expect("id");
    let (status, _) = send_json(
        app.router(),
        "GET",
        &format!("/api/net-definitions/{id}"),
        None,
        Some(&cookie),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "the created net is retrievable");
}
