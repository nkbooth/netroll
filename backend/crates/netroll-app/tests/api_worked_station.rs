// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
//! Full-loop API tests for the worked-station cursor and the two note scopes.
//! Real router, real Postgres (testcontainers), capturing fake
//! mailer. Asserts status codes, problem+json `type` slugs, and folded
//! cursor/note state — never message prose (house TDD rule).

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

fn entry<'a>(summary: &'a Value, check_in_id: &str) -> &'a Value {
    summary["roster"]
        .as_array()
        .expect("roster")
        .iter()
        .find(|e| e["checkInId"] == check_in_id)
        .expect("the entry")
}

// --- worked-station ---------------------------------------------------------

#[tokio::test]
async fn the_ncs_sets_the_worked_station_and_a_move_marks_the_prior_worked() {
    let app = test_app().await;
    let owner = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;
    let definition_id = create_net(&app, &owner).await;
    let session_id = start_session(&app, &owner, &definition_id).await;
    let a = add_check_in(&app, &owner, &session_id, "W1AAA").await;
    let b = add_check_in(&app, &owner, &session_id, "W1BBB").await;

    // Work A: the cursor points at A, nobody is worked yet.
    let (status, summary) = send_json(
        app.router(),
        "POST",
        &format!("/api/net-sessions/{session_id}/worked-station"),
        Some(json!({ "checkInId": a })),
        Some(&owner),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(summary["workingCheckInId"], a.as_str());
    assert_eq!(entry(&summary, &a)["worked"], false);

    // Move to B: A is now worked, the cursor points at B.
    let (status, summary) = send_json(
        app.router(),
        "POST",
        &format!("/api/net-sessions/{session_id}/worked-station"),
        Some(json!({ "checkInId": b })),
        Some(&owner),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(summary["workingCheckInId"], b.as_str());
    assert_eq!(entry(&summary, &a)["worked"], true);
    assert_eq!(entry(&summary, &b)["worked"], false);
    // The roster ORDER is unchanged — a cursor move is not a reorder.
    let order: Vec<_> = summary["roster"]
        .as_array()
        .expect("roster")
        .iter()
        .map(|e| e["callsign"].as_str().expect("callsign"))
        .collect();
    assert_eq!(order, vec!["W1AAA", "W1BBB"]);
}

#[tokio::test]
async fn clearing_the_worked_station_completes_it() {
    let app = test_app().await;
    let owner = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;
    let definition_id = create_net(&app, &owner).await;
    let session_id = start_session(&app, &owner, &definition_id).await;
    let a = add_check_in(&app, &owner, &session_id, "W1AAA").await;
    let _ = send_json(
        app.router(),
        "POST",
        &format!("/api/net-sessions/{session_id}/worked-station"),
        Some(json!({ "checkInId": a })),
        Some(&owner),
    )
    .await;

    let (status, summary) = send_json(
        app.router(),
        "POST",
        &format!("/api/net-sessions/{session_id}/worked-station"),
        Some(json!({ "checkInId": null })),
        Some(&owner),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(summary["workingCheckInId"].is_null());
    assert_eq!(entry(&summary, &a)["worked"], true);
}

#[tokio::test]
async fn a_logger_cannot_set_the_worked_station() {
    let app = test_app().await;
    let owner = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;
    let logger = sign_in_consent_callsign(&app, "logger@example.com", "w2bcd").await;
    let definition_id = create_net(&app, &owner).await;
    let session_id = start_session(&app, &owner, &definition_id).await;
    grant(&app, &owner, &session_id, "w2bcd", "logger").await;
    let a = add_check_in(&app, &owner, &session_id, "W1AAA").await;

    let (status, body) = send_json(
        app.router(),
        "POST",
        &format!("/api/net-sessions/{session_id}/worked-station"),
        Some(json!({ "checkInId": a })),
        Some(&logger),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(body["type"], "/errors/forbidden");
}

#[tokio::test]
async fn a_participant_cannot_set_the_worked_station() {
    let app = test_app().await;
    let owner = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;
    let participant = sign_in_consent(&app, "participant@example.com").await;
    let definition_id = create_net(&app, &owner).await;
    let session_id = start_session(&app, &owner, &definition_id).await;
    let a = add_check_in(&app, &owner, &session_id, "W1AAA").await;

    let (status, _) = send_json(
        app.router(),
        "POST",
        &format!("/api/net-sessions/{session_id}/worked-station"),
        Some(json!({ "checkInId": a })),
        Some(&participant),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn setting_the_worked_station_on_a_missing_session_is_404_before_403() {
    let app = test_app().await;
    let _owner = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;
    let stranger = sign_in_consent(&app, "stranger@example.com").await;
    let missing = uuid::Uuid::now_v7();

    let (status, body) = send_json(
        app.router(),
        "POST",
        &format!("/api/net-sessions/{missing}/worked-station"),
        Some(json!({ "checkInId": null })),
        Some(&stranger),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body["type"], "/errors/net-session-not-found");
}

#[tokio::test]
async fn an_off_roster_worked_station_target_is_404() {
    let app = test_app().await;
    let owner = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;
    let definition_id = create_net(&app, &owner).await;
    let session_id = start_session(&app, &owner, &definition_id).await;
    let phantom = uuid::Uuid::now_v7();

    let (status, body) = send_json(
        app.router(),
        "POST",
        &format!("/api/net-sessions/{session_id}/worked-station"),
        Some(json!({ "checkInId": phantom })),
        Some(&owner),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body["type"], "/errors/net-session-not-found");
}

#[tokio::test]
async fn setting_the_worked_station_on_a_closed_session_is_409() {
    let app = test_app().await;
    let owner = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;
    let definition_id = create_net(&app, &owner).await;
    let session_id = start_session(&app, &owner, &definition_id).await;
    let a = add_check_in(&app, &owner, &session_id, "W1AAA").await;
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
        &format!("/api/net-sessions/{session_id}/worked-station"),
        Some(json!({ "checkInId": a })),
        Some(&owner),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(body["type"], "/errors/session-already-closed");
}

// --- net-note ---------------------------------------------------------------

#[tokio::test]
async fn a_logger_sets_the_net_level_note() {
    let app = test_app().await;
    let owner = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;
    let logger = sign_in_consent_callsign(&app, "logger@example.com", "w2bcd").await;
    let definition_id = create_net(&app, &owner).await;
    let session_id = start_session(&app, &owner, &definition_id).await;
    grant(&app, &owner, &session_id, "w2bcd", "logger").await;

    let (status, summary) = send_json(
        app.router(),
        "PUT",
        &format!("/api/net-sessions/{session_id}/net-note"),
        Some(json!({ "note": "Weekly traffic net — all welcome" })),
        Some(&logger),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(summary["netNote"], "Weekly traffic net — all welcome");
}

#[tokio::test]
async fn a_relay_cannot_set_the_net_note() {
    let app = test_app().await;
    let owner = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;
    let relay = sign_in_consent_callsign(&app, "relay@example.com", "w2bcd").await;
    let definition_id = create_net(&app, &owner).await;
    let session_id = start_session(&app, &owner, &definition_id).await;
    grant(&app, &owner, &session_id, "w2bcd", "relay").await;

    let (status, body) = send_json(
        app.router(),
        "PUT",
        &format!("/api/net-sessions/{session_id}/net-note"),
        Some(json!({ "note": "hi" })),
        Some(&relay),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(body["type"], "/errors/forbidden");
}

#[tokio::test]
async fn an_over_bound_net_note_is_a_400_note_invalid() {
    let app = test_app().await;
    let owner = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;
    let definition_id = create_net(&app, &owner).await;
    let session_id = start_session(&app, &owner, &definition_id).await;

    // Well over MAX_NOTE_CHARS (2000).
    let huge = "x".repeat(2_500);
    let (status, body) = send_json(
        app.router(),
        "PUT",
        &format!("/api/net-sessions/{session_id}/net-note"),
        Some(json!({ "note": huge })),
        Some(&owner),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["type"], "/errors/note-invalid");
}

#[tokio::test]
async fn setting_the_net_note_on_a_closed_session_is_409() {
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
        "PUT",
        &format!("/api/net-sessions/{session_id}/net-note"),
        Some(json!({ "note": "too late" })),
        Some(&owner),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(body["type"], "/errors/session-already-closed");
}

// --- per-station notes on the edit path -------------------------------------

#[tokio::test]
async fn an_edit_setting_notes_folds_them_without_a_correction() {
    let app = test_app().await;
    let owner = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;
    let definition_id = create_net(&app, &owner).await;
    let session_id = start_session(&app, &owner, &definition_id).await;
    let check_in_id = add_check_in(&app, &owner, &session_id, "W9XYZ").await;

    let (status, summary) = send_json(
        app.router(),
        "PUT",
        &format!("/api/net-sessions/{session_id}/check-ins/{check_in_id}"),
        Some(json!({ "callsign": "W9XYZ", "notes": "handling one piece of traffic", "expectedVersion": 1 })),
        Some(&owner),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let e = entry(&summary, &check_in_id);
    assert_eq!(e["notes"], "handling one piece of traffic");
    // Notes derive NO correction.
    let corr = e["corrections"].as_array().expect("corrections");
    assert!(!corr.iter().any(|c| c["field"] == "notes"));
}

#[tokio::test]
async fn a_participant_cannot_set_notes_via_the_edit_endpoint() {
    let app = test_app().await;
    let owner = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;
    let participant = sign_in_consent(&app, "participant@example.com").await;
    let definition_id = create_net(&app, &owner).await;
    let session_id = start_session(&app, &owner, &definition_id).await;
    let check_in_id = add_check_in(&app, &owner, &session_id, "W9XYZ").await;

    let (status, _) = send_json(
        app.router(),
        "PUT",
        &format!("/api/net-sessions/{session_id}/check-ins/{check_in_id}"),
        Some(json!({ "callsign": "W9XYZ", "notes": "sneaky", "expectedVersion": 1 })),
        Some(&participant),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}

// --- public redaction -------------------------------------------------------

#[tokio::test]
async fn the_public_snapshot_shows_the_worked_station_and_the_public_note_but_never_the_staff_or_net_note()
 {
    let app = test_app().await;
    let owner = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;
    let definition_id = create_net(&app, &owner).await;
    let session_id = start_session(&app, &owner, &definition_id).await;
    let a = add_check_in(&app, &owner, &session_id, "W1AAA").await;

    // Set a per-station note, a net note, and the worked cursor.
    let _ = send_json(
        app.router(),
        "PUT",
        &format!("/api/net-sessions/{session_id}/check-ins/{a}"),
        Some(json!({
            "callsign": "W1AAA",
            "notes": "STAFF-ONLY: sounded rough, watch for a relay",
            "publicNote": "PUBLIC: relaying for W1BBB this round",
            "expectedVersion": 1
        })),
        Some(&owner),
    )
    .await;
    let _ = send_json(
        app.router(),
        "PUT",
        &format!("/api/net-sessions/{session_id}/net-note"),
        Some(json!({ "note": "operator-only net note" })),
        Some(&owner),
    )
    .await;
    let _ = send_json(
        app.router(),
        "POST",
        &format!("/api/net-sessions/{session_id}/worked-station"),
        Some(json!({ "checkInId": a })),
        Some(&owner),
    )
    .await;

    let (status, public) = send_json(
        app.router(),
        "GET",
        &format!("/api/net-sessions/{session_id}/live"),
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    // The worked station IS public radio data.
    assert_eq!(public["workingCheckInId"], a.as_str());
    // The net note never crosses the public wire.
    let obj = public.as_object().expect("object");
    assert!(
        !obj.contains_key("netNote"),
        "no net note on the public view"
    );
    // This half SPLITS IN TWO DIRECTIONS. The
    // per-station note is now two fields: the PUBLIC note reaches the observer,
    // the STAFF note does not. The net-level note assertion above is untouched
    // (it is a different field and stays operator-only).
    // The privacy inversion — publishing the STAFF note in the PUBLIC note's slot
    // — is caught by the absence of the staff STRING, not merely by the absence
    // of the `notes` KEY, and it is asserted FIRST on purpose: an assertion on
    // the public string's PRESENCE would fire first on that mutation and report
    // it as a mismatched value rather than as a leak (verified 2026-08-29, Task 9
    // mutation MUT-4).
    let raw = serde_json::to_string(&public).expect("json");
    assert!(
        !raw.contains("STAFF-ONLY: sounded rough, watch for a relay"),
        "the staff note never leaks in any shape: {raw}"
    );
    for e in public["roster"].as_array().expect("roster") {
        let eo = e.as_object().expect("entry object");
        assert_eq!(
            eo["publicNote"], "PUBLIC: relaying for W1BBB this round",
            "the public note reaches the account-less observer"
        );
        assert!(
            !eo.contains_key("notes"),
            "the STAFF note is still redacted from the public entry"
        );
        assert!(
            !eo.contains_key("worked"),
            "public entry has no worked flag"
        );
    }
    // The raw bytes carry neither the per-station STAFF note nor the NET note.
    // The case matches the fixture's own string: `contains` is case-SENSITIVE, so
    // the lowercase `"staff-only"` this line used to test for could not match the
    // seeded `"STAFF-ONLY: sounded rough…"` and asserted nothing at all. The
    // net-note half was always live and is unchanged.
    let raw = public.to_string();
    assert!(!raw.contains("STAFF-ONLY") && !raw.contains("operator-only net note"));
}
