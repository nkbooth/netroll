// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
//! Full-loop API tests for participant self-check-in and the widened self-edit /
//! self-checkout paths: real router, real Postgres (testcontainers),
//! capturing fake mailer. Asserts status codes, problem+json `type` slugs, body
//! values, the `source` discriminator, and log side-effects — never message
//! prose (house TDD rule).

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

/// Signs in, consents, and claims a callsign — the fully-gated participant.
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

/// Signs in and consents but claims NO callsign — an ungated (for self-check-in)
/// participant.
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

async fn set_profile(app: &TestApp, cookie: &str, display_name: &str, location: &str) {
    let (status, _) = send_json(
        app.router(),
        "PUT",
        "/api/accounts/me/profile",
        Some(json!({ "displayName": display_name, "location": location })),
        Some(cookie),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
}

async fn my_account_id(app: &TestApp, cookie: &str) -> String {
    let (status, body) =
        send_json(app.router(), "GET", "/api/accounts/me", None, Some(cookie)).await;
    assert_eq!(status, StatusCode::OK);
    body["id"].as_str().expect("account id").to_owned()
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

/// The id of the session's FIRST frozen connection — the only ids a `via` may
/// name, and the reason the write path 404s a stranger.
async fn first_connection_id(app: &TestApp, cookie: &str, session_id: &str) -> String {
    let (status, body) = send_json(
        app.router(),
        "GET",
        &format!("/api/net-sessions/{session_id}"),
        None,
        Some(cookie),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    body["connections"][0]["id"]
        .as_str()
        .expect("a session connection id")
        .to_owned()
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

/// The owner's view of the roster (the owner holds `ViewConsole`).
async fn owner_roster(app: &TestApp, owner: &str, session_id: &str) -> Vec<Value> {
    let (status, body) = send_json(
        app.router(),
        "GET",
        &format!("/api/net-sessions/{session_id}"),
        None,
        Some(owner),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    body["roster"].as_array().cloned().unwrap_or_default()
}

fn row<'a>(roster: &'a [Value], check_in_id: &str) -> &'a Value {
    roster
        .iter()
        .find(|e| e["checkInId"] == check_in_id)
        .expect("the row")
}

/// The participant self-checks in and returns the `checkInId` of their own entry.
async fn self_check_in(
    app: &TestApp,
    cookie: &str,
    session_id: &str,
    own_callsign: &str,
) -> String {
    let (status, body) = send_json(
        app.router(),
        "POST",
        &format!("/api/net-sessions/{session_id}/check-ins"),
        Some(json!({ "callsign": own_callsign })),
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

/// A staff operator adds a check-in and returns its `checkInId`.
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

/// The raw response body of a public GET (for redaction byte-assertions).
async fn get_raw(app: &TestApp, uri: &str) -> (StatusCode, String) {
    let request = Request::builder()
        .method("GET")
        .uri(uri)
        .body(Body::empty())
        .expect("build request");
    let response = app.router().oneshot(request).await.expect("route");
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024)
        .await
        .expect("body");
    (status, String::from_utf8(bytes.to_vec()).expect("utf8"))
}

// --- Self-toggle / self-checkout (own entry only) --------------------------

#[tokio::test]
async fn a_participant_toggles_their_own_staying_and_the_summary_folds_it() {
    let app = test_app().await;
    let owner = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;
    let participant = sign_in_consent_callsign(&app, "maria@example.com", "w2bcd").await;
    let definition_id = create_net(&app, &owner).await;
    let session_id = start_session(&app, &owner, &definition_id).await;
    let check_in_id = self_check_in(&app, &participant, &session_id, "w2bcd").await;

    let (status, body) = send_json(
        app.router(),
        "PUT",
        &format!("/api/net-sessions/{session_id}/check-ins/{check_in_id}"),
        Some(json!({
            "callsign": "w2bcd",
            "staying": "staying-for-comments",
            "expectedVersion": 1
        })),
        Some(&participant),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let roster = body["roster"].as_array().expect("roster");
    let entry = row(roster, &check_in_id);
    assert_eq!(entry["staying"], "staying-for-comments");
    assert_eq!(entry["source"], "self");
    assert_eq!(entry["version"], 2);
}

#[tokio::test]
async fn a_participant_cannot_edit_a_staff_entered_entry() {
    let app = test_app().await;
    let owner = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;
    let participant = sign_in_consent_callsign(&app, "maria@example.com", "w2bcd").await;
    let definition_id = create_net(&app, &owner).await;
    let session_id = start_session(&app, &owner, &definition_id).await;
    let staff_entry = staff_add(&app, &owner, &session_id, "W9XYZ").await;

    let (status, body) = send_json(
        app.router(),
        "PUT",
        &format!("/api/net-sessions/{session_id}/check-ins/{staff_entry}"),
        Some(
            json!({ "callsign": "W9XYZ", "staying": "staying-for-comments", "expectedVersion": 1 }),
        ),
        Some(&participant),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(body["type"], "/errors/forbidden");
    // The entry is unchanged (still version 1, in-and-out).
    let roster = owner_roster(&app, &owner, &session_id).await;
    assert_eq!(row(&roster, &staff_entry)["version"], 1);
}

#[tokio::test]
async fn a_participant_cannot_edit_another_participants_self_entry() {
    let app = test_app().await;
    let owner = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;
    let alice = sign_in_consent_callsign(&app, "alice@example.com", "w2bcd").await;
    let bob = sign_in_consent_callsign(&app, "bob@example.com", "w3cde").await;
    let definition_id = create_net(&app, &owner).await;
    let session_id = start_session(&app, &owner, &definition_id).await;
    let alice_entry = self_check_in(&app, &alice, &session_id, "w2bcd").await;
    let _bob_entry = self_check_in(&app, &bob, &session_id, "w3cde").await;

    // Bob tries to edit Alice's self-entry → 403.
    let (status, body) = send_json(
        app.router(),
        "PUT",
        &format!("/api/net-sessions/{session_id}/check-ins/{alice_entry}"),
        Some(
            json!({ "callsign": "w2bcd", "staying": "staying-for-comments", "expectedVersion": 1 }),
        ),
        Some(&bob),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(body["type"], "/errors/forbidden");
}

#[tokio::test]
async fn a_participant_changing_a_staff_field_on_their_own_entry_is_forbidden() {
    let app = test_app().await;
    let owner = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;
    let participant = sign_in_consent_callsign(&app, "maria@example.com", "w2bcd").await;
    let definition_id = create_net(&app, &owner).await;
    let session_id = start_session(&app, &owner, &definition_id).await;
    let check_in_id = self_check_in(&app, &participant, &session_id, "w2bcd").await;

    let (status, body) = send_json(
        app.router(),
        "PUT",
        &format!("/api/net-sessions/{session_id}/check-ins/{check_in_id}"),
        Some(json!({ "callsign": "w2bcd", "signalReport": "599", "expectedVersion": 1 })),
        Some(&participant),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(body["type"], "/errors/forbidden");
}

#[tokio::test]
async fn a_participant_cannot_write_a_public_note_on_their_own_entry() {
    // The public note is a NEW writable free-prose field on
    // a surface with no account gate, so the temptation is to let the station it
    // describes write it. It rides `EditCheckIn` (Logger+)
    // exactly as the staff note does: the self path stays the three-field staying
    // toggle. Refused server-side, with NO event appended
    // and the folded roster unchanged — the refusal is not merely a 403 status.
    let app = test_app().await;
    let owner = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;
    let participant = sign_in_consent_callsign(&app, "maria@example.com", "w2bcd").await;
    let definition_id = create_net(&app, &owner).await;
    let session_id = start_session(&app, &owner, &definition_id).await;
    let check_in_id = self_check_in(&app, &participant, &session_id, "w2bcd").await;

    let (status, body) = send_json(
        app.router(),
        "PUT",
        &format!("/api/net-sessions/{session_id}/check-ins/{check_in_id}"),
        Some(json!({
            "callsign": "w2bcd",
            "publicNote": "I am the net control station",
            "expectedVersion": 1
        })),
        Some(&participant),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(body["type"], "/errors/forbidden");

    // No event appended: the entry's CAS version is untouched, and no public note
    // reached the observer surface.
    let roster = owner_roster(&app, &owner, &session_id).await;
    let entry = row(&roster, &check_in_id);
    assert_eq!(entry["version"], 1, "no checkin.updated was appended");
    assert!(
        entry.as_object().expect("obj").get("publicNote").is_none(),
        "the refused note never folded onto the entry"
    );
}

#[tokio::test]
async fn a_participant_checks_themselves_out_via_delete() {
    let app = test_app().await;
    let owner = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;
    let participant = sign_in_consent_callsign(&app, "maria@example.com", "w2bcd").await;
    let definition_id = create_net(&app, &owner).await;
    let session_id = start_session(&app, &owner, &definition_id).await;
    let check_in_id = self_check_in(&app, &participant, &session_id, "w2bcd").await;

    let (status, _) = send_json(
        app.router(),
        "DELETE",
        &format!("/api/net-sessions/{session_id}/check-ins/{check_in_id}"),
        Some(json!({ "expectedVersion": 1 })),
        Some(&participant),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(owner_roster(&app, &owner, &session_id).await.is_empty());
}

#[tokio::test]
async fn a_participant_cannot_remove_another_stations_entry() {
    let app = test_app().await;
    let owner = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;
    let participant = sign_in_consent_callsign(&app, "maria@example.com", "w2bcd").await;
    let definition_id = create_net(&app, &owner).await;
    let session_id = start_session(&app, &owner, &definition_id).await;
    let staff_entry = staff_add(&app, &owner, &session_id, "W9XYZ").await;

    let (status, body) = send_json(
        app.router(),
        "DELETE",
        &format!("/api/net-sessions/{session_id}/check-ins/{staff_entry}"),
        Some(json!({ "expectedVersion": 1 })),
        Some(&participant),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(body["type"], "/errors/forbidden");
    // Still on the roster.
    let roster = owner_roster(&app, &owner, &session_id).await;
    assert_eq!(roster.len(), 1);
}

#[tokio::test]
async fn the_public_live_view_carries_source_but_never_added_by_or_definition_id() {
    let app = test_app().await;
    let owner = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;
    let participant = sign_in_consent_callsign(&app, "maria@example.com", "w2bcd").await;
    let definition_id = create_net(&app, &owner).await;
    let session_id = start_session(&app, &owner, &definition_id).await;
    staff_add(&app, &owner, &session_id, "W9XYZ").await;
    self_check_in(&app, &participant, &session_id, "w2bcd").await;

    let (status, raw) = get_raw(&app, &format!("/api/net-sessions/{session_id}/live")).await;
    assert_eq!(status, StatusCode::OK);
    // Source IS public provenance and both variants appear on the redacted roster.
    assert!(
        raw.contains("\"source\""),
        "public roster carries source: {raw}"
    );
    assert!(
        raw.contains("\"self\""),
        "the self entry's badge crosses: {raw}"
    );
    assert!(
        raw.contains("\"staff\""),
        "the staff entry's badge crosses: {raw}"
    );
    // The operator account id and internal definition ids stay REDACTED.
    assert!(!raw.contains("addedBy"), "addedBy must never leak: {raw}");
    assert!(
        !raw.contains("definitionId"),
        "definitionId must never leak: {raw}"
    );
}

// --- The grid's visibility posture and the participant freeze ----

#[tokio::test]
async fn a_participant_edit_cannot_wipe_the_grid() {
    // A participant may toggle ONLY their own `staying`; every
    // other field is FORCED to the entry's current value. Without `grid` in that
    // frozen tuple a self toggle would silently wipe a grid staff recorded.
    let app = test_app().await;
    let owner = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;
    let participant = sign_in_consent_callsign(&app, "maria@example.com", "w2bcd").await;
    let definition_id = create_net(&app, &owner).await;
    let session_id = start_session(&app, &owner, &definition_id).await;
    let check_in_id = self_check_in(&app, &participant, &session_id, "w2bcd").await;

    // Staff record a grid on the participant's entry.
    let (status, body) = send_json(
        app.router(),
        "PUT",
        &format!("/api/net-sessions/{session_id}/check-ins/{check_in_id}"),
        Some(json!({
            "callsign": "w2bcd",
            "grid": "FN31pr",
            "expectedVersion": 1
        })),
        Some(&owner),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let roster = body["roster"].as_array().expect("roster");
    assert_eq!(
        row(roster, &check_in_id)["grid"],
        "FN31pr",
        "the fixture genuinely has a grid before the participant edit"
    );

    // The participant toggles their own staying — the grid must survive.
    let (status, body) = send_json(
        app.router(),
        "PUT",
        &format!("/api/net-sessions/{session_id}/check-ins/{check_in_id}"),
        Some(json!({
            "callsign": "w2bcd",
            "staying": "staying-for-comments",
            "expectedVersion": 2
        })),
        Some(&participant),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let roster = body["roster"].as_array().expect("roster");
    let entry = row(roster, &check_in_id);
    assert_eq!(entry["staying"], "staying-for-comments");
    assert_eq!(
        entry["grid"], "FN31pr",
        "the staff grid survives the toggle"
    );
    // …and no phantom grid correction was derived for an unchanged field.
    let grid_corrections = entry["corrections"]
        .as_array()
        .expect("corrections")
        .iter()
        .filter(|c| c["field"] == "grid")
        .count();
    assert_eq!(grid_corrections, 1, "only the staff set, not the toggle");
}

#[tokio::test]
async fn a_participants_own_staying_toggle_preserves_every_other_field() {
    // The self arm of `edit_check_in` is a
    // `RosterEntry` PROJECTION — it reads the stored entry and re-emits it as the
    // full post-edit `checkin.updated` field set, so a field it fails to carry is
    // WIPED, not merely un-editable. The exhaustive destructure there makes a new
    // domain field fail to compile; this test is the behaviour half, and it is
    // what goes red if a future author names a field and then drops it.
    // `a_participant_edit_cannot_wipe_the_grid` above covers `grid` alone; this
    // covers every field the tuple carries.
    let app = test_app().await;
    let owner = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;
    let participant = sign_in_consent_callsign(&app, "maria@example.com", "w2bcd").await;
    let definition_id = create_net(&app, &owner).await;
    let session_id = start_session(&app, &owner, &definition_id).await;
    let check_in_id = self_check_in(&app, &participant, &session_id, "w2bcd").await;
    // The way in is a connection of THIS session's frozen snapshot,
    // named by its own id — never by position, and never invented.
    let connection_id = first_connection_id(&app, &owner, &session_id).await;

    // Staff log every editable field on the participant's own entry.
    let (status, body) = send_json(
        app.router(),
        "PUT",
        &format!("/api/net-sessions/{session_id}/check-ins/{check_in_id}"),
        Some(json!({
            "callsign": "w2bcd",
            "name": "Maria",
            "location": "Hartford, CT",
            "grid": "FN31pr",
            "signalReport": "59",
            "precedence": "emergency",
            "traffic": 3,
            "notes": "handling one piece of traffic",
            // Sites 7 and 8: the public note is the field where the
            // field-wipe is most visible and least recoverable — the operator's
            // typed text is simply gone, from the surface with the widest
            // audience. Seeded here so the toggle below has something to destroy.
            "publicNote": "relaying for the county EOC",
            // `via` JOINS `CheckinUpdated`, so by this site's
            // own rule it joins the self arm's tuple. Seeded here so the toggle
            // below has a way in to destroy — and losing one is DATA LOSS, not a
            // thin output: nothing else in the log records which way a station
            // came in on.
            "via": { "kind": "connection", "connectionId": connection_id },
            // `relayed_by` JOINS `CheckinUpdated`, so by this
            // site's own rule it joins the self arm's tuple too. Seeded here so
            // the toggle below has a relaying station to destroy. It is a
            // DIFFERENT fact from `via` — one says how the traffic travelled,
            // the other who passed it — and W3REL holds no NetRoll account,
            // which is the ordinary on-air case.
            "relayedBy": "w3rel",
            "expectedVersion": 1
        })),
        Some(&owner),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let roster = body["roster"].as_array().expect("roster");
    let seeded = row(roster, &check_in_id).clone();
    assert_eq!(
        seeded["via"]["connectionId"], connection_id,
        "the fixture genuinely has a way in"
    );
    assert_eq!(
        seeded["relayedBy"], "W3REL",
        "the fixture genuinely has a relaying station"
    );
    assert_eq!(seeded["name"], "Maria", "the fixture genuinely has a name");
    assert_eq!(seeded["traffic"], 3, "the fixture genuinely has traffic");
    assert_eq!(
        seeded["publicNote"], "relaying for the county EOC",
        "the fixture genuinely has a public note"
    );

    // The participant toggles their OWN staying, and submits a DIFFERENT
    // callsign with it. The self arm forces `callsign` from the stored entry and
    // never parses the submitted one, so sending `w2bcd` back would make the
    // survival assertion below hold either way — an assertion that cannot fail.
    let (status, body) = send_json(
        app.router(),
        "PUT",
        &format!("/api/net-sessions/{session_id}/check-ins/{check_in_id}"),
        Some(json!({
            "callsign": "k9zzz",
            "staying": "staying-for-comments",
            "expectedVersion": 2
        })),
        Some(&participant),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let roster = body["roster"].as_array().expect("roster");
    let entry = row(roster, &check_in_id);
    assert_eq!(
        entry["staying"], "staying-for-comments",
        "the toggle DID change staying"
    );
    assert_eq!(
        entry["callsign"], "W2BCD",
        "the self arm forced the stored callsign, not the submitted `k9zzz`"
    );
    assert_eq!(entry["name"], "Maria", "name survived");
    assert_eq!(entry["location"], "Hartford, CT", "location survived");
    assert_eq!(entry["grid"], "FN31pr", "grid survived");
    assert_eq!(entry["signalReport"], "59", "signal report survived");
    assert_eq!(entry["precedence"], "emergency", "precedence survived");
    assert_eq!(entry["traffic"], 3, "traffic survived");
    assert_eq!(
        entry["publicNote"], "relaying for the county EOC",
        "the PUBLIC note survived the participant's own staying toggle"
    );
    assert_eq!(
        entry["notes"], "handling one piece of traffic",
        "notes survived"
    );
    assert_eq!(
        entry["via"]["connectionId"], connection_id,
        "the WAY IN survived the participant's own staying toggle"
    );
    assert_eq!(entry["via"]["kind"], "connection");
    assert_eq!(
        entry["relayedBy"], "W3REL",
        "the RELAYING STATION survived the participant's own staying toggle"
    );
    // A wipe is visible in the append-only LOG even where the read model looks
    // intact, so assert the toggle derived exactly ONE correction — its own.
    let non_staying = |value: &Value| {
        value["corrections"]
            .as_array()
            .expect("corrections")
            .iter()
            .filter(|c| c["field"] != "staying")
            .count()
    };
    assert_eq!(
        non_staying(entry),
        non_staying(&seeded),
        "the toggle derived a correction for a field it was not asked to change"
    );
}

#[tokio::test]
async fn the_public_roster_carries_neither_location_nor_grid() {
    // A Maidenhead grid is a MORE precise location than
    // the free-text field the public roster already withholds, so it inherits
    // location's posture exactly. That half is UNCHANGED.
    //
    // The EXACT key set is amended, and it stays an EXACT
    // key-set assertion on purpose. Relaxing it to `contains_key` checks would
    // disarm the one line in the tree that fails when an UNPLANNED field joins
    // the public projection. Four keys became eight by explicit ruling
    // (2026-08-27); the next unplanned one must still fail here.
    let app = test_app().await;
    let owner = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;
    let definition_id = create_net(&app, &owner).await;
    let session_id = start_session(&app, &owner, &definition_id).await;
    let check_in_id = staff_add(&app, &owner, &session_id, "W9XYZ").await;
    let (status, body) = send_json(
        app.router(),
        "PUT",
        &format!("/api/net-sessions/{session_id}/check-ins/{check_in_id}"),
        Some(json!({
            "callsign": "W9XYZ",
            "location": "Hartford, CT",
            "grid": "FN31pr",
            // Seeded so every OPTIONAL public key is genuinely PRESENT: an
            // omitted-when-absent field cannot prove an exact key set if the
            // fixture never populates it.
            "traffic": 4,
            "notes": "STAFF-ONLY commentary",
            "publicNote": "PUBLIC note for the observer",
            // `via` is a FIFTH field carried on this DTO, so it is
            // seeded here — an omitted-when-absent field cannot prove an exact
            // key set if the fixture never populates it, which is the same trap
            // the four fields above are seeded against.
            "via": { "kind": "unlisted", "text": "a phone patch" },
            "expectedVersion": 1
        })),
        Some(&owner),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        row(body["roster"].as_array().expect("roster"), &check_in_id)["grid"],
        "FN31pr",
        "the fixture genuinely has a grid on the OWNER wire"
    );

    let (status, raw) = get_raw(&app, &format!("/api/net-sessions/{session_id}/live")).await;
    assert_eq!(status, StatusCode::OK);
    let public: Value = serde_json::from_str(&raw).expect("json");
    let entry = &public["roster"].as_array().expect("public roster")[0];
    let mut keys: Vec<&str> = entry
        .as_object()
        .expect("obj")
        .keys()
        .map(String::as_str)
        .collect();
    keys.sort_unstable();
    assert_eq!(
        keys,
        vec![
            "addedAt",
            "callsign",
            "checkInId",
            "precedence",
            "publicNote",
            "source",
            "staying",
            "traffic",
            // WHICH way in the station came in on: the roster's `via` is the
            // visible payoff of this epic on
            // the public view, the connection id is a snapshot-local identifier
            // this same view already publishes under `connections`, and neither
            // variant is an account or an operator identity. The default answer
            // for the NEXT per-check-in field here is still NO.
            "via",
        ]
    );
    assert!(!raw.contains("FN31pr"), "the grid never leaks: {raw}");
    assert!(!raw.contains("Hartford"), "the location never leaks: {raw}");
    assert!(
        !raw.contains("STAFF-ONLY commentary"),
        "the STAFF note never leaks: {raw}"
    );
}

// --- Self check-in (create) ------------------------------------------------

#[tokio::test]
async fn a_participant_self_checks_in_with_source_self_forced_callsign_and_profile_identity() {
    let app = test_app().await;
    let owner = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;
    let participant = sign_in_consent_callsign(&app, "maria@example.com", "w2bcd").await;
    set_profile(&app, &participant, "Maria", "Hartford, CT").await;
    let participant_id = my_account_id(&app, &participant).await;
    let definition_id = create_net(&app, &owner).await;
    let session_id = start_session(&app, &owner, &definition_id).await;

    // The participant supplies a DIFFERENT callsign; the server FORCES their own.
    let (status, body) = send_json(
        app.router(),
        "POST",
        &format!("/api/net-sessions/{session_id}/check-ins"),
        Some(json!({ "callsign": "W9ZZZ" })),
        Some(&participant),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);

    let roster = body["roster"].as_array().expect("roster");
    assert_eq!(roster.len(), 1);
    let entry = &roster[0];
    // Own callsign forced (the client "W9ZZZ" is ignored — no injection).
    assert_eq!(entry["callsign"], "W2BCD");
    assert_eq!(entry["source"], "self");
    assert_eq!(entry["addedBy"], participant_id);
    // name/location derive server-side from the account profile.
    assert_eq!(entry["name"], "Maria");
    assert_eq!(entry["location"], "Hartford, CT");
}

#[tokio::test]
async fn a_self_check_in_derives_the_grid_from_the_account_profile() {
    // The self branch's contract is
    // "identity comes from the profile, never client-arbitrary", so a profile
    // that holds a validated grid must not silently drop it. `Account.grid` is a
    // plain `Option<String>`, so it is RE-parsed through the same grammar.
    let app = test_app().await;
    let owner = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;
    let participant = sign_in_consent_callsign(&app, "maria@example.com", "w2bcd").await;
    let (status, _) = send_json(
        app.router(),
        "PUT",
        "/api/accounts/me/profile",
        Some(json!({ "displayName": "Maria", "location": "Hartford, CT", "grid": "fn31PR" })),
        Some(&participant),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let definition_id = create_net(&app, &owner).await;
    let session_id = start_session(&app, &owner, &definition_id).await;

    // A client-supplied grid on the self path is IGNORED, like every other
    // identity field — the profile is the only source.
    let (status, body) = send_json(
        app.router(),
        "POST",
        &format!("/api/net-sessions/{session_id}/check-ins"),
        Some(json!({ "callsign": "W9ZZZ", "grid": "JJ00aa" })),
        Some(&participant),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    let entry = &body["roster"].as_array().expect("roster")[0];
    assert_eq!(entry["callsign"], "W2BCD");
    assert_eq!(entry["grid"], "FN31pr", "the PROFILE grid, canonicalized");
}

#[tokio::test]
async fn a_self_check_in_without_a_profile_grid_records_no_grid() {
    // The companion negative: a profile with NO grid must produce an ABSENT
    // grid on the entry, not an empty string. `set_profile` deliberately never
    // sends a grid, so this fixture genuinely exercises the None path.
    //
    // Honest note: this test PASSED before the implementation landed (nothing
    // set a grid then), so it was never a red. It earns its place as the
    // regression guard on `parse_edit_grid(None) == Ok(None)` — the arm that
    // would 400 every grid-less self check-in if the blank/None guard were ever
    // dropped, since `parse_grid("")` is `Err(Empty)`.
    let app = test_app().await;
    let owner = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;
    let participant = sign_in_consent_callsign(&app, "maria@example.com", "w2bcd").await;
    set_profile(&app, &participant, "Maria", "Hartford, CT").await;
    let definition_id = create_net(&app, &owner).await;
    let session_id = start_session(&app, &owner, &definition_id).await;

    let (status, body) = send_json(
        app.router(),
        "POST",
        &format!("/api/net-sessions/{session_id}/check-ins"),
        Some(json!({ "callsign": "w2bcd" })),
        Some(&participant),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    let entry = &body["roster"].as_array().expect("roster")[0];
    assert_eq!(entry["location"], "Hartford, CT");
    assert!(
        entry.as_object().expect("obj").get("grid").is_none(),
        "no profile grid means an ABSENT grid: {entry}"
    );
}

#[tokio::test]
async fn a_spoofed_source_field_in_the_self_add_body_is_silently_ignored() {
    let app = test_app().await;
    let owner = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;
    let participant = sign_in_consent_callsign(&app, "maria@example.com", "w2bcd").await;
    let definition_id = create_net(&app, &owner).await;
    let session_id = start_session(&app, &owner, &definition_id).await;

    // A Participant has no `source`/`addedBy` field on the wire request type at
    // all — attempting to spoof `source: "staff"` (or any other client-
    // supplied value) alongside a forced callsign attempt must have NO effect:
    // the server derives `source` purely from which capability branch resolved,
    // never from request body content.
    let (status, body) = send_json(
        app.router(),
        "POST",
        &format!("/api/net-sessions/{session_id}/check-ins"),
        Some(json!({ "callsign": "W9ZZZ", "source": "staff", "addedBy": "anyone" })),
        Some(&participant),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);

    let roster = body["roster"].as_array().expect("roster");
    assert_eq!(roster.len(), 1);
    let entry = &roster[0];
    assert_eq!(entry["callsign"], "W2BCD", "own callsign still forced");
    assert_eq!(
        entry["source"], "self",
        "spoofed source=staff had no effect"
    );
}

#[tokio::test]
async fn a_participant_without_a_callsign_is_refused_callsign_required_with_no_event() {
    let app = test_app().await;
    let owner = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;
    // Consented but NO callsign — the frontend routes this 4xx to callsign setup.
    let participant = sign_in_consent(&app, "nocall@example.com").await;
    let definition_id = create_net(&app, &owner).await;
    let session_id = start_session(&app, &owner, &definition_id).await;

    let (status, body) = send_json(
        app.router(),
        "POST",
        &format!("/api/net-sessions/{session_id}/check-ins"),
        Some(json!({ "callsign": "w2bcd" })),
        Some(&participant),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(body["type"], "/errors/callsign-required");
    assert!(owner_roster(&app, &owner, &session_id).await.is_empty());
}

#[tokio::test]
async fn a_callsign_less_account_s_doomed_retries_never_spend_the_rate_limiter() {
    let app = test_app().await;
    let owner = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;
    // Consented but NO callsign — every self-add attempt is refused
    // `callsign-required` and can never succeed until the account claims one.
    let participant = sign_in_consent(&app, "nocall@example.com").await;
    let definition_id = create_net(&app, &owner).await;
    let session_id = start_session(&app, &owner, &definition_id).await;

    // Flood well past the burst quota (10) with doomed requests — the
    // verified-email/callsign preconditions are checked BEFORE the limiter is
    // charged, so none of these should ever spend a burst cell.
    for _ in 0..15 {
        let (status, body) = send_json(
            app.router(),
            "POST",
            &format!("/api/net-sessions/{session_id}/check-ins"),
            Some(json!({ "callsign": "w2bcd" })),
            Some(&participant),
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        assert_eq!(body["type"], "/errors/callsign-required");
    }

    // Now claim a callsign and self-check-in — this must succeed (201), proving
    // none of the 15 doomed attempts above consumed the limiter's burst quota.
    let (status, _) = send_json(
        app.router(),
        "PUT",
        "/api/accounts/me/callsign",
        Some(json!({ "callsign": "w2bcd" })),
        Some(&participant),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let (status, _) = send_json(
        app.router(),
        "POST",
        &format!("/api/net-sessions/{session_id}/check-ins"),
        Some(json!({ "callsign": "w2bcd" })),
        Some(&participant),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::CREATED,
        "the account's first eligible self-add must not be pre-emptively rate-limited \
         by its own earlier doomed (callsign-less) attempts"
    );
}

#[tokio::test]
async fn a_participant_with_an_unverified_email_is_refused_with_no_event() {
    let app = test_app().await;
    let owner = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;
    let participant = sign_in_consent_callsign(&app, "unverified@example.com", "w2bcd").await;
    let definition_id = create_net(&app, &owner).await;
    let session_id = start_session(&app, &owner, &definition_id).await;

    // Force the email unverified server-side (the magic-link flow always verifies,
    // so this defensive guard is only reachable by clearing the column directly).
    sqlx::query("UPDATE accounts SET email_verified_at = NULL WHERE email = $1")
        .bind("unverified@example.com")
        .execute(&app.pool)
        .await
        .expect("unverify email");

    let (status, body) = send_json(
        app.router(),
        "POST",
        &format!("/api/net-sessions/{session_id}/check-ins"),
        Some(json!({ "callsign": "w2bcd" })),
        Some(&participant),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(body["type"], "/errors/email-unverified");
    assert!(owner_roster(&app, &owner, &session_id).await.is_empty());
}

#[tokio::test]
async fn a_participant_sending_a_signal_report_is_refused_forbidden_with_no_event() {
    let app = test_app().await;
    let owner = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;
    let participant = sign_in_consent_callsign(&app, "maria@example.com", "w2bcd").await;
    let definition_id = create_net(&app, &owner).await;
    let session_id = start_session(&app, &owner, &definition_id).await;

    let (status, body) = send_json(
        app.router(),
        "POST",
        &format!("/api/net-sessions/{session_id}/check-ins"),
        Some(json!({ "callsign": "w2bcd", "signalReport": "599" })),
        Some(&participant),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(body["type"], "/errors/forbidden");
    assert!(owner_roster(&app, &owner, &session_id).await.is_empty());
}

#[tokio::test]
async fn a_participant_may_name_a_listed_way_in_on_their_own_check_in() {
    // The accept half of the pair below. A rank-0 participant choosing among the
    // ids the OWNER published is the field working as intended — it is a choice
    // from a closed set, not a string they author.
    let app = test_app().await;
    let owner = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;
    let participant = sign_in_consent_callsign(&app, "maria@example.com", "w2bcd").await;
    let definition_id = create_net(&app, &owner).await;
    let session_id = start_session(&app, &owner, &definition_id).await;
    let connection_id = first_connection_id(&app, &owner, &session_id).await;

    let (status, body) = send_json(
        app.router(),
        "POST",
        &format!("/api/net-sessions/{session_id}/check-ins"),
        Some(json!({
            "callsign": "w2bcd",
            "via": { "kind": "connection", "connectionId": connection_id },
        })),
        Some(&participant),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    let entry = &body["roster"].as_array().expect("roster")[0];
    assert_eq!(entry["source"], "self");
    assert_eq!(entry["via"]["kind"], "connection");
    assert_eq!(entry["via"]["connectionId"], connection_id);
}

#[tokio::test]
async fn a_participant_writing_free_text_as_their_way_in_is_refused_with_no_event() {
    // Free text is PARTICIPANT-SUPPLIED PROSE that renders unfiltered on the
    // unauthenticated public roster, and that class is gated at
    // Logger+ when it gated `publicNote`. Before this refusal, `name`,
    // `location` and `grid` were forced from the account profile and
    // `signal_report` was a 403 — no participant-typed string reached that page.
    //
    // An unlisted connection mid-net is still FREE TEXT: net control records
    // it, on the staff path, which is the test
    // immediately below.
    let app = test_app().await;
    let owner = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;
    let participant = sign_in_consent_callsign(&app, "maria@example.com", "w2bcd").await;
    let definition_id = create_net(&app, &owner).await;
    let session_id = start_session(&app, &owner, &definition_id).await;

    let (status, body) = send_json(
        app.router(),
        "POST",
        &format!("/api/net-sessions/{session_id}/check-ins"),
        Some(json!({
            "callsign": "w2bcd",
            "via": { "kind": "unlisted", "text": "Bob's hotspot" },
        })),
        Some(&participant),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(body["type"], "/errors/forbidden");
    assert!(
        owner_roster(&app, &owner, &session_id).await.is_empty(),
        "no event was appended"
    );
}

#[tokio::test]
async fn net_control_may_still_record_a_way_in_the_net_never_listed() {
    // The staff half of epic ruling #5, asserted so the refusal above cannot be
    // read as retiring free text altogether.
    let app = test_app().await;
    let owner = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;
    let definition_id = create_net(&app, &owner).await;
    let session_id = start_session(&app, &owner, &definition_id).await;

    let (status, body) = send_json(
        app.router(),
        "POST",
        &format!("/api/net-sessions/{session_id}/check-ins"),
        Some(json!({
            "callsign": "W9XYZ",
            "via": { "kind": "unlisted", "text": "Bob's hotspot" },
        })),
        Some(&owner),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    let entry = &body["roster"].as_array().expect("roster")[0];
    assert_eq!(entry["via"]["kind"], "unlisted");
    assert_eq!(entry["via"]["text"], "Bob's hotspot");
}

#[tokio::test]
async fn a_free_text_way_in_is_bounded_and_single_line_and_the_error_names_the_way_in() {
    // This arm once ran on `parse_note`: 2000 characters, newlines
    // DELIBERATELY allowed, and a rejection answering `/errors/note-invalid`
    // with a detail naming "note" on a request carrying no note at all — which
    // breaks the rule that an error names the FAULTING field.
    let app = test_app().await;
    let owner = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;
    let definition_id = create_net(&app, &owner).await;
    let session_id = start_session(&app, &owner, &definition_id).await;

    for text in [
        // Far shorter than a note's 2000 and still refused.
        json!("x".repeat(65)),
        // A note collapses newlines and keeps the text; a way in is one line.
        json!("club\nhotspot"),
        // Blank after trim: the `Unlisted` → `NotRecorded` collapse
        // `ViaDisplay`'s own doc forbids. A body that says `unlisted` has
        // asserted a way in EXISTS; the way to say "not recorded" is to omit
        // the key.
        json!("   "),
    ] {
        let (status, body) = send_json(
            app.router(),
            "POST",
            &format!("/api/net-sessions/{session_id}/check-ins"),
            Some(json!({ "callsign": "W9XYZ", "via": { "kind": "unlisted", "text": text } })),
            Some(&owner),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "for {text}");
        assert_eq!(body["type"], "/errors/via-invalid", "for {text}");
        let detail = body["detail"].as_str().expect("a detail").to_lowercase();
        assert!(
            detail.contains("way in"),
            "the detail names the faulting field, got {detail:?}"
        );
        assert!(
            !detail.contains("note"),
            "and it never names a field this request does not carry, got {detail:?}"
        );
        assert!(
            owner_roster(&app, &owner, &session_id).await.is_empty(),
            "no event was appended for {text}"
        );
    }
}

#[tokio::test]
async fn the_staff_log_check_in_path_still_records_source_staff() {
    let app = test_app().await;
    let owner = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;
    let definition_id = create_net(&app, &owner).await;
    let session_id = start_session(&app, &owner, &definition_id).await;

    // A staff operator (the owner) adds someone else — an arbitrary callsign is
    // allowed on the staff path, and the entry is source=staff (unchanged).
    let (status, body) = send_json(
        app.router(),
        "POST",
        &format!("/api/net-sessions/{session_id}/check-ins"),
        Some(json!({ "callsign": "W9XYZ" })),
        Some(&owner),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    let roster = body["roster"].as_array().expect("roster");
    assert_eq!(roster[0]["callsign"], "W9XYZ");
    assert_eq!(roster[0]["source"], "staff");
}

#[tokio::test]
async fn a_second_self_add_for_the_same_account_is_an_idempotent_no_op() {
    let app = test_app().await;
    let owner = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;
    let participant = sign_in_consent_callsign(&app, "maria@example.com", "w2bcd").await;
    let definition_id = create_net(&app, &owner).await;
    let session_id = start_session(&app, &owner, &definition_id).await;

    let (first, _) = send_json(
        app.router(),
        "POST",
        &format!("/api/net-sessions/{session_id}/check-ins"),
        Some(json!({ "callsign": "w2bcd" })),
        Some(&participant),
    )
    .await;
    assert_eq!(first, StatusCode::CREATED);

    // A second self-add (no clientEventId) is an idempotent no-op → 200, and the
    // roster still holds exactly one self-entry (no duplicate row).
    let (second, body) = send_json(
        app.router(),
        "POST",
        &format!("/api/net-sessions/{session_id}/check-ins"),
        Some(json!({ "callsign": "w2bcd" })),
        Some(&participant),
    )
    .await;
    assert_eq!(second, StatusCode::OK);
    let roster = body["roster"].as_array().expect("roster");
    assert_eq!(roster.len(), 1);
    assert_eq!(roster[0]["source"], "self");
}

#[tokio::test]
async fn the_self_check_in_write_is_per_account_rate_limited_while_staff_is_unaffected() {
    let app = test_app().await;
    let owner = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;
    let participant = sign_in_consent_callsign(&app, "maria@example.com", "w2bcd").await;
    let definition_id = create_net(&app, &owner).await;
    let session_id = start_session(&app, &owner, &definition_id).await;

    // The per-account self-check-in limiter has a burst of 10.
    // The first self-add creates the entry; each subsequent attempt is an
    // idempotent no-op but still spends a limiter cell (the limiter is checked
    // FIRST), so the burst is exhausted and a later attempt is 429.
    let mut saw_rate_limit = false;
    for _ in 0..15 {
        let (status, _) = send_json(
            app.router(),
            "POST",
            &format!("/api/net-sessions/{session_id}/check-ins"),
            Some(json!({ "callsign": "w2bcd" })),
            Some(&participant),
        )
        .await;
        if status == StatusCode::TOO_MANY_REQUESTS {
            saw_rate_limit = true;
            break;
        }
    }
    assert!(
        saw_rate_limit,
        "the self-add flood must hit the 429 ceiling"
    );

    // The staff LogCheckIn path is UNGOVERNED — the owner can burst-log freely,
    // never touching the self limiter's bucket.
    for n in 0..12u8 {
        let suffix = (b'A' + n) as char;
        let (status, _) = send_json(
            app.router(),
            "POST",
            &format!("/api/net-sessions/{session_id}/check-ins"),
            Some(json!({ "callsign": format!("W9AB{suffix}") })),
            Some(&owner),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED, "staff add {n} is ungoverned");
    }
}

#[tokio::test]
async fn a_participant_may_not_claim_a_relaying_station_on_their_own_check_in() {
    // A 403 on PRESENCE, mirroring `signal_report` and the
    // free-text half of `via`. The reason is STRUCTURAL rather than policy: a
    // participant self-checking in reached the net THROUGH THE APP, so there was
    // no relaying station in that act and the claim is one this path cannot
    // make. It is also what makes the history and the export never carry it,
    // by construction instead of by convention.
    let app = test_app().await;
    let owner = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;
    let participant = sign_in_consent_callsign(&app, "maria@example.com", "w2bcd").await;
    let definition_id = create_net(&app, &owner).await;
    let session_id = start_session(&app, &owner, &definition_id).await;

    // A VALID relaying callsign — the refusal is on presence, not on validity.
    let (status, _) = send_json(
        app.router(),
        "POST",
        &format!("/api/net-sessions/{session_id}/check-ins"),
        Some(json!({ "callsign": "w2bcd", "relayedBy": "W3REL" })),
        Some(&participant),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    // NO event was appended: the participant still has no entry at all, so a
    // plain self check-in below is their FIRST and lands 201 rather than the
    // idempotent 200 a phantom entry would have produced.
    let (status, body) = send_json(
        app.router(),
        "POST",
        &format!("/api/net-sessions/{session_id}/check-ins"),
        Some(json!({ "callsign": "w2bcd" })),
        Some(&participant),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::CREATED,
        "the refused request appended nothing"
    );
    let roster = body["roster"].as_array().expect("roster");
    assert_eq!(roster.len(), 1);
    assert!(
        roster[0]
            .as_object()
            .expect("obj")
            .get("relayedBy")
            .is_none(),
        "and no relaying station reached the entry by any other route"
    );
}

#[tokio::test]
async fn an_invalid_relaying_station_on_the_self_path_is_still_a_403_on_presence() {
    // The refusal is on PRESENCE — value, validity and blankness are all
    // irrelevant, in the handler's own words.
    // The sibling test above proves that for a VALID callsign only, which is
    // exactly half the claim: a participant sending a MALFORMED one must get the
    // same 403, because a path that may not make the claim at all cannot first
    // adjudicate whether the claim is well-formed. Handing back
    // `/errors/relayed-by-invalid` would also tell a participant which relaying
    // callsigns the parser would have accepted, on a path where the answer is
    // always "none of them".
    let app = test_app().await;
    let owner = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;
    let participant = sign_in_consent_callsign(&app, "maria@example.com", "w2bcd").await;
    let definition_id = create_net(&app, &owner).await;
    let session_id = start_session(&app, &owner, &definition_id).await;

    for hostile in ["not a callsign", "W1AW/A/B/C/D", "\u{202e}W1AW", ""] {
        let (status, _) = send_json(
            app.router(),
            "POST",
            &format!("/api/net-sessions/{session_id}/check-ins"),
            Some(json!({ "callsign": "w2bcd", "relayedBy": hostile })),
            Some(&participant),
        )
        .await;
        assert_eq!(
            status,
            StatusCode::FORBIDDEN,
            "presence, not validity, decides the self path: {hostile:?}"
        );
    }

    // None of the four appended: the participant's first REAL check-in is a 201,
    // not the idempotent 200 a phantom entry would have produced.
    let (status, _) = send_json(
        app.router(),
        "POST",
        &format!("/api/net-sessions/{session_id}/check-ins"),
        Some(json!({ "callsign": "w2bcd" })),
        Some(&participant),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "no refusal appended anything");
}

#[tokio::test]
async fn a_refused_relaying_station_never_charges_the_self_check_in_limiter() {
    // The 403 must fire BEFORE the limiter. When the presence check sat after
    // the limiter charge, a claim this path can NEVER accept still burned the
    // participant's quota. `SELF_CHECK_IN_BURST` is 10 with one cell back every
    // 3 seconds, so eleven refusals inside one test run empty the bucket if they
    // are charged, and the legitimate check-in that follows 429s instead of
    // landing. The blocked-account check already sits above the limiter for this
    // same reason.
    let app = test_app().await;
    let owner = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;
    let participant = sign_in_consent_callsign(&app, "maria@example.com", "w2bcd").await;
    let definition_id = create_net(&app, &owner).await;
    let session_id = start_session(&app, &owner, &definition_id).await;

    for n in 0..11 {
        let (status, _) = send_json(
            app.router(),
            "POST",
            &format!("/api/net-sessions/{session_id}/check-ins"),
            Some(json!({ "callsign": "w2bcd", "relayedBy": "W3REL" })),
            Some(&participant),
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN, "refusal {n}");
    }

    let (status, _) = send_json(
        app.router(),
        "POST",
        &format!("/api/net-sessions/{session_id}/check-ins"),
        Some(json!({ "callsign": "w2bcd" })),
        Some(&participant),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::CREATED,
        "eleven refusals must leave the whole burst intact"
    );
}

#[tokio::test]
async fn a_staff_operator_may_record_a_relaying_station_on_the_same_endpoint() {
    // The other half, so the refusal above cannot be satisfied by the field
    // simply not working: the SAME POST, from an operator holding `LogCheckIn`,
    // records it. No new capability is minted; this rides the existing gate.
    let app = test_app().await;
    let owner = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;
    let definition_id = create_net(&app, &owner).await;
    let session_id = start_session(&app, &owner, &definition_id).await;

    let (status, body) = send_json(
        app.router(),
        "POST",
        &format!("/api/net-sessions/{session_id}/check-ins"),
        Some(json!({ "callsign": "k9xyz", "relayedBy": "w3rel" })),
        Some(&owner),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    let roster = body["roster"].as_array().expect("roster");
    assert_eq!(roster[0]["callsign"], "K9XYZ");
    assert_eq!(
        roster[0]["relayedBy"], "W3REL",
        "recorded AT ADD, which is why it joins both owner frames and not the edit-only three"
    );
}

#[tokio::test]
async fn the_public_roster_carries_no_relaying_station() {
    // The decision most likely to be got wrong by analogy to
    // `via`, and the ONLY one here that is unrecoverable if wrong: a published
    // third-party callsign cannot be recalled.
    //
    // Asserted at the WIRE BYTES of the public view, not at the type. The key
    // AND the callsign's own bytes, because projecting it under another name
    // leaks it just as completely.
    let app = test_app().await;
    let owner = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;
    let definition_id = create_net(&app, &owner).await;
    let session_id = start_session(&app, &owner, &definition_id).await;

    let (status, _) = send_json(
        app.router(),
        "POST",
        &format!("/api/net-sessions/{session_id}/check-ins"),
        Some(json!({ "callsign": "k9xyz", "relayedBy": "K7ZZQ" })),
        Some(&owner),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);

    let (status, public) = send_json(
        app.router(),
        "GET",
        &format!("/api/net-sessions/{session_id}/live"),
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let raw = serde_json::to_string(&public).expect("serializes");
    assert!(
        raw.contains("K9XYZ"),
        "the fixture genuinely publishes a roster, so the next two assertions are not vacuous"
    );
    assert!(!raw.contains("relayedBy"), "{raw}");
    assert!(
        !raw.contains("K7ZZQ"),
        "a third-party station that never checked in is not on this page under any key: {raw}"
    );

    // The owner view of the SAME session does carry it — so the refusal above is
    // a redaction and not the field failing to work.
    let (status, owned) = send_json(
        app.router(),
        "GET",
        &format!("/api/net-sessions/{session_id}"),
        None,
        Some(&owner),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(owned["roster"][0]["relayedBy"], "K7ZZQ");
}
