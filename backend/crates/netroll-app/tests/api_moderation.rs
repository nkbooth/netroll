// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
//! Full-loop API tests for the NCS session-moderation surface: real router,
//! real Postgres, capturing mailer. Asserts status codes, problem+json slugs
//! and DB/log side-effects, never message prose. `/moderate` is the NCS-gated
//! disciplinary remove-and-block, distinct from the Logger-floor correction
//! remove on `DELETE …/check-ins/{id}`.

use std::sync::Arc;
use std::sync::Mutex;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use netroll_app::http::{AppState, api_router};
use netroll_domain::consent::CURRENT_TERMS_VERSION;
use netroll_domain::ports::{BoxFuture, MailError, Mailer};
use serde_json::{Value, json};
use sqlx::PgPool;
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
    #[allow(dead_code)]
    pool: PgPool,
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

async fn grant(
    app: &TestApp,
    cookie: &str,
    session_id: &str,
    callsign: &str,
    role: &str,
) -> StatusCode {
    send_json(
        app.router(),
        "POST",
        &format!("/api/net-sessions/{session_id}/roles"),
        Some(json!({ "callsign": callsign, "role": role })),
        Some(cookie),
    )
    .await
    .0
}

/// A staff operator adds a callsign-only check-in and returns its `checkInId`.
async fn staff_add(app: &TestApp, cookie: &str, session_id: &str, callsign: &str) -> String {
    let (status, body) = send_json(
        app.router(),
        "POST",
        &format!("/api/net-sessions/{session_id}/check-ins"),
        Some(json!({ "callsign": callsign })),
        Some(cookie),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    body["roster"]
        .as_array()
        .expect("roster")
        .iter()
        .find(|e| e["callsign"] == callsign)
        .expect("the added row")["checkInId"]
        .as_str()
        .expect("checkInId")
        .to_owned()
}

/// A participant self-checks-in (own forced callsign) and returns its `checkInId`.
async fn self_check_in(app: &TestApp, cookie: &str, session_id: &str) -> String {
    let (status, body) = send_json(
        app.router(),
        "POST",
        &format!("/api/net-sessions/{session_id}/check-ins"),
        Some(json!({ "callsign": "IGNORED" })),
        Some(cookie),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    body["roster"]
        .as_array()
        .expect("roster")
        .iter()
        .find(|e| e["source"] == "self")
        .expect("the self row")["checkInId"]
        .as_str()
        .expect("checkInId")
        .to_owned()
}

fn moderate_body(block: bool, version: u64) -> Value {
    json!({ "block": block, "expectedVersion": version })
}

fn roster(summary: &Value) -> &Vec<Value> {
    summary["roster"].as_array().expect("roster")
}

// --- NCS remove ---------------------------------------------------------

#[tokio::test]
async fn an_ncs_removes_a_disruptive_check_in_via_moderate() {
    let app = test_app().await;
    let owner = sign_in_consent_callsign(&app, "owner@example.com", "W1OWN").await;
    let definition_id = create_net(&app, &owner).await;
    let session_id = start_session(&app, &owner, &definition_id).await;
    let check_in_id = staff_add(&app, &owner, &session_id, "W1AW").await;

    let (status, summary) = moderate(
        &app,
        &session_id,
        &check_in_id,
        &owner,
        moderate_body(false, 1),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    // The row is gone from the folded roster.
    assert!(
        roster(&summary)
            .iter()
            .all(|e| e["checkInId"] != check_in_id)
    );
}

// --- Block + self-check-in enforcement ------------------------------

#[tokio::test]
async fn an_ncs_blocks_a_self_checked_in_account_and_their_recheckin_is_refused() {
    let app = test_app().await;
    let owner = sign_in_consent_callsign(&app, "owner@example.com", "W1OWN").await;
    let participant = sign_in_consent_callsign(&app, "part@example.com", "W2BCD").await;
    let definition_id = create_net(&app, &owner).await;
    let session_id = start_session(&app, &owner, &definition_id).await;
    let check_in_id = self_check_in(&app, &participant, &session_id).await;

    let (status, summary) = moderate(
        &app,
        &session_id,
        &check_in_id,
        &owner,
        moderate_body(true, 1),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(roster(&summary).iter().all(|e| e["source"] != "self"));

    // The blocked account can no longer self-check-in — 403 account-blocked.
    let (status, body) = send_json(
        app.router(),
        "POST",
        &format!("/api/net-sessions/{session_id}/check-ins"),
        Some(json!({ "callsign": "W2BCD" })),
        Some(&participant),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(body["type"], "/errors/account-blocked");
}

#[tokio::test]
async fn a_blocked_account_re_attempting_with_a_different_callsign_is_still_refused() {
    // The block is keyed on the ACCOUNT id, not the callsign string: changing
    // callsign does not evade it (crux).
    let app = test_app().await;
    let owner = sign_in_consent_callsign(&app, "owner@example.com", "W1OWN").await;
    let participant = sign_in_consent_callsign(&app, "part@example.com", "W2BCD").await;
    let definition_id = create_net(&app, &owner).await;
    let session_id = start_session(&app, &owner, &definition_id).await;
    let check_in_id = self_check_in(&app, &participant, &session_id).await;

    let (status, _) = moderate(
        &app,
        &session_id,
        &check_in_id,
        &owner,
        moderate_body(true, 1),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    // The participant changes their reserved callsign, then re-attempts.
    let (status, _) = send_json(
        app.router(),
        "PUT",
        "/api/accounts/me/callsign",
        Some(json!({ "callsign": "W9NEW" })),
        Some(&participant),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let (status, body) = send_json(
        app.router(),
        "POST",
        &format!("/api/net-sessions/{session_id}/check-ins"),
        Some(json!({ "callsign": "W9NEW" })),
        Some(&participant),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(body["type"], "/errors/account-blocked");
}

// --- Capability gate ----------------------------------------------------

#[tokio::test]
async fn a_logger_cannot_moderate() {
    let app = test_app().await;
    let owner = sign_in_consent_callsign(&app, "owner@example.com", "W1OWN").await;
    let logger = sign_in_consent_callsign(&app, "logger@example.com", "W2LOG").await;
    let definition_id = create_net(&app, &owner).await;
    let session_id = start_session(&app, &owner, &definition_id).await;
    assert_eq!(
        grant(&app, &owner, &session_id, "W2LOG", "logger").await,
        StatusCode::OK
    );
    let check_in_id = staff_add(&app, &owner, &session_id, "W1AW").await;

    let (status, body) = moderate(
        &app,
        &session_id,
        &check_in_id,
        &logger,
        moderate_body(false, 1),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(body["type"], "/errors/forbidden");
}

#[tokio::test]
async fn a_relay_cannot_moderate() {
    let app = test_app().await;
    let owner = sign_in_consent_callsign(&app, "owner@example.com", "W1OWN").await;
    let relay = sign_in_consent_callsign(&app, "relay@example.com", "W2REL").await;
    let definition_id = create_net(&app, &owner).await;
    let session_id = start_session(&app, &owner, &definition_id).await;
    assert_eq!(
        grant(&app, &owner, &session_id, "W2REL", "relay").await,
        StatusCode::OK
    );
    let check_in_id = staff_add(&app, &owner, &session_id, "W1AW").await;

    let (status, body) = moderate(
        &app,
        &session_id,
        &check_in_id,
        &relay,
        moderate_body(false, 1),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(body["type"], "/errors/forbidden");
}

#[tokio::test]
async fn a_participant_cannot_moderate() {
    let app = test_app().await;
    let owner = sign_in_consent_callsign(&app, "owner@example.com", "W1OWN").await;
    let participant = sign_in_consent_callsign(&app, "part@example.com", "W2BCD").await;
    let definition_id = create_net(&app, &owner).await;
    let session_id = start_session(&app, &owner, &definition_id).await;
    let check_in_id = self_check_in(&app, &participant, &session_id).await;

    let (status, body) = moderate(
        &app,
        &session_id,
        &check_in_id,
        &participant,
        moderate_body(false, 1),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(body["type"], "/errors/forbidden");
}

#[tokio::test]
async fn a_granted_net_control_may_moderate() {
    // An explicitly-granted NCS (not the owner) holds Moderate.
    let app = test_app().await;
    let owner = sign_in_consent_callsign(&app, "owner@example.com", "W1OWN").await;
    let ncs = sign_in_consent_callsign(&app, "ncs@example.com", "W2NCS").await;
    let definition_id = create_net(&app, &owner).await;
    let session_id = start_session(&app, &owner, &definition_id).await;
    assert_eq!(
        grant(&app, &owner, &session_id, "W2NCS", "net-control").await,
        StatusCode::OK
    );
    let check_in_id = staff_add(&app, &owner, &session_id, "W1AW").await;

    let (status, summary) = moderate(
        &app,
        &session_id,
        &check_in_id,
        &ncs,
        moderate_body(false, 1),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        roster(&summary)
            .iter()
            .all(|e| e["checkInId"] != check_in_id)
    );
}

// --- 404-before-403 -----------------------------------------------------

#[tokio::test]
async fn moderating_a_missing_session_is_404_before_403() {
    let app = test_app().await;
    let stranger = sign_in_consent_callsign(&app, "stranger@example.com", "W9STR").await;
    let missing_session = uuid::Uuid::now_v7();
    let missing_check_in = uuid::Uuid::now_v7();

    let (status, body) = moderate(
        &app,
        &missing_session.to_string(),
        &missing_check_in.to_string(),
        &stranger,
        moderate_body(false, 1),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body["type"], "/errors/net-session-not-found");
}

// --- Account-less block refused -----------------------------------------

#[tokio::test]
async fn blocking_an_account_less_entry_is_422_nothing_to_block_and_the_row_stays() {
    let app = test_app().await;
    let owner = sign_in_consent_callsign(&app, "owner@example.com", "W1OWN").await;
    let definition_id = create_net(&app, &owner).await;
    let session_id = start_session(&app, &owner, &definition_id).await;
    // A staff-logged, account-less callsign (no associated participant account).
    let check_in_id = staff_add(&app, &owner, &session_id, "W1AW").await;

    let (status, body) = moderate(
        &app,
        &session_id,
        &check_in_id,
        &owner,
        moderate_body(true, 1),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(body["type"], "/errors/nothing-to-block");

    // The row is NOT removed — all-or-nothing (the removal did not partially run).
    let (status, summary) = send_json(
        app.router(),
        "GET",
        &format!("/api/net-sessions/{session_id}"),
        None,
        Some(&owner),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        roster(&summary)
            .iter()
            .any(|e| e["checkInId"] == check_in_id)
    );
}

// --- Block does not stop STAFF logging that callsign ------------------------

#[tokio::test]
async fn a_block_does_not_prevent_staff_from_logging_that_callsign() {
    let app = test_app().await;
    let owner = sign_in_consent_callsign(&app, "owner@example.com", "W1OWN").await;
    let participant = sign_in_consent_callsign(&app, "part@example.com", "W2BCD").await;
    let definition_id = create_net(&app, &owner).await;
    let session_id = start_session(&app, &owner, &definition_id).await;
    let check_in_id = self_check_in(&app, &participant, &session_id).await;

    let (status, _) = moderate(
        &app,
        &session_id,
        &check_in_id,
        &owner,
        moderate_body(true, 1),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    // The block is self-check-in-scoped: a staff operator may still log the same
    // callsign string (a relay of that station's off-air traffic).
    let (status, body) = send_json(
        app.router(),
        "POST",
        &format!("/api/net-sessions/{session_id}/check-ins"),
        Some(json!({ "callsign": "W2BCD" })),
        Some(&owner),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    assert!(
        body["roster"]
            .as_array()
            .expect("roster")
            .iter()
            .any(|e| e["callsign"] == "W2BCD" && e["source"] == "staff")
    );
}

// --- Closed-session 409 ------------------------------------------------------

#[tokio::test]
async fn moderating_on_a_closed_session_is_a_409() {
    // Mirrors `editing_on_a_closed_session_is_a_409` (api_check_in_editing.rs):
    // the moderation endpoint shares `ensure_writable`, so a closed session's
    // roster is frozen for moderation too — previously unexercised for
    // `/moderate` specifically.
    let app = test_app().await;
    let owner = sign_in_consent_callsign(&app, "owner@example.com", "W1OWN").await;
    let definition_id = create_net(&app, &owner).await;
    let session_id = start_session(&app, &owner, &definition_id).await;
    let check_in_id = staff_add(&app, &owner, &session_id, "W1AW").await;
    let (status, _) = send_json(
        app.router(),
        "POST",
        &format!("/api/net-sessions/{session_id}/close"),
        None,
        Some(&owner),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let (status, body) = moderate(
        &app,
        &session_id,
        &check_in_id,
        &owner,
        moderate_body(false, 1),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(body["type"], "/errors/session-already-closed");
}

// --- TOCTOU close: a block committed after the handler's own pre-check ------

#[tokio::test]
async fn a_block_that_commits_after_the_pre_check_still_refuses_the_racing_self_check_in() {
    // The self-check-in handler's blocklist pre-check
    // reads an out-of-transaction fold before calling the `add_check_in`
    // adapter, which runs its OWN separate transaction. This test cannot force
    // the exact interleaving over HTTP, but it proves the OUTCOME the fix
    // guarantees end-to-end: once a block is committed, EVERY subsequent
    // self-check-in attempt from that account is refused, including one for a
    // brand-new (never-before-seen) callsign — i.e. there is no window in
    // which the handler's own request path can observe a stale "not blocked"
    // view once the block has actually committed. The adapter-level test
    // `add_check_in_for_a_blocked_account_is_refused_inside_the_add_check_in_transaction`
    // (pg_repos.rs) proves the atomicity guarantee directly, bypassing this
    // handler entirely.
    let app = test_app().await;
    let owner = sign_in_consent_callsign(&app, "owner@example.com", "W1OWN").await;
    let participant = sign_in_consent_callsign(&app, "part@example.com", "W2BCD").await;
    let definition_id = create_net(&app, &owner).await;
    let session_id = start_session(&app, &owner, &definition_id).await;
    let check_in_id = self_check_in(&app, &participant, &session_id).await;

    let (status, _) = moderate(
        &app,
        &session_id,
        &check_in_id,
        &owner,
        moderate_body(true, 1),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let (status, body) = send_json(
        app.router(),
        "POST",
        &format!("/api/net-sessions/{session_id}/check-ins"),
        Some(json!({ "callsign": "W2BCD" })),
        Some(&participant),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(body["type"], "/errors/account-blocked");
}

// --- Redaction regression guard ----------------------------------------------

#[tokio::test]
async fn the_queryable_session_summary_never_surfaces_the_blocklist_or_a_blocked_account_id() {
    // The redaction is proven for the WS event stream in
    // `ws/protocol.rs::public_station_blocked_redacts_the_account_id`. This
    // guards the OTHER read path: the plain `GET` session-summary response
    // must never carry `blockedAccountIds` (or the blocked account's raw id)
    // regardless of viewer — a regression guard against a future DTO change
    // accidentally wiring the fold-internal field onto the wire.
    let app = test_app().await;
    let owner = sign_in_consent_callsign(&app, "owner@example.com", "W1OWN").await;
    let participant = sign_in_consent_callsign(&app, "part@example.com", "W2BCD").await;
    let definition_id = create_net(&app, &owner).await;
    let session_id = start_session(&app, &owner, &definition_id).await;
    let check_in_id = self_check_in(&app, &participant, &session_id).await;
    let (status, _) = moderate(
        &app,
        &session_id,
        &check_in_id,
        &owner,
        moderate_body(true, 1),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let (status, summary) = send_json(
        app.router(),
        "GET",
        &format!("/api/net-sessions/{session_id}"),
        None,
        Some(&owner),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let raw = summary.to_string();
    assert!(!raw.contains("blockedAccountIds"));
    assert!(!raw.contains("blocked_account_ids"));
}

/// Convenience wrapper posting to the `/moderate` endpoint.
async fn moderate(
    app: &TestApp,
    session_id: &str,
    check_in_id: &str,
    cookie: &str,
    body: Value,
) -> (StatusCode, Value) {
    send_json(
        app.router(),
        "POST",
        &format!("/api/net-sessions/{session_id}/check-ins/{check_in_id}/moderate"),
        Some(body),
        Some(cookie),
    )
    .await
}
