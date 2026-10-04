// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
//! Full-loop API tests for start/close/summary of a net session:
//! real router, real Postgres (testcontainers), capturing fake mailer. Asserts
//! status codes, problem+json `type` slugs, body values, and DB side-effects —
//! never message prose (house TDD rule).

use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use netroll_app::http::{AppState, api_router};
use netroll_app::presence_monitor::run_presence_monitor_tick;
use netroll_domain::consent::CURRENT_TERMS_VERSION;
use netroll_domain::ports::{BoxFuture, Clock, MailError, Mailer};
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

/// A clock whose value the test drives between HTTP requests via a shared
/// atomic — lets the clamp test script a backward wall-clock step.
struct HandleClock {
    millis: Arc<AtomicU64>,
}

impl Clock for HandleClock {
    fn now_epoch_millis(&self) -> u64 {
        self.millis.load(Ordering::SeqCst)
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
    let content_type = response
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_owned();
    let bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024)
        .await
        .expect("body");
    let mut json = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).expect("body is JSON")
    };
    // Surface the content-type to error-shape assertions without a second
    // request: problem+json bodies must carry the RFC 9457 media type.
    if let Value::Object(ref mut map) = json {
        map.insert("__contentType".into(), Value::String(content_type));
    }
    (status, json)
}

/// Signs in a fresh email, records consent, and reserves a callsign — the full
/// gate an owner needs to create a net and start a session.
async fn sign_in_consent_callsign(app: &TestApp, email: &str, callsign: &str) -> String {
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

fn full_definition_json() -> Value {
    json!({
        "title": "Sunday Traffic Net",
        "description": "Weekly NTS traffic",
        "connections": [
            { "kind": "hf", "plannedFrequencyHz": 14_230_000, "band": "20m", "mode": "ssb" }
        ],
        "country": "USA",
        "state": "CT",
        "grid": "fn31pr",
        "netCategory": "traffic",
        "netType": "open",
        "expectedDuration": "90"
    })
}

/// Creates a net definition owned by `cookie`'s account; returns its id and the
/// server-assigned `definitionVersion`.
async fn create_net(app: &TestApp, cookie: &str) -> (String, i64) {
    let (status, body) = send_json(
        app.router(),
        "POST",
        "/api/net-definitions",
        Some(full_definition_json()),
        Some(cookie),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    (
        body["id"].as_str().expect("definition id").to_owned(),
        body["definitionVersion"].as_i64().expect("version"),
    )
}

/// The id of the session's FIRST frozen connection — what a `frequency.changed`
/// names. Read from the stored snapshot, so it is the same id
/// every read surface renders.
async fn first_connection_id(app: &TestApp, session_id: &str) -> String {
    let snapshot: Value =
        sqlx::query_scalar("SELECT definition_snapshot FROM net_sessions WHERE id = $1")
            .bind(uuid::Uuid::parse_str(session_id).expect("uuid"))
            .fetch_one(&app.pool)
            .await
            .expect("stored snapshot");
    snapshot["connections"][0]["id"]
        .as_str()
        .expect("the snapshot carries its connection ids")
        .to_owned()
}

async fn session_count(app: &TestApp) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM net_sessions")
        .fetch_one(&app.pool)
        .await
        .expect("count sessions")
}

async fn event_count(app: &TestApp, kind: &str) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM session_events WHERE kind = $1")
        .bind(kind)
        .fetch_one(&app.pool)
        .await
        .expect("count events")
}

