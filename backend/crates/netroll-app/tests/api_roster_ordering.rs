// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
//! Full-loop API tests for the precedence-set edit path and the NCS
//! roster-reorder command: real router, real Postgres
//! (testcontainers), capturing fake mailer. Asserts status codes, problem+json
//! `type` slugs, and folded roster order/precedence — never message prose
//! (house TDD rule).

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

/// Adds a callsign-only check-in and returns its `checkInId`.
async fn add_check_in(app: &TestApp, cookie: &str, session_id: &str, callsign: &str) -> String {
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

fn roster_order(summary: &Value) -> Vec<String> {
    summary["roster"]
        .as_array()
        .expect("roster")
        .iter()
        .map(|e| e["callsign"].as_str().expect("callsign").to_owned())
        .collect()
}

// --- Precedence via the edit path -------------------------------------------

#[tokio::test]
async fn an_edit_setting_precedence_and_traffic_folds_onto_the_roster_with_a_correction() {
    let app = test_app().await;
    let owner = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;
    let definition_id = create_net(&app, &owner).await;
    let session_id = start_session(&app, &owner, &definition_id).await;
    let check_in_id = add_check_in(&app, &owner, &session_id, "W9XYZ").await;

    let (status, body) = send_json(
        app.router(),
        "PUT",
        &format!("/api/net-sessions/{session_id}/check-ins/{check_in_id}"),
        Some(json!({
            "callsign": "W9XYZ",
            "precedence": "emergency",
            "traffic": 3,
            "expectedVersion": 1
        })),
        Some(&owner),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let entry = body["roster"]
        .as_array()
        .expect("roster")
        .iter()
        .find(|e| e["checkInId"] == check_in_id.as_str())
        .expect("the edited row");
    assert_eq!(entry["precedence"], "emergency");
    assert_eq!(entry["traffic"], 3);
    let corr = entry["corrections"].as_array().expect("corrections");
    assert!(
        corr.iter().any(|c| c["field"] == "precedence"
            && c["from"] == "routine"
            && c["to"] == "emergency")
    );
    assert!(
        corr.iter()
            .any(|c| c["field"] == "traffic" && c["to"] == "3")
    );
}

#[tokio::test]
async fn a_participant_cannot_set_precedence_via_the_edit_endpoint() {
    let app = test_app().await;
    let owner = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;
    let participant = sign_in_consent(&app, "participant@example.com").await;
    let definition_id = create_net(&app, &owner).await;
    let session_id = start_session(&app, &owner, &definition_id).await;
    let check_in_id = add_check_in(&app, &owner, &session_id, "W9XYZ").await;

    let (status, body) = send_json(
        app.router(),
        "PUT",
        &format!("/api/net-sessions/{session_id}/check-ins/{check_in_id}"),
        Some(json!({ "callsign": "W9XYZ", "precedence": "emergency", "expectedVersion": 1 })),
        Some(&participant),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(body["type"], "/errors/forbidden");
}

#[tokio::test]
async fn a_relay_cannot_set_precedence_via_the_edit_endpoint() {
    let app = test_app().await;
    let owner = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;
    let relay = sign_in_consent_callsign(&app, "relay@example.com", "w2bcd").await;
    let definition_id = create_net(&app, &owner).await;
    let session_id = start_session(&app, &owner, &definition_id).await;
    grant(&app, &owner, &session_id, "w2bcd", "relay").await;
    let check_in_id = add_check_in(&app, &owner, &session_id, "W9XYZ").await;

    let (status, _) = send_json(
        app.router(),
        "PUT",
        &format!("/api/net-sessions/{session_id}/check-ins/{check_in_id}"),
        Some(json!({ "callsign": "W9XYZ", "precedence": "priority", "expectedVersion": 1 })),
        Some(&relay),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn an_unknown_precedence_token_on_edit_is_a_400() {
    let app = test_app().await;
    let owner = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;
    let definition_id = create_net(&app, &owner).await;
    let session_id = start_session(&app, &owner, &definition_id).await;
    let check_in_id = add_check_in(&app, &owner, &session_id, "W9XYZ").await;

    let (status, body) = send_json(
        app.router(),
        "PUT",
        &format!("/api/net-sessions/{session_id}/check-ins/{check_in_id}"),
        Some(json!({ "callsign": "W9XYZ", "precedence": "urgent", "expectedVersion": 1 })),
        Some(&owner),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["type"], "/errors/precedence-invalid");
}

#[tokio::test]
async fn a_traffic_count_over_the_bound_on_edit_is_a_400() {
    let app = test_app().await;
    let owner = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;
    let definition_id = create_net(&app, &owner).await;
    let session_id = start_session(&app, &owner, &definition_id).await;
    let check_in_id = add_check_in(&app, &owner, &session_id, "W9XYZ").await;

    let (status, body) = send_json(
        app.router(),
        "PUT",
        &format!("/api/net-sessions/{session_id}/check-ins/{check_in_id}"),
        Some(json!({ "callsign": "W9XYZ", "traffic": 1000, "expectedVersion": 1 })),
        Some(&owner),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["type"], "/errors/traffic-invalid");
}

// --- The reorder command ----------------------------------------------------

/// Adds three check-ins A/B/C, promotes C to emergency + B to priority, then
/// returns `(session_id, [idA, idB, idC])`.
async fn seed_mixed_precedence(app: &TestApp, cookie: &str) -> (String, [String; 3]) {
    let definition_id = create_net(app, cookie).await;
    let session_id = start_session(app, cookie, &definition_id).await;
    let id_a = add_check_in(app, cookie, &session_id, "W1AAA").await;
    let id_b = add_check_in(app, cookie, &session_id, "W1BBB").await;
    let id_c = add_check_in(app, cookie, &session_id, "W1CCC").await;
    // C -> emergency, B -> priority; A stays routine.
    let (status, _) = send_json(
        app.router(),
        "PUT",
        &format!("/api/net-sessions/{session_id}/check-ins/{id_c}"),
        Some(json!({ "callsign": "W1CCC", "precedence": "emergency", "traffic": 3, "expectedVersion": 1 })),
        Some(cookie),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (status, _) = send_json(
        app.router(),
        "PUT",
        &format!("/api/net-sessions/{session_id}/check-ins/{id_b}"),
        Some(json!({ "callsign": "W1BBB", "precedence": "priority", "expectedVersion": 1 })),
        Some(cookie),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    (session_id, [id_a, id_b, id_c])
}

#[tokio::test]
async fn the_ncs_reorders_the_roster_emergency_priority_routine() {
    let app = test_app().await;
    let owner = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;
    let (session_id, _ids) = seed_mixed_precedence(&app, &owner).await;

    // Before the reorder the roster is in insertion order A, B, C.
    let (status, before) = send_json(
        app.router(),
        "GET",
        &format!("/api/net-sessions/{session_id}"),
        None,
        Some(&owner),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(roster_order(&before), vec!["W1AAA", "W1BBB", "W1CCC"]);

    let (status, summary) = send_json(
        app.router(),
        "POST",
        &format!("/api/net-sessions/{session_id}/reorder"),
        Some(json!({ "by": "precedence" })),
        Some(&owner),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    // Emergency (C) → Priority (B) → Routine (A).
    assert_eq!(roster_order(&summary), vec!["W1CCC", "W1BBB", "W1AAA"]);
}

#[tokio::test]
async fn a_granted_net_control_may_reorder() {
    let app = test_app().await;
    let owner = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;
    let ncs = sign_in_consent_callsign(&app, "ncs@example.com", "w2bcd").await;
    let (session_id, _ids) = seed_mixed_precedence(&app, &owner).await;
    grant(&app, &owner, &session_id, "w2bcd", "net-control").await;

    let (status, summary) = send_json(
        app.router(),
        "POST",
        &format!("/api/net-sessions/{session_id}/reorder"),
        Some(json!({ "by": "precedence" })),
        Some(&ncs),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(roster_order(&summary), vec!["W1CCC", "W1BBB", "W1AAA"]);
}

#[tokio::test]
async fn a_logger_cannot_reorder_the_roster() {
    let app = test_app().await;
    let owner = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;
    let logger = sign_in_consent_callsign(&app, "logger@example.com", "w2bcd").await;
    let definition_id = create_net(&app, &owner).await;
    let session_id = start_session(&app, &owner, &definition_id).await;
    grant(&app, &owner, &session_id, "w2bcd", "logger").await;

    let (status, body) = send_json(
        app.router(),
        "POST",
        &format!("/api/net-sessions/{session_id}/reorder"),
        Some(json!({ "by": "precedence" })),
        Some(&logger),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(body["type"], "/errors/forbidden");
}

#[tokio::test]
async fn a_participant_cannot_reorder_the_roster() {
    let app = test_app().await;
    let owner = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;
    let participant = sign_in_consent(&app, "participant@example.com").await;
    let definition_id = create_net(&app, &owner).await;
    let session_id = start_session(&app, &owner, &definition_id).await;

    let (status, _) = send_json(
        app.router(),
        "POST",
        &format!("/api/net-sessions/{session_id}/reorder"),
        Some(json!({ "by": "precedence" })),
        Some(&participant),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn reordering_a_missing_session_is_404_before_403() {
    let app = test_app().await;
    let _owner = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;
    let stranger = sign_in_consent(&app, "stranger@example.com").await;
    let missing = uuid::Uuid::now_v7();

    let (status, body) = send_json(
        app.router(),
        "POST",
        &format!("/api/net-sessions/{missing}/reorder"),
        Some(json!({ "by": "precedence" })),
        Some(&stranger),
    )
    .await;
    // A non-member gets 404 (existence decided first), never a 403 that would
    // confirm the session exists.
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body["type"], "/errors/net-session-not-found");
}

#[tokio::test]
async fn reordering_a_closed_session_is_409() {
    let app = test_app().await;
    let owner = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;
    let definition_id = create_net(&app, &owner).await;
    let session_id = start_session(&app, &owner, &definition_id).await;
    let (status, _) = send_json(
        app.router(),
        "POST",
        &format!("/api/net-sessions/{session_id}/close"),
        None,
        Some(&owner),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let (status, body) = send_json(
        app.router(),
        "POST",
        &format!("/api/net-sessions/{session_id}/reorder"),
        Some(json!({ "by": "precedence" })),
        Some(&owner),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(body["type"], "/errors/session-already-closed");
}

#[tokio::test]
async fn an_unknown_reorder_strategy_is_400() {
    let app = test_app().await;
    let owner = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;
    let definition_id = create_net(&app, &owner).await;
    let session_id = start_session(&app, &owner, &definition_id).await;

    let (status, body) = send_json(
        app.router(),
        "POST",
        &format!("/api/net-sessions/{session_id}/reorder"),
        Some(json!({ "by": "callsign" })),
        Some(&owner),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["type"], "/errors/validation");
}

#[tokio::test]
async fn the_public_snapshot_after_a_reorder_shows_the_new_order_and_the_precedence() {
    let app = test_app().await;
    let owner = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;
    let (session_id, _ids) = seed_mixed_precedence(&app, &owner).await;

    let (status, _) = send_json(
        app.router(),
        "POST",
        &format!("/api/net-sessions/{session_id}/reorder"),
        Some(json!({ "by": "precedence" })),
        Some(&owner),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    // AMENDED. The account-less public view still
    // reflects the reordered roster (assertion below is UNTOUCHED),
    // and it now also carries the precedence LABEL that produced that order, plus
    // the declared traffic count. An observer once saw the order without
    // the reason for it.
    let (status, public) = send_json(
        app.router(),
        "GET",
        &format!("/api/net-sessions/{session_id}/live"),
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(roster_order(&public), vec!["W1CCC", "W1BBB", "W1AAA"]);
    let public_precedence: Vec<&str> = public["roster"]
        .as_array()
        .expect("roster")
        .iter()
        .map(|e| e["precedence"].as_str().expect("precedence"))
        .collect();
    assert_eq!(
        public_precedence,
        vec!["emergency", "priority", "routine"],
        "each public entry carries the precedence the operator set, not `routine` for all three"
    );
    let emergency = &public["roster"].as_array().expect("roster")[0];
    assert_eq!(
        emergency["traffic"], 3,
        "the declared traffic count reaches the observer"
    );
    // STILL TRUE: four fields moved onto the public roster
    // entry, and every roster-entry field outside `PublicRosterEntry`'s carried
    // key set (enumerated in its doc, `http/net_sessions.rs`) did not — those
    // are `build_public_view`'s `_`-bound destructure arms, and how many there
    // are is asserted by
    // `roster_projection_sites.rs`'s `GUARDED` register rather than stated here
    // The report, the staff note and the operator ids are three of
    // them and stay redacted here.
    for entry in public["roster"].as_array().expect("roster") {
        let obj = entry.as_object().expect("entry object");
        assert!(!obj.contains_key("signalReport"), "still no signalReport");
        assert!(!obj.contains_key("notes"), "still no STAFF note");
        assert!(!obj.contains_key("addedBy"), "still no addedBy");
    }
}

// --- The persistent worked-sink ordering mode ------------------

/// Sets the session's roster ordering mode through the endpoint, returning
/// (status, body).
async fn set_order_mode(
    app: &TestApp,
    cookie: &str,
    session_id: &str,
    mode: &str,
) -> (StatusCode, Value) {
    send_json(
        app.router(),
        "POST",
        &format!("/api/net-sessions/{session_id}/roster-order-mode"),
        Some(json!({ "mode": mode })),
        Some(cookie),
    )
    .await
}

/// Moves the working-station cursor through the endpoint.
async fn set_worked(
    app: &TestApp,
    cookie: &str,
    session_id: &str,
    check_in_id: Option<&str>,
) -> (StatusCode, Value) {
    send_json(
        app.router(),
        "POST",
        &format!("/api/net-sessions/{session_id}/worked-station"),
        Some(json!({ "checkInId": check_in_id })),
        Some(cookie),
    )
    .await
}

#[tokio::test]
async fn the_ncs_enables_worked_sink_and_the_folded_summary_reports_it() {
    let app = test_app().await;
    let owner = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;
    let (session_id, _ids) = seed_mixed_precedence(&app, &owner).await;

    let (status, before) = send_json(
        app.router(),
        "GET",
        &format!("/api/net-sessions/{session_id}"),
        None,
        Some(&owner),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(before["rosterOrderMode"], "manual");

    let (status, summary) = set_order_mode(&app, &owner, &session_id, "worked-sink").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(summary["rosterOrderMode"], "worked-sink");

    // The mode is durable — a fresh read of the folded summary still has it.
    let (status, reread) = send_json(
        app.router(),
        "GET",
        &format!("/api/net-sessions/{session_id}"),
        None,
        Some(&owner),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(reread["rosterOrderMode"], "worked-sink");
}

#[tokio::test]
async fn a_worked_station_sinks_in_the_shared_order_without_a_second_command() {
    // The mode is PERSISTENT. Working A then B then C sinks each station as
    // it happens, with no further operator action.
    let app = test_app().await;
    let owner = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;
    let (session_id, ids) = seed_mixed_precedence(&app, &owner).await;
    let (status, _) = set_order_mode(&app, &owner, &session_id, "worked-sink").await;
    assert_eq!(status, StatusCode::OK);

    let (status, _) = set_worked(&app, &owner, &session_id, Some(&ids[0])).await;
    assert_eq!(status, StatusCode::OK);
    let (status, summary) = set_worked(&app, &owner, &session_id, Some(&ids[1])).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(roster_order(&summary), vec!["W1BBB", "W1CCC", "W1AAA"]);

    // Working C sinks B too. The worked group keeps the relative order it HELD,
    // which after the first sink is B-above-A — the stable partition preserves
    // whatever it is handed rather than imposing a worked-at ordering.
    let (status, summary) = set_worked(&app, &owner, &session_id, Some(&ids[2])).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(roster_order(&summary), vec!["W1CCC", "W1BBB", "W1AAA"]);
}

#[tokio::test]
async fn the_mode_survives_the_operator_who_set_it_and_binds_the_next_one() {
    // The mode is DURABLE session state, not one console's React state. A
    // DIFFERENT operator, on a session they did not configure, works a station
    // and it sinks — nobody re-enables anything.
    let app = test_app().await;
    let owner = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;
    let ncs = sign_in_consent_callsign(&app, "ncs@example.com", "w2bcd").await;
    let (session_id, ids) = seed_mixed_precedence(&app, &owner).await;
    grant(&app, &owner, &session_id, "w2bcd", "net-control").await;

    let (status, _) = set_order_mode(&app, &owner, &session_id, "worked-sink").await;
    assert_eq!(status, StatusCode::OK);

    // The second operator's own folded summary already reports the mode.
    let (status, seen_by_ncs) = send_json(
        app.router(),
        "GET",
        &format!("/api/net-sessions/{session_id}"),
        None,
        Some(&ncs),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(seen_by_ncs["rosterOrderMode"], "worked-sink");

    // And their worked-station calls sink, with no mode command of their own.
    let (status, _) = set_worked(&app, &ncs, &session_id, Some(&ids[0])).await;
    assert_eq!(status, StatusCode::OK);
    let (status, summary) = set_worked(&app, &ncs, &session_id, Some(&ids[1])).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(roster_order(&summary), vec!["W1BBB", "W1CCC", "W1AAA"]);
}

#[tokio::test]
async fn a_granted_net_control_may_set_the_roster_order_mode() {
    let app = test_app().await;
    let owner = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;
    let ncs = sign_in_consent_callsign(&app, "ncs@example.com", "w2bcd").await;
    let (session_id, _ids) = seed_mixed_precedence(&app, &owner).await;
    grant(&app, &owner, &session_id, "w2bcd", "net-control").await;

    let (status, summary) = set_order_mode(&app, &ncs, &session_id, "worked-sink").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(summary["rosterOrderMode"], "worked-sink");
}

#[tokio::test]
async fn a_logger_cannot_set_the_roster_order_mode() {
    let app = test_app().await;
    let owner = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;
    let logger = sign_in_consent_callsign(&app, "logger@example.com", "w2bcd").await;
    let definition_id = create_net(&app, &owner).await;
    let session_id = start_session(&app, &owner, &definition_id).await;
    grant(&app, &owner, &session_id, "w2bcd", "logger").await;

    let (status, body) = set_order_mode(&app, &logger, &session_id, "worked-sink").await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(body["type"], "/errors/forbidden");
}

#[tokio::test]
async fn a_relay_cannot_set_the_roster_order_mode() {
    let app = test_app().await;
    let owner = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;
    let relay = sign_in_consent_callsign(&app, "relay@example.com", "w2bcd").await;
    let definition_id = create_net(&app, &owner).await;
    let session_id = start_session(&app, &owner, &definition_id).await;
    grant(&app, &owner, &session_id, "w2bcd", "relay").await;

    let (status, body) = set_order_mode(&app, &relay, &session_id, "worked-sink").await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(body["type"], "/errors/forbidden");
}

#[tokio::test]
async fn a_participant_cannot_set_the_roster_order_mode() {
    let app = test_app().await;
    let owner = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;
    let participant = sign_in_consent(&app, "participant@example.com").await;
    let definition_id = create_net(&app, &owner).await;
    let session_id = start_session(&app, &owner, &definition_id).await;

    let (status, _) = set_order_mode(&app, &participant, &session_id, "worked-sink").await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn setting_the_order_mode_on_a_missing_session_is_404_before_403() {
    let app = test_app().await;
    let _owner = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;
    let stranger = sign_in_consent(&app, "stranger@example.com").await;
    let missing = uuid::Uuid::now_v7().to_string();

    let (status, body) = set_order_mode(&app, &stranger, &missing, "worked-sink").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body["type"], "/errors/net-session-not-found");
}

#[tokio::test]
async fn setting_the_order_mode_on_a_closed_session_is_409() {
    let app = test_app().await;
    let owner = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;
    let definition_id = create_net(&app, &owner).await;
    let session_id = start_session(&app, &owner, &definition_id).await;
    let (status, _) = send_json(
        app.router(),
        "POST",
        &format!("/api/net-sessions/{session_id}/close"),
        None,
        Some(&owner),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let (status, body) = set_order_mode(&app, &owner, &session_id, "worked-sink").await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(body["type"], "/errors/session-already-closed");
}

#[tokio::test]
async fn an_unknown_roster_order_mode_is_400() {
    let app = test_app().await;
    let owner = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;
    let definition_id = create_net(&app, &owner).await;
    let session_id = start_session(&app, &owner, &definition_id).await;

    let (status, body) = set_order_mode(&app, &owner, &session_id, "shuffle").await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["type"], "/errors/validation");
}

#[tokio::test]
async fn a_check_in_arriving_under_worked_sink_lands_above_the_worked_block() {
    // The most frequent trigger: the fold appends a new check-in at the END,
    // which is BELOW the worked block — the command boundary must re-sink it.
    let app = test_app().await;
    let owner = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;
    let (session_id, ids) = seed_mixed_precedence(&app, &owner).await;
    let (status, _) = set_order_mode(&app, &owner, &session_id, "worked-sink").await;
    assert_eq!(status, StatusCode::OK);
    let (status, _) = set_worked(&app, &owner, &session_id, Some(&ids[0])).await;
    assert_eq!(status, StatusCode::OK);
    let (status, _) = set_worked(&app, &owner, &session_id, Some(&ids[1])).await;
    assert_eq!(status, StatusCode::OK);

    add_check_in(&app, &owner, &session_id, "W1DDD").await;
    let (status, summary) = send_json(
        app.router(),
        "GET",
        &format!("/api/net-sessions/{session_id}"),
        None,
        Some(&owner),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        roster_order(&summary),
        vec!["W1BBB", "W1CCC", "W1DDD", "W1AAA"]
    );
}

#[tokio::test]
async fn the_public_snapshot_after_a_sink_shows_the_new_order_and_the_precedence() {
    // The order is SHARED — an account-less viewer sees exactly
    // the order the operator console does. That assertion is UNTOUCHED below.
    // The precedence labels cross with the order they produced.
    let app = test_app().await;
    let owner = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;
    let (session_id, ids) = seed_mixed_precedence(&app, &owner).await;
    let (status, _) = set_order_mode(&app, &owner, &session_id, "worked-sink").await;
    assert_eq!(status, StatusCode::OK);
    let (status, _) = set_worked(&app, &owner, &session_id, Some(&ids[0])).await;
    assert_eq!(status, StatusCode::OK);
    let (status, staff) = set_worked(&app, &owner, &session_id, Some(&ids[1])).await;
    assert_eq!(status, StatusCode::OK);

    let (status, public) = send_json(
        app.router(),
        "GET",
        &format!("/api/net-sessions/{session_id}/live"),
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(roster_order(&public), roster_order(&staff));
    assert_eq!(roster_order(&public), vec!["W1BBB", "W1CCC", "W1AAA"]);
    for entry in public["roster"].as_array().expect("roster") {
        let obj = entry.as_object().expect("entry object");
        assert!(
            obj.contains_key("precedence"),
            "every public entry carries its precedence"
        );
        // STILL TRUE: the report and the staff note did not move.
        assert!(!obj.contains_key("signalReport"), "still no signalReport");
        assert!(!obj.contains_key("notes"), "still no STAFF note");
    }
    // This assertion used to pin that the public view gains no ordering-mode
    // field. The public page's your-turn selector now
    // needs to know which ordering is in force, and the mode names an order the
    // viewer can already see — the roster above arrived in it.
    assert_eq!(
        public["rosterOrderMode"], "worked-sink",
        "the public view carries the session's real ordering mode"
    );
}
