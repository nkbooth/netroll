// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
//! Full-loop API tests for the roster-memory prefill: the additive
//! name/location on the check-in add path, and the staff-gated,
//! definition-scoped lookup endpoint. Real router, real Postgres
//! (testcontainers), capturing fake mailer. Asserts status codes, problem+json
//! `type` slugs, and folded/looked-up state — never message prose.

use std::sync::Arc;
use std::sync::Mutex;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use netroll_app::http::{AppState, api_router};
use netroll_domain::consent::CURRENT_TERMS_VERSION;
use netroll_domain::ports::{BoxFuture, MailError, Mailer};
use serde_json::{Value, json};
use sqlx::postgres::PgPoolOptions;
use testcontainers_modules::postgres::Postgres;
use testcontainers_modules::testcontainers::ContainerAsync;
use testcontainers_modules::testcontainers::runners::AsyncRunner;
use tower::ServiceExt;

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
}

impl TestApp {
    fn router(&self) -> Router {
        api_router(self.state.clone())
    }
}

async fn test_app() -> TestApp {
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
    let state = AppState::new(pool.clone(), mailer.clone(), "http://localhost:5173".into());
    TestApp {
        _container: container,
        state,
        mailer,
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

async fn sign_in_consent_callsign(app: &TestApp, email: &str, callsign: &str) -> String {
    let cookie = sign_in_consent(app, email).await;
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

async fn sign_in_consent(app: &TestApp, email: &str) -> String {
    let (status, _) = send_json(
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
    cookie
}

fn full_definition_json() -> Value {
    json!({
        "title": "Sunday Traffic Net",
        "connections": [
            { "kind": "hf", "plannedFrequencyHz": 14_230_000, "band": "20m", "mode": "ssb" }
        ],
        "netCategory": "traffic",
        "netType": "open"
    })
}

async fn create_net(app: &TestApp, cookie: &str) -> String {
    let (status, body) = send_json(
        app.router(),
        "POST",
        "/api/net-definitions",
        Some(full_definition_json()),
        Some(cookie),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    body["id"].as_str().expect("definition id").to_owned()
}

async fn start_session(app: &TestApp, cookie: &str, definition_id: &str) -> String {
    let (status, body) = send_json(
        app.router(),
        "POST",
        "/api/net-sessions",
        Some(json!({ "definitionId": definition_id })),
        Some(cookie),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    body["id"].as_str().expect("session id").to_owned()
}

async fn grant(app: &TestApp, cookie: &str, session_id: &str, callsign: &str, role: &str) {
    let (status, _) = send_json(
        app.router(),
        "POST",
        &format!("/api/net-sessions/{session_id}/roles"),
        Some(json!({ "callsign": callsign, "role": role })),
        Some(cookie),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "grant {role} should succeed");
}

/// Adds a check-in with an optional name/location body, returning the folded
/// summary. Used to prove the add path carries the identity fields.
async fn add_check_in(
    app: &TestApp,
    cookie: &str,
    session_id: &str,
    body: Value,
) -> (StatusCode, Value) {
    send_json(
        app.router(),
        "POST",
        &format!("/api/net-sessions/{session_id}/check-ins"),
        Some(body),
        Some(cookie),
    )
    .await
}

fn entry<'a>(summary: &'a Value, callsign: &str) -> &'a Value {
    summary["roster"]
        .as_array()
        .expect("roster")
        .iter()
        .find(|e| e["callsign"] == callsign)
        .expect("the entry")
}

async fn lookup(
    app: &TestApp,
    cookie: Option<&str>,
    session_id: &str,
    callsign: &str,
) -> (StatusCode, Value) {
    send_json(
        app.router(),
        "GET",
        &format!("/api/net-sessions/{session_id}/roster-memory?callsign={callsign}"),
        None,
        cookie,
    )
    .await
}

// --- add path carries name/location -----------------------------------------

#[tokio::test]
async fn a_check_in_committed_with_name_and_location_shows_them_on_the_roster() {
    // The prefilled identity committed at add time is persisted on the
    // roster entry (folded), not just at edit time.
    let app = test_app().await;
    let owner = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;
    let definition_id = create_net(&app, &owner).await;
    let session_id = start_session(&app, &owner, &definition_id).await;

    let (status, summary) = add_check_in(
        &app,
        &owner,
        &session_id,
        json!({ "callsign": "W1AW", "name": "Maria", "location": "Hartford, CT" }),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    let e = entry(&summary, "W1AW");
    assert_eq!(e["name"], "Maria");
    assert_eq!(e["location"], "Hartford, CT");
}

// --- lookup endpoint --------------------------------------------------------

#[tokio::test]
async fn a_returning_station_is_remembered_across_sessions_of_the_same_definition() {
    // Add + close in session 1 → the lookup in a LATER session of the same
    // definition returns the remembered name/location (the two-session invariant).
    let app = test_app().await;
    let owner = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;
    let definition_id = create_net(&app, &owner).await;

    let session1 = start_session(&app, &owner, &definition_id).await;
    let (status, _) = add_check_in(
        &app,
        &owner,
        &session1,
        json!({ "callsign": "W1AW", "name": "Maria", "location": "Hartford, CT" }),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    let (status, _) = send_json(
        app.router(),
        "POST",
        &format!("/api/net-sessions/{session1}/close"),
        None,
        Some(&owner),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    // A NEW session of the SAME definition prefills the returning station.
    let session2 = start_session(&app, &owner, &definition_id).await;
    let (status, body) = lookup(&app, Some(&owner), &session2, "W1AW").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["name"], "Maria");
    assert_eq!(body["location"], "Hartford, CT");
}

#[tokio::test]
async fn a_lookup_miss_returns_an_empty_result_not_an_error() {
    let app = test_app().await;
    let owner = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;
    let definition_id = create_net(&app, &owner).await;
    let session_id = start_session(&app, &owner, &definition_id).await;

    let (status, body) = lookup(&app, Some(&owner), &session_id, "K9NEVER").await;
    assert_eq!(status, StatusCode::OK, "a miss is a 200, never an error");
    assert!(body["name"].is_null());
    assert!(body["location"].is_null());
}

#[tokio::test]
async fn a_lowercase_callsign_normalizes_and_still_hits() {
    let app = test_app().await;
    let owner = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;
    let definition_id = create_net(&app, &owner).await;
    let session_id = start_session(&app, &owner, &definition_id).await;
    let (status, _) = add_check_in(
        &app,
        &owner,
        &session_id,
        json!({ "callsign": "W1AW", "name": "Maria" }),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);

    // The caller sends the raw (lowercase) callsign; parse_callsign normalizes
    // it to the stored form so the lookup still hits.
    let (status, body) = lookup(&app, Some(&owner), &session_id, "w1aw").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["name"], "Maria");
}

#[tokio::test]
async fn a_logger_may_look_up_but_a_participant_may_not() {
    let app = test_app().await;
    let owner = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;
    let logger = sign_in_consent_callsign(&app, "logger@example.com", "w2bcd").await;
    let participant = sign_in_consent(&app, "participant@example.com").await;
    let definition_id = create_net(&app, &owner).await;
    let session_id = start_session(&app, &owner, &definition_id).await;
    grant(&app, &owner, &session_id, "w2bcd", "logger").await;

    // A Logger holds LogCheckIn → may prefill.
    let (status, _) = lookup(&app, Some(&logger), &session_id, "W1AW").await;
    assert_eq!(status, StatusCode::OK);

    // A Participant does NOT prefill (that is the self-check-in path) → 403.
    let (status, body) = lookup(&app, Some(&participant), &session_id, "W1AW").await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(body["type"], "/errors/forbidden");
}

#[tokio::test]
async fn a_lookup_on_a_missing_session_is_404_before_403() {
    let app = test_app().await;
    let _owner = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;
    let stranger = sign_in_consent(&app, "stranger@example.com").await;
    let missing = uuid::Uuid::now_v7();

    let (status, body) = lookup(&app, Some(&stranger), &missing.to_string(), "W1AW").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body["type"], "/errors/net-session-not-found");
}

#[tokio::test]
async fn the_lookup_requires_a_session_and_is_not_a_public_route() {
    // No cookie → 401 (the endpoint sits on the session-gated tree, never the
    // public account-less router).
    let app = test_app().await;
    let owner = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;
    let definition_id = create_net(&app, &owner).await;
    let session_id = start_session(&app, &owner, &definition_id).await;

    let (status, _) = lookup(&app, None, &session_id, "W1AW").await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn a_lookup_never_returns_another_definitions_memory() {
    // A callsign remembered on definition A must not surface for a
    // session of definition B — the definition is the scope boundary.
    let app = test_app().await;
    let owner = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;
    let def_a = create_net(&app, &owner).await;
    // A second definition needs a distinct title (unique per owner is not
    // required, but start a fresh net for clarity).
    let (status, body) = send_json(
        app.router(),
        "POST",
        "/api/net-definitions",
        Some(json!({
            "title": "Evening ARES Net",
            "connections": [
                { "kind": "hf", "plannedFrequencyHz": 7_200_000, "band": "40m", "mode": "ssb" }
            ],
            "netCategory": "emergency",
            "netType": "open"
        })),
        Some(&owner),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    let def_b = body["id"].as_str().expect("definition B id").to_owned();

    let session_a = start_session(&app, &owner, &def_a).await;
    let (status, _) = add_check_in(
        &app,
        &owner,
        &session_a,
        json!({ "callsign": "W1AW", "name": "Maria" }),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);

    // A session of definition B must NOT see A's memory for the same callsign.
    let session_b = start_session(&app, &owner, &def_b).await;
    let (status, body) = lookup(&app, Some(&owner), &session_b, "W1AW").await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        body["name"].is_null(),
        "definition B must not see definition A's remembered identity"
    );
}