#[tokio::test]
async fn owner_starts_a_session_and_gets_a_folded_live_summary() {
    let app = test_app().await;
    let cookie = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;
    let (definition_id, version) = create_net(&app, &cookie).await;

    let (status, body) = send_json(
        app.router(),
        "POST",
        "/api/net-sessions",
        Some(json!({ "definitionId": definition_id })),
        Some(&cookie),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(body["lifecycle"], "live");
    // The session carries its ways in, not one frequency.
    assert_eq!(body["connections"][0]["kind"], "hf");
    assert_eq!(body["connections"][0]["plannedFrequencyHz"], 14_230_000);
    assert!(body.get("operatingFrequencyHz").is_none());
    assert_eq!(body["latestSeq"], 1);
    assert_eq!(body["definitionId"], definition_id);
    assert_eq!(body["definitionVersion"], version);
    assert_eq!(body["definition"]["title"], "Sunday Traffic Net");
    assert_eq!(body["participantCount"], 0);
    assert_eq!(body["roster"].as_array().expect("roster").len(), 0);
    assert!(body["startedAt"].is_string(), "startedAt present");
    assert!(body["closedAt"].is_null(), "a live session has no closedAt");

    // DB side-effects: exactly one net_sessions row, live, last_seq=1, and
    // exactly one session.started event at seq=1 — the atomic start invariant.
    assert_eq!(session_count(&app).await, 1);
    let (lifecycle, last_seq): (String, i64) =
        sqlx::query_as("SELECT lifecycle, last_seq FROM net_sessions")
            .fetch_one(&app.pool)
            .await
            .expect("session row");
    assert_eq!(lifecycle, "live");
    assert_eq!(last_seq, 1);
    let (seq, kind): (i64, String) = sqlx::query_as("SELECT seq, kind FROM session_events")
        .fetch_one(&app.pool)
        .await
        .expect("event row");
    assert_eq!(seq, 1);
    assert_eq!(kind, "session.started");

    // Start never edits the source definition (last clause).
    let def_version: i32 =
        sqlx::query_scalar("SELECT definition_version FROM net_definitions WHERE id = $1")
            .bind(uuid::Uuid::parse_str(&definition_id).unwrap())
            .fetch_one(&app.pool)
            .await
            .expect("definition version");
    assert_eq!(
        def_version as i64, version,
        "start leaves the definition untouched"
    );
}

#[tokio::test]
async fn a_mid_session_move_leaves_the_definitions_planned_frequency_alone() {
    // The operating frequency may differ from the planned one: the difference
    // lives on a CONNECTION, and the definition it was copied from is untouched.
    let app = test_app().await;
    let cookie = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;
    // Started on THIS definition, not on one `fresh_live_session` mints for
    // itself: the assertion below reads `net_definitions` by `definition_id`,
    // and a session running on some other row makes it pass without ever
    // touching the thing it claims is untouched.
    let (definition_id, _version) = create_net(&app, &cookie).await;
    let (status, body) = send_json(
        app.router(),
        "POST",
        "/api/net-sessions",
        Some(json!({ "definitionId": definition_id })),
        Some(&cookie),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let session_id = body["id"].as_str().expect("session id").to_owned();
    let connection_id = first_connection_id(&app, &session_id).await;

    let (status, body) = send_json(
        app.router(),
        "POST",
        &format!("/api/net-sessions/{session_id}/frequency"),
        Some(json!({ "connectionId": connection_id, "operatingFrequency": "7.200" })),
        Some(&cookie),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["connections"][0]["plannedFrequencyHz"], 7_200_000);

    // The definition's planned frequency lives on its connection row; a
    // session's mid-run move is a log event and never reaches it.
    let planned: i64 = sqlx::query_scalar(
        "SELECT planned_frequency_hz FROM net_connections
          WHERE definition_id = $1 AND position = 0",
    )
    .bind(uuid::Uuid::parse_str(&definition_id).unwrap())
    .fetch_one(&app.pool)
    .await
    .expect("planned frequency");
    assert_eq!(
        planned, 14_230_000,
        "the definition's connection row is untouched"
    );
}

#[tokio::test]
async fn a_non_owner_cannot_start_a_session_and_no_row_is_written() {
    let app = test_app().await;
    let owner = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;
    let (definition_id, _version) = create_net(&app, &owner).await;
    let stranger = sign_in_consent_callsign(&app, "stranger@example.com", "n1ale").await;

    let (status, body) = send_json(
        app.router(),
        "POST",
        "/api/net-sessions",
        Some(json!({ "definitionId": definition_id })),
        Some(&stranger),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(body["type"], "/errors/forbidden");
    assert_eq!(body["__contentType"], "application/problem+json");
    assert_eq!(
        session_count(&app).await,
        0,
        "no session for a non-owner start"
    );
}

#[tokio::test]
async fn starting_an_archived_definition_is_refused_with_not_found() {
    let app = test_app().await;
    let cookie = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;
    let (definition_id, _version) = create_net(&app, &cookie).await;

    // Archive the net (owner delete = archive).
    let (status, _) = send_json(
        app.router(),
        "DELETE",
        &format!("/api/net-definitions/{definition_id}"),
        None,
        Some(&cookie),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    let (status, body) = send_json(
        app.router(),
        "POST",
        "/api/net-sessions",
        Some(json!({ "definitionId": definition_id })),
        Some(&cookie),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body["type"], "/errors/net-definition-not-found");
    assert_eq!(
        session_count(&app).await,
        0,
        "an archived net spawns no run"
    );
}

#[tokio::test]
async fn starting_a_second_session_for_an_already_live_definition_is_a_409_conflict() {
    // A definition may have at
    // most one concurrently-live session — enforced end-to-end here via the
    // `net_sessions_one_live_per_definition` partial unique index, not just
    // documented as a forward item.
    let app = test_app().await;
    let cookie = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;
    let (definition_id, _version) = create_net(&app, &cookie).await;

    let (status, _) = send_json(
        app.router(),
        "POST",
        "/api/net-sessions",
        Some(json!({ "definitionId": definition_id })),
        Some(&cookie),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(session_count(&app).await, 1);

    let (status, body) = send_json(
        app.router(),
        "POST",
        "/api/net-sessions",
        Some(json!({ "definitionId": definition_id })),
        Some(&cookie),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(body["type"], "/errors/session-already-live");
    assert_eq!(body["__contentType"], "application/problem+json");
    assert_eq!(
        session_count(&app).await,
        1,
        "the refused second start leaves exactly one session"
    );
}

#[tokio::test]
async fn owner_closes_a_live_session_and_gets_a_folded_summary() {
    let app = test_app().await;
    let cookie = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;
    let (definition_id, _version) = create_net(&app, &cookie).await;

    let (_status, start_body) = send_json(
        app.router(),
        "POST",
        "/api/net-sessions",
        Some(json!({ "definitionId": definition_id })),
        Some(&cookie),
    )
    .await;
    let session_id = start_body["id"].as_str().expect("session id").to_owned();

    let (status, body) = send_json(
        app.router(),
        "POST",
        &format!("/api/net-sessions/{session_id}/close"),
        None,
        Some(&cookie),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["lifecycle"], "closed");
    assert_eq!(body["latestSeq"], 2);
    assert!(body["closedAt"].is_string(), "closedAt present after close");
    let started = body["startedAt"].as_str().expect("startedAt");
    let closed = body["closedAt"].as_str().expect("closedAt");
    assert!(closed >= started, "closedAt >= startedAt");
    assert!(
        body["durationSeconds"].as_i64().expect("durationSeconds") >= 0,
        "duration is never negative"
    );

    // DB: lifecycle flipped, closed_at set, session.closed at seq=2.
    let (lifecycle, last_seq, closed_at_present): (String, i64, bool) = sqlx::query_as(
        "SELECT lifecycle, last_seq, closed_at IS NOT NULL FROM net_sessions WHERE id = $1",
    )
    .bind(uuid::Uuid::parse_str(&session_id).unwrap())
    .fetch_one(&app.pool)
    .await
    .expect("session row");
    assert_eq!(lifecycle, "closed");
    assert_eq!(last_seq, 2);
    assert!(closed_at_present, "closed_at column is set");
    assert_eq!(event_count(&app, "session.closed").await, 1);
}

#[tokio::test]
async fn a_non_owner_cannot_close_a_session_and_it_stays_live() {
    let app = test_app().await;
    let owner = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;
    let (definition_id, _version) = create_net(&app, &owner).await;
    let stranger = sign_in_consent_callsign(&app, "stranger@example.com", "n1ale").await;

    let (_status, start_body) = send_json(
        app.router(),
        "POST",
        "/api/net-sessions",
        Some(json!({ "definitionId": definition_id })),
        Some(&owner),
    )
    .await;
    let session_id = start_body["id"].as_str().expect("session id").to_owned();

    let (status, body) = send_json(
        app.router(),
        "POST",
        &format!("/api/net-sessions/{session_id}/close"),
        None,
        Some(&stranger),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(body["type"], "/errors/forbidden");

    let lifecycle: String = sqlx::query_scalar("SELECT lifecycle FROM net_sessions WHERE id = $1")
        .bind(uuid::Uuid::parse_str(&session_id).unwrap())
        .fetch_one(&app.pool)
        .await
        .expect("session row");
    assert_eq!(
        lifecycle, "live",
        "a rejected close leaves the session live"
    );
    assert_eq!(event_count(&app, "session.closed").await, 0);
}

#[tokio::test]
async fn closing_an_already_closed_session_is_a_409_conflict() {
    let app = test_app().await;
    let cookie = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;
    let (definition_id, _version) = create_net(&app, &cookie).await;

    let (_status, start_body) = send_json(
        app.router(),
        "POST",
        "/api/net-sessions",
        Some(json!({ "definitionId": definition_id })),
        Some(&cookie),
    )
    .await;
    let session_id = start_body["id"].as_str().expect("session id").to_owned();

    let uri = format!("/api/net-sessions/{session_id}/close");
    let (status, _) = send_json(app.router(), "POST", &uri, None, Some(&cookie)).await;
    assert_eq!(status, StatusCode::OK);

    // A second close is rejected by the lifecycle guard, mapped to 409.
    let (status, body) = send_json(app.router(), "POST", &uri, None, Some(&cookie)).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(body["type"], "/errors/session-already-closed");
    assert_eq!(body["__contentType"], "application/problem+json");
    // Still exactly one session.closed event — the rejected close appended none.
    assert_eq!(event_count(&app, "session.closed").await, 1);
}

#[tokio::test]
async fn get_returns_the_same_summary_shape_for_reload() {
    let app = test_app().await;
    let cookie = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;
    let (definition_id, _version) = create_net(&app, &cookie).await;

    let (_status, start_body) = send_json(
        app.router(),
        "POST",
        "/api/net-sessions",
        Some(json!({ "definitionId": definition_id })),
        Some(&cookie),
    )
    .await;
    let session_id = start_body["id"].as_str().expect("session id").to_owned();

    let (status, close_body) = send_json(
        app.router(),
        "POST",
        &format!("/api/net-sessions/{session_id}/close"),
        None,
        Some(&cookie),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let (status, get_body) = send_json(
        app.router(),
        "GET",
        &format!("/api/net-sessions/{session_id}"),
        None,
        Some(&cookie),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    // The GET reload reproduces the close summary exactly (same folded source).
    for key in [
        "id",
        "lifecycle",
        "latestSeq",
        "connections",
        "startedAt",
        "closedAt",
        "durationSeconds",
    ] {
        assert_eq!(
            get_body[key], close_body[key],
            "GET matches close for {key}"
        );
    }
}

#[tokio::test]
async fn get_is_owner_only() {
    let app = test_app().await;
    let owner = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;
    let (definition_id, _version) = create_net(&app, &owner).await;
    let stranger = sign_in_consent_callsign(&app, "stranger@example.com", "n1ale").await;

    let (_status, start_body) = send_json(
        app.router(),
        "POST",
        "/api/net-sessions",
        Some(json!({ "definitionId": definition_id })),
        Some(&owner),
    )
    .await;
    let session_id = start_body["id"].as_str().expect("session id").to_owned();

    let (status, body) = send_json(
        app.router(),
        "GET",
        &format!("/api/net-sessions/{session_id}"),
        None,
        Some(&stranger),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(body["type"], "/errors/forbidden");
}

#[tokio::test]
async fn close_and_get_on_an_unknown_session_are_not_found() {
    let app = test_app().await;
    let cookie = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;
    let missing = uuid::Uuid::now_v7();

    let (status, body) = send_json(
        app.router(),
        "POST",
        &format!("/api/net-sessions/{missing}/close"),
        None,
        Some(&cookie),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body["type"], "/errors/net-session-not-found");

    let (status, body) = send_json(
        app.router(),
        "GET",
        &format!("/api/net-sessions/{missing}"),
        None,
        Some(&cookie),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body["type"], "/errors/net-session-not-found");
}

#[tokio::test]
async fn a_bad_operating_frequency_is_a_validation_error_and_no_event_is_written() {
    // START takes no frequency, so this pin sits on the surface that does: the
    // mid-session move.
    let app = test_app().await;
    let cookie = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;
    let session_id = fresh_live_session(&app, &cookie).await;
    let connection_id = first_connection_id(&app, &session_id).await;

    let (status, body) = send_json(
        app.router(),
        "POST",
        &format!("/api/net-sessions/{session_id}/frequency"),
        Some(json!({ "connectionId": connection_id, "operatingFrequency": "not-a-number" })),
        Some(&cookie),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["type"], "/errors/validation");
    assert_eq!(body["__contentType"], "application/problem+json");
    assert_eq!(event_count(&app, "frequency.changed").await, 0);
}

#[tokio::test]
async fn distinct_operating_frequency_faults_carry_distinct_detail() {
    // `parse_frequency_hz` already classifies five faults; the
    // handler discarded all five with `.map_err(|_| ..)` and answered by
    // reciting the rule. Asserting WHICH fault reached the wire — behaviour,
    // not wording.
    let app = test_app().await;
    let cookie = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;
    let session_id = fresh_live_session(&app, &cookie).await;
    let connection_id = first_connection_id(&app, &session_id).await;

    let mut details = Vec::new();
    for bad in ["not-a-number", "300000", "-7.200"] {
        let (status, body) = send_json(
            app.router(),
            "POST",
            &format!("/api/net-sessions/{session_id}/frequency"),
            Some(json!({ "connectionId": connection_id, "operatingFrequency": bad })),
            Some(&cookie),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{bad} must reject");
        assert_eq!(body["type"], "/errors/validation", "slug is unchanged");
        details.push(body["detail"].as_str().expect("detail").to_owned());
    }
    assert_ne!(
        details[0], details[1],
        "an unparseable value and an out-of-band value are different faults"
    );
    assert_ne!(details[1], details[2]);
    assert_ne!(details[0], details[2]);
    assert_eq!(
        event_count(&app, "frequency.changed").await,
        0,
        "no event written for any"
    );
}

#[tokio::test]
async fn close_clamps_closed_at_to_started_at_under_a_backward_clock_step() {
    // SystemClock is wall-clock and not guaranteed monotonic; an NTP step
    // can move it backward between the start and close requests. The handler
    // clamps closed_at = max(clock_now, started_at) so the folded duration is
    // never negative. Drive an injected clock that steps BACKWARD for close.
    const STARTED: u64 = 1_700_000_000_000;
    let mut app = test_app().await;
    let handle = Arc::new(AtomicU64::new(STARTED));
    app.state.clock = Arc::new(HandleClock {
        millis: handle.clone(),
    });

    let cookie = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;
    let (definition_id, _version) = create_net(&app, &cookie).await;

    let (_status, start_body) = send_json(
        app.router(),
        "POST",
        "/api/net-sessions",
        Some(json!({ "definitionId": definition_id })),
        Some(&cookie),
    )
    .await;
    let session_id = start_body["id"].as_str().expect("session id").to_owned();

    // Wall clock steps backward by 60s between the two requests.
    handle.store(STARTED - 60_000, Ordering::SeqCst);

    let (status, body) = send_json(
        app.router(),
        "POST",
        &format!("/api/net-sessions/{session_id}/close"),
        None,
        Some(&cookie),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        body["closedAt"], body["startedAt"],
        "the clamp pins closedAt to startedAt on a backward clock step"
    );
    assert_eq!(
        body["durationSeconds"], 0,
        "the clamped duration is exactly zero, never negative"
    );
}

// ---------------------------------------------------------------------------
// POST /api/net-sessions/{id}/frequency (owner-only mid-session
// operating-frequency change, ensure_mutable-gated, atomic guarded append).
// ---------------------------------------------------------------------------

/// Starts a fresh net + live session owned by `cookie`; returns the session id.
async fn fresh_live_session(app: &TestApp, cookie: &str) -> String {
    let (definition_id, _version) = create_net(app, cookie).await;
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

#[tokio::test]
async fn owner_changes_the_operating_frequency_mid_session() {
    // An owner POSTs a new decimal-MHz frequency for ONE of the session's
    // connections; the handler appends
    // frequency.changed (seq advances) and returns the folded summary with that
    // connection moved and no other.
    let app = test_app().await;
    let cookie = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;
    let session_id = fresh_live_session(&app, &cookie).await;
    let connection_id = first_connection_id(&app, &session_id).await;

    let (status, body) = send_json(
        app.router(),
        "POST",
        &format!("/api/net-sessions/{session_id}/frequency"),
        Some(json!({ "connectionId": connection_id, "operatingFrequency": "7.200" })),
        Some(&cookie),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["connections"][0]["id"], connection_id);
    assert_eq!(body["connections"][0]["plannedFrequencyHz"], 7_200_000);
    assert_eq!(body["latestSeq"], 2, "the append advanced the seq to 2");
    assert_eq!(
        body["lifecycle"], "live",
        "a frequency change is not a lifecycle transition"
    );
    assert_eq!(body["__contentType"], "application/json");

    // DB side-effects: exactly one frequency.changed at seq=2. The FROZEN
    // snapshot is not rewritten; the move lives in the log and the fold overlays
    // it, exactly as the retired projection column's value used to.
    assert_eq!(event_count(&app, "frequency.changed").await, 1);
    let (snapshot, last_seq): (Value, i64) =
        sqlx::query_as("SELECT definition_snapshot, last_seq FROM net_sessions WHERE id = $1")
            .bind(uuid::Uuid::parse_str(&session_id).unwrap())
            .fetch_one(&app.pool)
            .await
            .expect("session row");
    assert_eq!(
        snapshot["connections"][0]["plannedFrequencyHz"], 14_230_000,
        "a running session's stored snapshot is never rewritten"
    );
    assert_eq!(last_seq, 2);
}

#[tokio::test]
async fn a_non_owner_cannot_change_frequency_and_no_event_is_appended() {
    // A signed-in non-owner gets 403; the log gains no frequency.changed
    // and the folded frequency is unchanged. Reuses load_owned_session verbatim.
    let app = test_app().await;
    let owner = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;
    let session_id = fresh_live_session(&app, &owner).await;
    let connection_id = first_connection_id(&app, &session_id).await;
    let stranger = sign_in_consent_callsign(&app, "stranger@example.com", "n1ale").await;

    let (status, body) = send_json(
        app.router(),
        "POST",
        &format!("/api/net-sessions/{session_id}/frequency"),
        Some(json!({ "connectionId": connection_id, "operatingFrequency": "7.200" })),
        Some(&stranger),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(body["type"], "/errors/forbidden");
    assert_eq!(body["__contentType"], "application/problem+json");
    assert_eq!(
        event_count(&app, "frequency.changed").await,
        0,
        "a rejected change leaves the connection untouched"
    );
}

#[tokio::test]
async fn changing_frequency_unauthenticated_is_401_and_missing_is_404() {
    // An unauthenticated caller is 401; an absent session is 404 (decided
    // before ownership) — the shared load_owned_session posture.
    let app = test_app().await;
    let cookie = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;
    let session_id = fresh_live_session(&app, &cookie).await;

    let (status, _) = send_json(
        app.router(),
        "POST",
        &format!("/api/net-sessions/{session_id}/frequency"),
        Some(json!({
            "connectionId": "00000000-0000-0000-0000-000000000001",
            "operatingFrequency": "7.200"
        })),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    let missing = uuid::Uuid::now_v7();
    let (status, body) = send_json(
        app.router(),
        "POST",
        &format!("/api/net-sessions/{missing}/frequency"),
        Some(json!({
            "connectionId": "00000000-0000-0000-0000-000000000001",
            "operatingFrequency": "7.200"
        })),
        Some(&cookie),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body["type"], "/errors/net-session-not-found");
    assert_eq!(event_count(&app, "frequency.changed").await, 0);
}

#[tokio::test]
async fn changing_frequency_on_a_closed_session_is_a_409_already_closed() {
    // ensure_mutable(Closed) → 409 /errors/session-already-closed; no
    // event appended.
    let app = test_app().await;
    let cookie = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;
    let session_id = fresh_live_session(&app, &cookie).await;

    let (status, _) = send_json(
        app.router(),
        "POST",
        &format!("/api/net-sessions/{session_id}/close"),
        None,
        Some(&cookie),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let (status, body) = send_json(
        app.router(),
        "POST",
        &format!("/api/net-sessions/{session_id}/frequency"),
        Some(json!({
            "connectionId": "00000000-0000-0000-0000-000000000001",
            "operatingFrequency": "7.200"
        })),
        Some(&cookie),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(body["type"], "/errors/session-already-closed");
    assert_eq!(body["__contentType"], "application/problem+json");
    assert_eq!(event_count(&app, "frequency.changed").await, 0);
}

#[tokio::test]
async fn changing_frequency_on_a_not_yet_live_session_is_a_409_not_yet_live() {
    // ensure_mutable(Scheduled) → 409 /errors/session-not-yet-live. A
    // session cannot be created scheduled through the API (start writes 'live'),
    // so drive the barrier by forcing the row to 'scheduled' directly, proving
    // the ensure_mutable pre-check produces the precise slug.
    let app = test_app().await;
    let cookie = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;
    let session_id = fresh_live_session(&app, &cookie).await;
    sqlx::query("UPDATE net_sessions SET lifecycle = 'scheduled' WHERE id = $1")
        .bind(uuid::Uuid::parse_str(&session_id).unwrap())
        .execute(&app.pool)
        .await
        .expect("force scheduled");

    let (status, body) = send_json(
        app.router(),
        "POST",
        &format!("/api/net-sessions/{session_id}/frequency"),
        Some(json!({
            "connectionId": "00000000-0000-0000-0000-000000000001",
            "operatingFrequency": "7.200"
        })),
        Some(&cookie),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(body["type"], "/errors/session-not-yet-live");
    assert_eq!(event_count(&app, "frequency.changed").await, 0);
}

#[tokio::test]
async fn changing_frequency_with_a_malformed_value_is_a_400_and_no_event() {
    // The shipped parser rejects a non-numeric OR out-of-band value
    // with 400 /errors/validation; no frequency.changed appended.
    let app = test_app().await;
    let cookie = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;
    let session_id = fresh_live_session(&app, &cookie).await;

    let connection_id = first_connection_id(&app, &session_id).await;
    for bad in ["abc", "0.1"] {
        let (status, body) = send_json(
            app.router(),
            "POST",
            &format!("/api/net-sessions/{session_id}/frequency"),
            Some(json!({ "connectionId": connection_id, "operatingFrequency": bad })),
            Some(&cookie),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{bad} is a 400");
        assert_eq!(body["type"], "/errors/validation");
    }
    assert_eq!(event_count(&app, "frequency.changed").await, 0);
}

// ---------------------------------------------------------------------------
// POST /api/net-sessions/{id}/check-ins (owner-only staff check-in
// by callsign, ensure_mutable-gated, atomic guarded append, redacted public
// read). The LAST ensure_mutable consumer.
// ---------------------------------------------------------------------------

/// Counts roster entries for a session by folding its log (the on-wire roster
/// length is `participantCount`, but this asserts the DB-side truth directly).
async fn checkin_count(app: &TestApp, session_id: &str) -> i64 {
    sqlx::query_scalar(
        "SELECT count(*) FROM session_events WHERE session_id = $1 AND kind = 'checkin.added'",
    )
    .bind(uuid::Uuid::parse_str(session_id).unwrap())
    .fetch_one(&app.pool)
    .await
    .expect("count check-ins")
}

#[tokio::test]
async fn owner_adds_a_check_in_and_gets_a_folded_summary_with_the_new_roster_row() {
    // An owner POSTs a callsign to a live session; the handler appends
    // checkin.added (seq advances) and returns 201 + the folded summary with the
    // new roster row and advanced latestSeq.
    let app = test_app().await;
    let cookie = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;
    let session_id = fresh_live_session(&app, &cookie).await;

    let (status, body) = send_json(
        app.router(),
        "POST",
        &format!("/api/net-sessions/{session_id}/check-ins"),
        Some(json!({ "callsign": "N1CCK" })),
        Some(&cookie),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(body["latestSeq"], 2, "the append advanced the seq to 2");
    assert_eq!(body["lifecycle"], "live");
    assert_eq!(body["participantCount"], 1);
    let roster = body["roster"].as_array().expect("roster");
    assert_eq!(roster.len(), 1);
    assert_eq!(roster[0]["callsign"], "N1CCK");
    assert_eq!(body["__contentType"], "application/json");

    // DB side-effects: exactly one checkin.added at seq=2, session stays live.
    assert_eq!(checkin_count(&app, &session_id).await, 1);
    let (last_seq, lifecycle): (i64, String) =
        sqlx::query_as("SELECT last_seq, lifecycle FROM net_sessions WHERE id = $1")
            .bind(uuid::Uuid::parse_str(&session_id).unwrap())
            .fetch_one(&app.pool)
            .await
            .expect("session row");
    assert_eq!(last_seq, 2);
    assert_eq!(lifecycle, "live");
}

#[tokio::test]
async fn a_non_owner_cannot_log_an_arbitrary_station_as_staff() {
    // A signed-in non-owner once held NO capability at all, so this asserted a
    // flat 403. `Capability::SelfCheckIn` now sits at the Participant rank-0
    // floor — EVERY authenticated+consented account holds
    // it — so a non-owner "stranger" is no longer refused outright: the
    // widened endpoint routes them to the SELF path instead. The security
    // property that ACTUALLY matters is unchanged and re-asserted here: the
    // stranger can never log the ARBITRARY "K2XYZ" callsign (the staff
    // `LogCheckIn` power some other operator holds), only self-check-in with
    // their OWN forced callsign. Reuses load_owned_session verbatim.
    let app = test_app().await;
    let owner = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;
    let session_id = fresh_live_session(&app, &owner).await;
    let stranger = sign_in_consent_callsign(&app, "stranger@example.com", "n1ale").await;

    let (status, body) = send_json(
        app.router(),
        "POST",
        &format!("/api/net-sessions/{session_id}/check-ins"),
        Some(json!({ "callsign": "K2XYZ" })),
        Some(&stranger),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    let roster = body["roster"].as_array().expect("roster");
    assert!(
        !roster.iter().any(|e| e["callsign"] == "K2XYZ"),
        "the arbitrary staff-style callsign is never logged by a non-owner stranger"
    );
    let self_entry = roster
        .iter()
        .find(|e| e["source"] == "self")
        .expect("a self-check-in entry for the stranger");
    assert_eq!(self_entry["callsign"], "N1ALE");
    assert_eq!(checkin_count(&app, &session_id).await, 1);
}

#[tokio::test]
async fn adding_a_check_in_unauthenticated_is_401_and_missing_is_404() {
    // Unauthenticated → 401; absent session → 404 (decided before
    // ownership) — the shared load_owned_session posture.
    let app = test_app().await;
    let cookie = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;
    let session_id = fresh_live_session(&app, &cookie).await;

    let (status, _) = send_json(
        app.router(),
        "POST",
        &format!("/api/net-sessions/{session_id}/check-ins"),
        Some(json!({ "callsign": "N1CCK" })),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    let missing = uuid::Uuid::now_v7();
    let (status, body) = send_json(
        app.router(),
        "POST",
        &format!("/api/net-sessions/{missing}/check-ins"),
        Some(json!({ "callsign": "N1CCK" })),
        Some(&cookie),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body["type"], "/errors/net-session-not-found");
    assert_eq!(checkin_count(&app, &session_id).await, 0);
}

#[tokio::test]
async fn adding_a_check_in_on_a_closed_session_is_a_409_already_closed() {
    // ensure_mutable(Closed) → 409 /errors/session-already-closed; no
    // event appended (the LAST ensure_mutable consumer).
    let app = test_app().await;
    let cookie = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;
    let session_id = fresh_live_session(&app, &cookie).await;

    let (status, _) = send_json(
        app.router(),
        "POST",
        &format!("/api/net-sessions/{session_id}/close"),
        None,
        Some(&cookie),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let (status, body) = send_json(
        app.router(),
        "POST",
        &format!("/api/net-sessions/{session_id}/check-ins"),
        Some(json!({ "callsign": "N1CCK" })),
        Some(&cookie),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(body["type"], "/errors/session-already-closed");
    assert_eq!(body["__contentType"], "application/problem+json");
    assert_eq!(checkin_count(&app, &session_id).await, 0);
}

#[tokio::test]
async fn adding_a_check_in_on_a_not_yet_live_session_is_a_409_not_yet_live() {
    // ensure_mutable(Scheduled) → 409 /errors/session-not-yet-live. Force
    // the row to 'scheduled' directly (start writes 'live'), proving the
    // ensure_mutable pre-check produces the precise slug.
    let app = test_app().await;
    let cookie = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;
    let session_id = fresh_live_session(&app, &cookie).await;
    sqlx::query("UPDATE net_sessions SET lifecycle = 'scheduled' WHERE id = $1")
        .bind(uuid::Uuid::parse_str(&session_id).unwrap())
        .execute(&app.pool)
        .await
        .expect("force scheduled");

    let (status, body) = send_json(
        app.router(),
        "POST",
        &format!("/api/net-sessions/{session_id}/check-ins"),
        Some(json!({ "callsign": "N1CCK" })),
        Some(&cookie),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(body["type"], "/errors/session-not-yet-live");
    assert_eq!(checkin_count(&app, &session_id).await, 0);
}

// ---------------------------------------------------------------------------
// The per-check-in Maidenhead grid, threaded end to end.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_grid_survives_the_add_and_reads_back_canonicalized() {
    // The operator's casing is normalized by the SAME grammar
    // `profile::parse_grid` enforces, and `location` is an independent field
    // that keeps whatever free text was sent alongside it.
    let app = test_app().await;
    let cookie = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;
    let session_id = fresh_live_session(&app, &cookie).await;

    let (status, body) = send_json(
        app.router(),
        "POST",
        &format!("/api/net-sessions/{session_id}/check-ins"),
        Some(json!({ "callsign": "N1CCK", "location": "Hartford, CT", "grid": "fn31PR" })),
        Some(&cookie),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    let entry = &body["roster"].as_array().expect("roster")[0];
    assert_eq!(entry["grid"], "FN31pr", "canonicalized on the way in");
    assert_eq!(entry["location"], "Hartford, CT", "independent of the grid");

    // And it survives a re-read through the fold, not just the write response.
    let check_in_id = entry["checkInId"].as_str().expect("id").to_owned();
    let (status, summary) = send_json(
        app.router(),
        "GET",
        &format!("/api/net-sessions/{session_id}"),
        None,
        Some(&cookie),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let refolded = summary["roster"]
        .as_array()
        .expect("roster")
        .iter()
        .find(|e| e["checkInId"] == check_in_id.as_str())
        .expect("the row");
    assert_eq!(refolded["grid"], "FN31pr");
}

#[tokio::test]
async fn an_invalid_grid_is_refused_400_with_no_event_appended() {
    // The same grammar the profile path enforces, surfaced through the
    // ALREADY-EXISTING `/errors/grid-invalid` problem type. Assert the status
    // and the type slug — never the detail prose.
    let app = test_app().await;
    let cookie = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;
    let session_id = fresh_live_session(&app, &cookie).await;

    for bad in ["FN3", "SS11", "FN-31"] {
        let (status, body) = send_json(
            app.router(),
            "POST",
            &format!("/api/net-sessions/{session_id}/check-ins"),
            Some(json!({ "callsign": "N1CCK", "grid": bad })),
            Some(&cookie),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{bad:?} is a 400");
        assert_eq!(body["type"], "/errors/grid-invalid", "{bad:?}");
    }
    // No phantom append: the session is still at the seq the start left it.
    assert_eq!(checkin_count(&app, &session_id).await, 0);
    let last_seq: i64 = sqlx::query_scalar("SELECT last_seq FROM net_sessions WHERE id = $1")
        .bind(uuid::Uuid::parse_str(&session_id).unwrap())
        .fetch_one(&app.pool)
        .await
        .expect("session row");
    assert_eq!(last_seq, 1, "only the session.started event exists");

    // A blank grid CLEARS rather than 400s — `parse_grid("")` is Err(Empty), so
    // this is the arm that would break without the blank guard.
    let (status, body) = send_json(
        app.router(),
        "POST",
        &format!("/api/net-sessions/{session_id}/check-ins"),
        Some(json!({ "callsign": "N1CCK", "grid": "   " })),
        Some(&cookie),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    let entry = &body["roster"].as_array().expect("roster")[0];
    assert!(
        entry.as_object().expect("obj").get("grid").is_none(),
        "a blank grid is absent, not an error and not an empty string: {entry}"
    );
}

#[tokio::test]
async fn a_check_in_written_without_a_grid_key_reads_back_with_grid_absent() {
    // The `checkin.added` payload is inserted DIRECTLY through the pool with no
    // `grid` key, exactly as a row already sitting in `session_events` looks.
    // Nothing rewrites it.
    let app = test_app().await;
    let cookie = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;
    let session_id = fresh_live_session(&app, &cookie).await;
    let session_uuid = uuid::Uuid::parse_str(&session_id).unwrap();
    let check_in_id = uuid::Uuid::now_v7();

    let historical = json!({
        "checkInId": check_in_id.to_string(),
        "callsign": "N1CCK",
        "staying": "in-and-out",
        "location": "Hartford, CT",
    });
    assert!(
        historical.as_object().expect("obj").get("grid").is_none(),
        "the inserted payload must genuinely carry no grid key"
    );
    sqlx::query(
        "INSERT INTO session_events (id, session_id, seq, kind, payload, actor, created_at) \
         VALUES ($1, $2, 2, 'checkin.added', $3, NULL, now())",
    )
    .bind(uuid::Uuid::now_v7())
    .bind(session_uuid)
    .bind(&historical)
    .execute(&app.pool)
    .await
    .expect("insert an event with no grid key");
    sqlx::query("UPDATE net_sessions SET last_seq = 2 WHERE id = $1")
        .bind(session_uuid)
        .execute(&app.pool)
        .await
        .expect("advance last_seq");

    let (status, body) = send_json(
        app.router(),
        "GET",
        &format!("/api/net-sessions/{session_id}"),
        None,
        Some(&cookie),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "a historical payload with no grid key still decodes"
    );
    let entry = &body["roster"].as_array().expect("roster")[0];
    assert!(
        entry.as_object().expect("obj").get("grid").is_none(),
        "absent means ABSENT — not null, not \"\": {entry}"
    );
    assert_eq!(entry["location"], "Hartford, CT");

    // An edit that changes only `staying` must not fabricate a grid correction.
    let (status, body) = send_json(
        app.router(),
        "PUT",
        &format!("/api/net-sessions/{session_id}/check-ins/{check_in_id}"),
        Some(json!({
            "callsign": "N1CCK",
            "location": "Hartford, CT",
            "staying": "staying-for-comments",
            "expectedVersion": 1
        })),
        Some(&cookie),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let entry = &body["roster"].as_array().expect("roster")[0];
    assert!(
        entry.as_object().expect("obj").get("grid").is_none(),
        "still no grid after the edit: {entry}"
    );
    let fields: Vec<&str> = entry["corrections"]
        .as_array()
        .expect("corrections")
        .iter()
        .map(|c| c["field"].as_str().expect("field"))
        .collect();
    assert_eq!(
        fields,
        vec!["staying"],
        "a historical entry grows NO grid correction"
    );
}

#[tokio::test]
async fn adding_a_check_in_with_a_malformed_callsign_is_a_400_and_no_event() {
    // The shipped parse_callsign rejects a malformed value with 400
    // /errors/callsign-invalid; no checkin.added appended.
    let app = test_app().await;
    let cookie = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;
    let session_id = fresh_live_session(&app, &cookie).await;

    for bad in ["", "not a callsign", "123"] {
        let (status, body) = send_json(
            app.router(),
            "POST",
            &format!("/api/net-sessions/{session_id}/check-ins"),
            Some(json!({ "callsign": bad })),
            Some(&cookie),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{bad:?} is a 400");
        assert_eq!(body["type"], "/errors/callsign-invalid");
    }
    assert_eq!(checkin_count(&app, &session_id).await, 0);
}

// ---------------------------------------------------------------------------
// The clientEventId seam turned ON end-to-end + idempotency on
// (net_session_id, clientEventId). The optimistic quick-add's server backstop.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_check_in_with_a_client_event_id_round_trips_it_into_the_emitted_event() {
    // The POST body accepts clientEventId; the handler threads it into
    // the checkin.added payload. The owner catch-up wire carries it; the public
    // (account-less) wire strips it. Asserted through the real serializers
    // via the two catch-up endpoints, not just the DB column.
    let app = test_app().await;
    let cookie = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;
    let session_id = fresh_live_session(&app, &cookie).await;
    let client_event_id = uuid::Uuid::now_v7().to_string();

    let (status, _body) = send_json(
        app.router(),
        "POST",
        &format!("/api/net-sessions/{session_id}/check-ins"),
        Some(json!({ "callsign": "N1CCK", "clientEventId": client_event_id })),
        Some(&cookie),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);

    // Owner catch-up: the checkin.added delta echoes the clientEventId.
    let (status, events) = send_json(
        app.router(),
        "GET",
        &format!("/api/net-sessions/{session_id}/events?since=1"),
        None,
        Some(&cookie),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let events = events.as_array().expect("events array");
    let checkin = events
        .iter()
        .find(|e| e["kind"] == "checkin.added")
        .expect("a checkin.added delta");
    assert_eq!(
        checkin["payload"]["clientEventId"], client_event_id,
        "the owner wire echoes the clientEventId the operator sent"
    );

    // Public catch-up: the redacted wire strips the operator's optimistic id.
    let (status, public_events) = send_json(
        app.router(),
        "GET",
        &format!("/api/net-sessions/{session_id}/live/events?since=1"),
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let public_events = public_events.as_array().expect("public events array");
    let public_checkin = public_events
        .iter()
        .find(|e| e["kind"] == "checkin.added")
        .expect("a public checkin.added delta");
    assert!(
        public_checkin["payload"].get("clientEventId").is_none(),
        "the public wire never leaks another client's optimistic echo id"
    );
}

#[tokio::test]
async fn two_adds_with_the_same_client_event_id_yield_one_roster_row_and_an_idempotent_200() {
    // A rapid double-submit (same clientEventId, e.g. a double-click
    // or a retry-after-slow-response) must NOT append two events. The first is a
    // 201; the second is an idempotent 200 returning the existing check-in, with
    // exactly one roster row and no second checkin.added in the log.
    let app = test_app().await;
    let cookie = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;
    let session_id = fresh_live_session(&app, &cookie).await;
    let client_event_id = uuid::Uuid::now_v7().to_string();

    let (status, first) = send_json(
        app.router(),
        "POST",
        &format!("/api/net-sessions/{session_id}/check-ins"),
        Some(json!({ "callsign": "N1CCK", "clientEventId": client_event_id })),
        Some(&cookie),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "the first add creates the row");
    assert_eq!(first["participantCount"], 1);
    assert_eq!(first["latestSeq"], 2);

    let (status, second) = send_json(
        app.router(),
        "POST",
        &format!("/api/net-sessions/{session_id}/check-ins"),
        Some(json!({ "callsign": "N1CCK", "clientEventId": client_event_id })),
        Some(&cookie),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "a duplicate clientEventId is an idempotent success, not a 201 or an error"
    );
    assert_eq!(
        second["participantCount"], 1,
        "the duplicate add did not grow the roster"
    );
    assert_eq!(
        second["latestSeq"], 2,
        "the reverted duplicate append left the sequence counter untouched (gapless)"
    );
    let roster = second["roster"].as_array().expect("roster");
    assert_eq!(roster.len(), 1);
    assert_eq!(roster[0]["callsign"], "N1CCK");

    // The log holds exactly one checkin.added — the second write was folded away,
    // never appended as a phantom second event with a fresh check_in_id.
    assert_eq!(checkin_count(&app, &session_id).await, 1);
}

#[tokio::test]
async fn adds_with_no_client_event_id_are_unchanged_and_never_deduped() {
    // A NULL/omitted clientEventId is exempt from the partial idempotency
    // index, so two id-less adds of the same callsign produce two rows.
    let app = test_app().await;
    let cookie = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;
    let session_id = fresh_live_session(&app, &cookie).await;

    for _ in 0..2 {
        let (status, _) = send_json(
            app.router(),
            "POST",
            &format!("/api/net-sessions/{session_id}/check-ins"),
            Some(json!({ "callsign": "N1CCK" })),
            Some(&cookie),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED);
    }
    assert_eq!(
        checkin_count(&app, &session_id).await,
        2,
        "id-less adds are never idempotency-deduped (the partial index exempts NULL)"
    );
}

// ---------------------------------------------------------------------------
// Mode-shaped signal report (staff-only, EditStaffFields) + staying
// status, both additive on checkin.added. Report/staying on the owner wire only.
// ---------------------------------------------------------------------------

/// Grants `role` to `callsign` on `session_id` as the acting `cookie` — the
/// role surface, reused to set up the Relay/Logger field-gate cases.
async fn grant_role(
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

#[tokio::test]
async fn a_staff_check_in_carries_signal_report_and_staying_on_the_owner_wire() {
    // An owner (holds EditStaffFields) POSTs a report + staying;
    // the folded owner summary roster row carries BOTH verbatim.
    let app = test_app().await;
    let cookie = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;
    let session_id = fresh_live_session(&app, &cookie).await;

    let (status, body) = send_json(
        app.router(),
        "POST",
        &format!("/api/net-sessions/{session_id}/check-ins"),
        Some(json!({ "callsign": "N1CCK", "signalReport": "599", "staying": "staying-for-comments" })),
        Some(&cookie),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    let roster = body["roster"].as_array().expect("roster");
    assert_eq!(roster.len(), 1);
    assert_eq!(roster[0]["signalReport"], "599");
    assert_eq!(roster[0]["staying"], "staying-for-comments");
    assert_eq!(checkin_count(&app, &session_id).await, 1);
}

#[tokio::test]
async fn a_relay_setting_a_report_is_403_while_a_callsign_only_relay_add_is_201() {
    // A Relay holds LogCheckIn (may log a callsign-only check-in) but NOT
    // EditStaffFields — a Relay sending a report is 403 with NO append; the
    // callsign-only add still succeeds (the LogCheckIn gate is unchanged).
    let app = test_app().await;
    let owner = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;
    let session_id = fresh_live_session(&app, &owner).await;
    let relay = sign_in_consent_callsign(&app, "relay@example.com", "n1ale").await;
    assert_eq!(
        grant_role(&app, &owner, &session_id, "n1ale", "relay").await,
        StatusCode::OK
    );

    // Relay + report → 403, no append.
    let (status, body) = send_json(
        app.router(),
        "POST",
        &format!("/api/net-sessions/{session_id}/check-ins"),
        Some(json!({ "callsign": "K2XYZ", "signalReport": "59" })),
        Some(&relay),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(body["type"], "/errors/forbidden");
    assert_eq!(checkin_count(&app, &session_id).await, 0);

    // Relay callsign-only → 201 (LogCheckIn unchanged).
    let (status, _body) = send_json(
        app.router(),
        "POST",
        &format!("/api/net-sessions/{session_id}/check-ins"),
        Some(json!({ "callsign": "K2XYZ" })),
        Some(&relay),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(checkin_count(&app, &session_id).await, 1);
}

#[tokio::test]
async fn a_relay_sending_an_explicit_blank_report_is_still_403_no_append() {
    // The EditStaffFields gate is keyed on the PRESENCE of `signalReport` in
    // the request JSON, not on whether it resolves to a non-blank value after
    // trimming. An explicit blank string is a
    // distinct wire shape from an OMITTED key, and a Relay including the key
    // at all — even blank — is refused, exactly like a non-blank report. This
    // is the intended presence-gated design (documented in the handler), not
    // a bug; this test closes the coverage gap two review layers flagged.
    let app = test_app().await;
    let owner = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;
    let session_id = fresh_live_session(&app, &owner).await;
    let relay = sign_in_consent_callsign(&app, "relay@example.com", "n1ale").await;
    assert_eq!(
        grant_role(&app, &owner, &session_id, "n1ale", "relay").await,
        StatusCode::OK
    );

    let (status, body) = send_json(
        app.router(),
        "POST",
        &format!("/api/net-sessions/{session_id}/check-ins"),
        Some(json!({ "callsign": "K2XYZ", "signalReport": "" })),
        Some(&relay),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(body["type"], "/errors/forbidden");
    assert_eq!(
        checkin_count(&app, &session_id).await,
        0,
        "an explicit blank report from a Relay must append nothing, same as a non-blank one"
    );
}

#[tokio::test]
async fn a_logger_may_set_a_signal_report() {
    // A Logger holds EditStaffFields (the Logger threshold), so a Logger
    // POSTing a report succeeds.
    let app = test_app().await;
    let owner = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;
    let session_id = fresh_live_session(&app, &owner).await;
    let logger = sign_in_consent_callsign(&app, "logger@example.com", "n1ale").await;
    assert_eq!(
        grant_role(&app, &owner, &session_id, "n1ale", "logger").await,
        StatusCode::OK
    );

    let (status, body) = send_json(
        app.router(),
        "POST",
        &format!("/api/net-sessions/{session_id}/check-ins"),
        Some(json!({ "callsign": "K2XYZ", "signalReport": "-06" })),
        Some(&logger),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(body["roster"][0]["signalReport"], "-06");
    assert_eq!(checkin_count(&app, &session_id).await, 1);
}

#[tokio::test]
async fn an_omitted_staying_defaults_to_in_and_out() {
    // A check-in with no `staying` field folds to in-and-out.
    let app = test_app().await;
    let cookie = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;
    let session_id = fresh_live_session(&app, &cookie).await;

    let (status, body) = send_json(
        app.router(),
        "POST",
        &format!("/api/net-sessions/{session_id}/check-ins"),
        Some(json!({ "callsign": "N1CCK" })),
        Some(&cookie),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(body["roster"][0]["staying"], "in-and-out");
    // A callsign-only add carries no signalReport key on the wire.
    assert!(
        body["roster"][0].get("signalReport").is_none(),
        "signalReport is omitted when absent"
    );
}

#[tokio::test]
async fn an_unknown_staying_token_is_a_400_and_no_event() {
    // An out-of-vocabulary staying token is a 400 with no append.
    let app = test_app().await;
    let cookie = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;
    let session_id = fresh_live_session(&app, &cookie).await;

    let (status, body) = send_json(
        app.router(),
        "POST",
        &format!("/api/net-sessions/{session_id}/check-ins"),
        Some(json!({ "callsign": "N1CCK", "staying": "maybe" })),
        Some(&cookie),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["type"], "/errors/staying-invalid");
    assert_eq!(checkin_count(&app, &session_id).await, 0);
}

#[tokio::test]
async fn the_public_view_roster_carries_staying_and_precedence_but_still_no_report() {
    // The signal report AND the staying status were once both behind the staff
    // console in one sentence; the 2026-08-27 ruling
    // moved `staying`, `precedence`, `traffic` and the PUBLIC note onto the
    // account-less wire and left every other redacted field exactly where it
    // was. So the sentence SPLITS, and both halves are asserted here in one
    // test: the report is still absent, and staying/precedence/traffic are now
    // present carrying the value the operator actually set.
    //
    // Seeded with NON-DEFAULT values deliberately. The frontend
    // reducer folds an absent `staying` to `in-and-out` and an absent
    // `precedence` to `routine`, so a fixture seeded with the DEFAULTS could not
    // discriminate a redacted wire from a widened one — it would pass against
    // both.
    let app = test_app().await;
    let cookie = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;
    let session_id = fresh_live_session(&app, &cookie).await;

    let (status, body) = send_json(
        app.router(),
        "POST",
        &format!("/api/net-sessions/{session_id}/check-ins"),
        Some(json!({ "callsign": "N1CCK", "signalReport": "599", "staying": "staying-for-comments" })),
        Some(&cookie),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    let check_in_id = body["roster"].as_array().expect("roster")[0]["checkInId"]
        .as_str()
        .expect("id")
        .to_owned();

    // Precedence/traffic/notes are edit-only, so the non-default seed rides an
    // edit. Both notes are set, with distinguishable strings.
    let (status, owner_body) = send_json(
        app.router(),
        "PUT",
        &format!("/api/net-sessions/{session_id}/check-ins/{check_in_id}"),
        Some(json!({
            "callsign": "N1CCK",
            "precedence": "emergency",
            "traffic": 3,
            "notes": "STAFF-ONLY round commentary",
            "publicNote": "handling one piece of health-and-welfare traffic",
            "expectedVersion": 1
        })),
        Some(&cookie),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let owner_entry = &owner_body["roster"].as_array().expect("roster")[0];
    assert_eq!(
        owner_entry["precedence"], "emergency",
        "the fixture genuinely holds emergency traffic on the OWNER wire"
    );
    assert_eq!(
        owner_entry["traffic"], 3,
        "the fixture genuinely has a count"
    );

    let (status, raw) = get_public_raw(&app, &session_id).await;
    assert_eq!(status, StatusCode::OK);
    let public: Value = serde_json::from_str(&raw).expect("json");
    let roster = public["roster"].as_array().expect("public roster");
    assert_eq!(roster.len(), 1);
    assert_eq!(roster[0]["callsign"], "N1CCK");

    // NOW TRUE: the observer sees what the operator set.
    assert_eq!(
        roster[0]["staying"], "staying-for-comments",
        "the public roster carries the staying the operator set, not a default"
    );
    assert_eq!(
        roster[0]["precedence"], "emergency",
        "the public roster carries the precedence the operator set, not `routine`"
    );
    assert_eq!(
        roster[0]["traffic"], 3,
        "the public roster carries the declared traffic count"
    );
    assert_eq!(
        roster[0]["publicNote"], "handling one piece of health-and-welfare traffic",
        "the PUBLIC note reaches the observer"
    );

    // STILL TRUE, and re-asserted in the same test so no assertion is merely
    // deleted: the report and the STAFF note stay redacted.
    assert!(
        roster[0].get("signalReport").is_none(),
        "public roster still carries no signalReport"
    );
    assert!(
        roster[0].get("notes").is_none(),
        "public roster still carries no STAFF note"
    );
    assert!(
        !raw.contains("STAFF-ONLY round commentary"),
        "the staff note never leaks in any shape: {raw}"
    );
}

/// The account-less `GET /…/live` as RAW BYTES — the byte form is what proves a
/// redacted string is absent in EVERY shape, not merely under the key it would
/// have had.
async fn get_public_raw(app: &TestApp, session_id: &str) -> (StatusCode, String) {
    let request = Request::builder()
        .method("GET")
        .uri(format!("/api/net-sessions/{session_id}/live"))
        .body(Body::empty())
        .expect("build request");
    let response = app.router().oneshot(request).await.expect("route");
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024)
        .await
        .expect("body");
    (status, String::from_utf8(bytes.to_vec()).expect("utf8"))
}

// --- Presence, stall, handoff, claim, auto-close ----------------

/// The account id (as a `Uuid`) reserved for a callsign — the handoff target id.
async fn account_id_by_callsign(app: &TestApp, callsign: &str) -> uuid::Uuid {
    // Callsigns are stored ASCII-uppercased (parse_callsign normalizes on write).
    sqlx::query_scalar("SELECT id FROM accounts WHERE callsign = upper($1)")
        .bind(callsign)
        .fetch_one(&app.pool)
        .await
        .expect("account id by callsign")
}

/// The session id as a `Uuid` (for seeding presence / driving the monitor tick).
fn session_uuid(session_id: &str) -> uuid::Uuid {
    session_id.parse().expect("session id is a uuid")
}

/// Drives exactly ONE monitor tick at `now_millis` with the standard thresholds
/// (no sleeps) — the injected-clock single-tick test seam.
async fn tick_monitor(app: &TestApp, now_millis: u64) {
    run_presence_monitor_tick(
        &app.state.net_sessions,
        &app.state.hub,
        &app.state.session_presence,
        now_millis,
        90_000,
        900_000,
        &app.state.delivery_service(),
    )
    .await
    .expect("monitor tick");
}

#[tokio::test]
async fn active_ncs_voluntarily_hands_off_to_a_qualified_target_keeping_the_stream_active() {
    // The active NCS hands off to a NetControl-tier target; control moves,
    // control_state stays active (stream uninterrupted), and a control.handed-off
    // event is appended.
    let app = test_app().await;
    let owner = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;
    let session_id = fresh_live_session(&app, &owner).await;
    let target_cookie = sign_in_consent_callsign(&app, "nc@example.com", "n1cck").await;
    assert_eq!(
        grant_role(&app, &owner, &session_id, "n1cck", "net-control").await,
        StatusCode::OK
    );
    let target_id = account_id_by_callsign(&app, "n1cck").await;
    let _ = target_cookie;

    let (status, body) = send_json(
        app.router(),
        "POST",
        &format!("/api/net-sessions/{session_id}/handoff"),
        Some(json!({ "targetAccountId": target_id })),
        Some(&owner),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        body["controlState"], "active",
        "handoff keeps the net active"
    );
    assert_eq!(
        body["activeNcsAccountId"],
        target_id.to_string(),
        "the target is the new active NCS"
    );
    assert_eq!(event_count(&app, "control.handed-off").await, 1);
}

#[tokio::test]
async fn a_handoff_to_self_is_an_inert_no_op_with_no_event() {
    // The active NCS "handing off" to
    // themselves would mint a spurious control.handed-off event with no actual
    // authority change. It short-circuits to a 200 with the unchanged summary
    // and appends nothing.
    let app = test_app().await;
    let owner = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;
    let session_id = fresh_live_session(&app, &owner).await;
    let owner_id = account_id_by_callsign(&app, "w1aw").await;

    let (status, body) = send_json(
        app.router(),
        "POST",
        &format!("/api/net-sessions/{session_id}/handoff"),
        Some(json!({ "targetAccountId": owner_id })),
        Some(&owner),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["activeNcsAccountId"], owner_id.to_string());
    assert_eq!(event_count(&app, "control.handed-off").await, 0);
}

#[tokio::test]
async fn a_handoff_to_an_unqualified_target_is_422_with_no_event() {
    // A Relay-tier target cannot receive control.
    let app = test_app().await;
    let owner = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;
    let session_id = fresh_live_session(&app, &owner).await;
    let _relay = sign_in_consent_callsign(&app, "relay@example.com", "n1ale").await;
    assert_eq!(
        grant_role(&app, &owner, &session_id, "n1ale", "relay").await,
        StatusCode::OK
    );
    let target_id = account_id_by_callsign(&app, "n1ale").await;

    let (status, _body) = send_json(
        app.router(),
        "POST",
        &format!("/api/net-sessions/{session_id}/handoff"),
        Some(json!({ "targetAccountId": target_id })),
        Some(&owner),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(event_count(&app, "control.handed-off").await, 0);
}

#[tokio::test]
async fn a_handoff_by_a_non_active_ncs_is_403() {
    // A co-owner who is NOT the current active NCS cannot hand off.
    let app = test_app().await;
    let owner = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;
    let session_id = fresh_live_session(&app, &owner).await;
    // A second NetControl who is not the active NCS.
    let other = sign_in_consent_callsign(&app, "other@example.com", "n1cck").await;
    assert_eq!(
        grant_role(&app, &owner, &session_id, "n1cck", "net-control").await,
        StatusCode::OK
    );
    let target_id = account_id_by_callsign(&app, "w1aw").await;

    let (status, _body) = send_json(
        app.router(),
        "POST",
        &format!("/api/net-sessions/{session_id}/handoff"),
        Some(json!({ "targetAccountId": target_id })),
        Some(&other),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn claiming_control_on_a_non_stalled_session_is_409_control_not_stalled() {
    // A claim only applies to a stalled session.
    let app = test_app().await;
    let owner = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;
    let session_id = fresh_live_session(&app, &owner).await;
    let logger = sign_in_consent_callsign(&app, "logger@example.com", "n1cck").await;
    assert_eq!(
        grant_role(&app, &owner, &session_id, "n1cck", "logger").await,
        StatusCode::OK
    );

    let (status, body) = send_json(
        app.router(),
        "POST",
        &format!("/api/net-sessions/{session_id}/claim-control"),
        None,
        Some(&logger),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(body["type"], "/errors/control-not-stalled");
}

#[tokio::test]
async fn a_relay_cannot_claim_control_403() {
    // Relay/Participant lack ClaimControl.
    let app = test_app().await;
    let owner = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;
    let session_id = fresh_live_session(&app, &owner).await;
    let relay = sign_in_consent_callsign(&app, "relay@example.com", "n1ale").await;
    assert_eq!(
        grant_role(&app, &owner, &session_id, "n1ale", "relay").await,
        StatusCode::OK
    );
    // Stall the session first so the ONLY refusal reason is the capability.
    tick_monitor(&app, 1_000_000).await;
    assert_eq!(event_count(&app, "ncs.stalled").await, 1);

    let (status, _body) = send_json(
        app.router(),
        "POST",
        &format!("/api/net-sessions/{session_id}/claim-control"),
        None,
        Some(&relay),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn a_logger_claims_a_stalled_session_and_the_net_returns_to_active_under_them() {
    // The Logger-floor rescue path. Stall the net (monitor), then a logger
    // claims — control.handed-off appended, controlState active, logger is NCS.
    let app = test_app().await;
    let owner = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;
    let session_id = fresh_live_session(&app, &owner).await;
    let logger = sign_in_consent_callsign(&app, "logger@example.com", "n1cck").await;
    assert_eq!(
        grant_role(&app, &owner, &session_id, "n1cck", "logger").await,
        StatusCode::OK
    );
    let logger_id = account_id_by_callsign(&app, "n1cck").await;

    // The owner (active NCS) never beat presence → the first tick stalls the net.
    tick_monitor(&app, 1_000_000).await;
    assert_eq!(event_count(&app, "ncs.stalled").await, 1);

    let (status, body) = send_json(
        app.router(),
        "POST",
        &format!("/api/net-sessions/{session_id}/claim-control"),
        None,
        Some(&logger),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["controlState"], "active");
    assert_eq!(body["activeNcsAccountId"], logger_id.to_string());
    assert_eq!(event_count(&app, "control.handed-off").await, 1);
}

#[tokio::test]
async fn a_check_in_on_a_stalled_session_is_409_session_paused_with_no_event() {
    // The roster is frozen while stalled — a check-in POST is refused with
    // session-paused and appends nothing.
    let app = test_app().await;
    let owner = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;
    let session_id = fresh_live_session(&app, &owner).await;
    tick_monitor(&app, 1_000_000).await;
    assert_eq!(event_count(&app, "ncs.stalled").await, 1);
    let checkins_before = event_count(&app, "checkin.added").await;

    let (status, body) = send_json(
        app.router(),
        "POST",
        &format!("/api/net-sessions/{session_id}/check-ins"),
        Some(json!({ "callsign": "N1CCK" })),
        Some(&owner),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(body["type"], "/errors/session-paused");
    assert_eq!(event_count(&app, "checkin.added").await, checkins_before);
}

#[tokio::test]
async fn the_public_view_carries_control_state_but_never_the_active_ncs_account_id() {
    // controlState is public radio data; activeNcsAccountId is redacted.
    let app = test_app().await;
    let owner = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;
    let session_id = fresh_live_session(&app, &owner).await;

    let (status, public) = send_json(
        app.router(),
        "GET",
        &format!("/api/net-sessions/{session_id}/live"),
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(public["controlState"], "active", "controlState is public");
    assert!(
        public.get("activeNcsAccountId").is_none(),
        "the operator id is redacted from the public view"
    );
    // Raw-bytes belt-and-braces: the serialized public body never names the field.
    assert!(!public.to_string().contains("activeNcsAccountId"));

    // The OWNER summary carries BOTH.
    let (status, owner_summary) = send_json(
        app.router(),
        "GET",
        &format!("/api/net-sessions/{session_id}"),
        None,
        Some(&owner),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(owner_summary["controlState"], "active");
    assert!(
        owner_summary["activeNcsAccountId"].is_string(),
        "the owner summary carries the active NCS id"
    );
}

#[tokio::test]
async fn the_monitor_stalls_then_auto_closes_an_abandoned_net_idempotently() {
    // With no active-NCS presence, one tick stalls the net; a tick past
    // the 15-min window auto-closes it (exactly one session.closed); a further
    // tick is inert.
    let app = test_app().await;
    let owner = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;
    let session_id = fresh_live_session(&app, &owner).await;
    let sid = session_uuid(&session_id);

    // Tick 1 at t=1_000_000: the owner never beat presence → stall.
    tick_monitor(&app, 1_000_000).await;
    assert_eq!(event_count(&app, "ncs.stalled").await, 1);
    let (control_state,): (String,) =
        sqlx::query_as("SELECT control_state FROM net_sessions WHERE id = $1")
            .bind(sid)
            .fetch_one(&app.pool)
            .await
            .expect("row");
    assert_eq!(control_state, "stalled");

    // A tick still inside the 15-min window does NOT close.
    tick_monitor(&app, 1_000_000 + 899_000).await;
    assert_eq!(event_count(&app, "session.closed").await, 0);

    // A tick past stalled_at + 900_000 auto-closes exactly once.
    tick_monitor(&app, 1_000_000 + 900_000).await;
    assert_eq!(event_count(&app, "session.closed").await, 1);
    // A further tick is inert (idempotent — the session is closed/not-live).
    tick_monitor(&app, 1_000_000 + 2_000_000).await;
    assert_eq!(event_count(&app, "session.closed").await, 1);
    assert_eq!(event_count(&app, "ncs.stalled").await, 1);
}

#[tokio::test]
async fn a_returned_active_ncs_presence_resumes_a_stalled_net() {
    // The presence-driven resume — the active NCS's fresh heartbeat before
    // auto-close mints exactly one ncs.resumed and returns the net to active.
    let app = test_app().await;
    let owner = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;
    let session_id = fresh_live_session(&app, &owner).await;
    let sid = session_uuid(&session_id);
    let owner_id = account_id_by_callsign(&app, "w1aw").await;

    // Stall first (no presence).
    tick_monitor(&app, 1_000_000).await;
    assert_eq!(event_count(&app, "ncs.stalled").await, 1);

    // The active NCS reconnects: a fresh heartbeat lands in the registry.
    app.state
        .session_presence
        .heartbeat(sid, owner_id, 1_050_000);
    // The next tick (within the stall threshold of the fresh beat) resumes.
    tick_monitor(&app, 1_050_000).await;
    assert_eq!(event_count(&app, "ncs.resumed").await, 1);
    let (control_state,): (String,) =
        sqlx::query_as("SELECT control_state FROM net_sessions WHERE id = $1")
            .bind(sid)
            .fetch_one(&app.pool)
            .await
            .expect("row");
    assert_eq!(control_state, "active");
}

#[tokio::test]
async fn the_stall_threshold_is_a_strict_boundary_not_stale_at_exactly_90s_stale_past_it() {
    // Every other stall test drives an NCS
    // that never beats presence at all (`last_seen == None`), which reports
    // stale unconditionally regardless of what STALL_THRESHOLD_MILLIS is set
    // to. This test seeds a REAL heartbeat and ticks exactly at the threshold
    // (89_999_ms and 90_000ms), then past it (90_001ms), proving
    // `active_ncs_presence_is_stale`'s strict `>` comparison actually gates
    // the transition at the documented 90s boundary.
    let app = test_app().await;
    let owner = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;
    let session_id = fresh_live_session(&app, &owner).await;
    let sid = session_uuid(&session_id);
    let owner_id = account_id_by_callsign(&app, "w1aw").await;

    // The starter's last heartbeat lands at t=0.
    app.state.session_presence.heartbeat(sid, owner_id, 0);

    // A tick at exactly the threshold (delta == 90_000) is NOT stale (strict >).
    tick_monitor(&app, 90_000).await;
    assert_eq!(event_count(&app, "ncs.stalled").await, 0);

    // A tick one millisecond past the threshold (delta == 90_001) IS stale.
    tick_monitor(&app, 90_001).await;
    assert_eq!(event_count(&app, "ncs.stalled").await, 1);
    let (control_state,): (String,) =
        sqlx::query_as("SELECT control_state FROM net_sessions WHERE id = $1")
            .bind(sid)
            .fetch_one(&app.pool)
            .await
            .expect("row");
    assert_eq!(control_state, "stalled");
}

#[tokio::test]
async fn a_rejected_signal_report_on_a_staff_check_in_names_the_field() {
    // The staff check-in path once rendered `ProfileError`'s `Display` straight
    // into `detail`, so an over-bound report answered a subjectless fragment
    // naming no field. The assertion is the field POINTER, not the sentence.
    let app = test_app().await;
    let cookie = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;
    let session_id = fresh_live_session(&app, &cookie).await;

    let (status, body) = send_json(
        app.router(),
        "POST",
        &format!("/api/net-sessions/{session_id}/check-ins"),
        Some(json!({ "callsign": "N1CCK", "signalReport": "x".repeat(17) })),
        Some(&cookie),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(
        body["type"], "/errors/signal-report-invalid",
        "slug unchanged"
    );
    let detail = body["detail"].as_str().expect("detail");
    assert!(
        detail.contains("signal report"),
        "the rejection must name the field it is about: {detail}"
    );
    assert_eq!(checkin_count(&app, &session_id).await, 0, "no row written");
}

// --- A historical snapshot keeps decoding -------------------

#[tokio::test]
async fn a_stored_snapshot_carrying_the_retired_flat_keys_still_decodes_and_renders() {
    // A REGRESSION PIN, not a red. Six residual flat fields are gone from
    // `DefinitionSnapshot` (`repeaterOffsetHz`, `toneMode`, `toneValue`,
    // `echolinkNode`, `reflector`, `allstarNode`). Every session snapshotted
    // before it carries some of those keys in its jsonb, and every one of them
    // must go on decoding — serde ignores an unknown key unless the struct says
    // `deny_unknown_fields`, and this struct must never say it. The
    // `410 /errors/unreplayable-log` exists for a snapshot with NO
    // `connections` key; a snapshot with EXTRA keys is not that, and turning it
    // into that would kill every historical session a second time.
    let app = test_app().await;
    let cookie = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;
    let (definition_id, _version) = create_net(&app, &cookie).await;

    let (status, started) = send_json(
        app.router(),
        "POST",
        "/api/net-sessions",
        Some(json!({ "definitionId": definition_id })),
        Some(&cookie),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{started}");
    let session_id = started["id"].as_str().expect("session id").to_owned();

    // The shape an older writer produced: the connection set PLUS the six
    // definition-copied flat keys, all populated.
    sqlx::query(
        "UPDATE net_sessions SET definition_snapshot = definition_snapshot || $2::jsonb
          WHERE id = $1",
    )
    .bind(uuid::Uuid::parse_str(&session_id).expect("uuid"))
    .bind(json!({
        "repeaterOffsetHz": -600_000,
        "toneMode": "ctcss",
        "toneValue": "100.0",
        "echolinkNode": "12345",
        "reflector": "REF030 C",
        "allstarNode": "54321",
    }))
    .execute(&app.pool)
    .await
    .expect("plant the retired keys on the stored snapshot");

    let (status, body) = send_json(
        app.router(),
        "GET",
        &format!("/api/net-sessions/{session_id}"),
        None,
        Some(&cookie),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "a snapshot with keys this build no longer reads is still a readable snapshot: {body}"
    );
    assert_eq!(body["definition"]["title"], "Sunday Traffic Net");
    assert!(
        !body["definition"]["connections"]
            .as_array()
            .expect("the rendered definition carries its connections")
            .is_empty(),
        "the session renders from the connection set the snapshot carries"
    );
}
