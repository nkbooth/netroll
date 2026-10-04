// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
//! Full-loop API tests for the session snapshot: it carries the definition's
//! connection set BY VALUE, there is no session-level operating frequency, and
//! a log recorded before either was true is refused readably.
//!
//! The stored snapshot is read back from Postgres, never from a response body.

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

async fn event_count(app: &TestApp, kind: &str) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM session_events WHERE kind = $1")
        .bind(kind)
        .fetch_one(&app.pool)
        .await
        .expect("count events")
}

/// Replaces a definition's connection list with three connections of three
/// DIFFERENT kinds — RF first, then two internet-only ways — and returns the
/// definition's own connection ids in position order.
///
/// Three kinds rather than three of one: a snapshot writer that carries only
/// the fields the first connection happens to have passes a same-kind fixture
/// — a fixture that cannot express the case it is named for.
async fn set_three_connections(app: &TestApp, cookie: &str, definition_id: &str) -> Vec<String> {
    let (status, body) = send_json(
        app.router(),
        "PUT",
        &format!("/api/net-definitions/{definition_id}/connections"),
        Some(json!({
            "expectedDefinitionVersion": definition_version(app, definition_id).await,
            "connections": [
                { "kind": "hf", "plannedFrequencyHz": 14_230_000, "band": "20m", "mode": "ssb" },
                { "kind": "echolink", "node": "12345" },
                { "kind": "dmr", "talkgroup": "31337", "network": "Brandmeister" }
            ]
        })),
        Some(cookie),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "connection replace: {body}");
    body["connections"]
        .as_array()
        .expect("connections array")
        .iter()
        .map(|c| c["id"].as_str().expect("connection id").to_owned())
        .collect()
}

async fn definition_version(app: &TestApp, definition_id: &str) -> i32 {
    sqlx::query_scalar("SELECT definition_version FROM net_definitions WHERE id = $1")
        .bind(uuid::Uuid::parse_str(definition_id).expect("uuid"))
        .fetch_one(&app.pool)
        .await
        .expect("definition version")
}

/// Appends a SELF check-in that RECORDS which way in it came on.
///
/// Not over HTTP: an owner holds `LogCheckIn`, so the HTTP handler records their
/// add as STAFF, and the self-history page reads only `self` entries. The point
/// of these tests is the history row's band/mode, not the add path.
async fn self_check_in_via(
    app: &TestApp,
    session_id: &str,
    email: &str,
    callsign: &str,
    via: Option<netroll_domain::net::connection::Via>,
) {
    use netroll_adapters::pg::net_sessions::AddCheckInOutcome;
    use netroll_domain::callsign::parse_callsign;
    use netroll_domain::check_in::{CheckInSource, StayingStatus};

    let account: uuid::Uuid = sqlx::query_scalar("SELECT id FROM accounts WHERE email = $1")
        .bind(email)
        .fetch_one(&app.pool)
        .await
        .expect("account id");
    let outcome = app
        .state
        .net_sessions
        .add_check_in(
            uuid::Uuid::parse_str(session_id).expect("uuid"),
            &parse_callsign(callsign).expect("valid callsign"),
            uuid::Uuid::now_v7(),
            None,
            None,
            StayingStatus::InAndOut,
            None,
            None,
            None,
            CheckInSource::SelfService,
            via.as_ref(),
            None,
            Some(account),
            app.state.clock.now_epoch_millis(),
        )
        .await
        .expect("self check-in");
    assert!(matches!(outcome, AddCheckInOutcome::Added(_)));
}

/// The same, for a check-in that records NO way in.
async fn self_check_in(app: &TestApp, session_id: &str, email: &str, callsign: &str) {
    use netroll_adapters::pg::net_sessions::AddCheckInOutcome;
    use netroll_domain::callsign::parse_callsign;
    use netroll_domain::check_in::{CheckInSource, StayingStatus};

    let account: uuid::Uuid = sqlx::query_scalar("SELECT id FROM accounts WHERE email = $1")
        .bind(email)
        .fetch_one(&app.pool)
        .await
        .expect("account id");
    let outcome = app
        .state
        .net_sessions
        .add_check_in(
            uuid::Uuid::parse_str(session_id).expect("uuid"),
            &parse_callsign(callsign).expect("valid callsign"),
            uuid::Uuid::now_v7(),
            None,
            None,
            StayingStatus::InAndOut,
            None,
            None,
            None,
            CheckInSource::SelfService,
            None,
            None,
            Some(account),
            now_millis(),
        )
        .await
        .expect("add check-in");
    assert!(
        matches!(outcome, AddCheckInOutcome::Added(_)),
        "{outcome:?}"
    );
}

fn now_millis() -> u64 {
    1_700_000_000_000
}

/// The session's snapshot AS STORED — read straight out of Postgres, so no
/// response body can stand in for it.
async fn stored_snapshot(app: &TestApp, session_id: &str) -> Value {
    sqlx::query_scalar("SELECT definition_snapshot FROM net_sessions WHERE id = $1")
        .bind(uuid::Uuid::parse_str(session_id).expect("uuid"))
        .fetch_one(&app.pool)
        .await
        .expect("stored snapshot")
}

/// Starts a session on `definition_id`. No frequency is sent —
/// an internet-only net has none to give.
async fn start_session(app: &TestApp, cookie: &str, definition_id: &str) -> String {
    let (status, body) = send_json(
        app.router(),
        "POST",
        "/api/net-sessions",
        Some(json!({ "definitionId": definition_id })),
        Some(cookie),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "start session: {body}");
    body["id"].as_str().expect("session id").to_owned()
}

// --- The snapshot carries the set, by value ----------

#[tokio::test]
async fn the_stored_snapshot_carries_every_connection_in_position_order_with_the_definitions_own_ids()
 {
    // EXPECTED RED before the implementation: at the
    // baseline the stored snapshot has no `connections` key at all.
    let app = test_app().await;
    let cookie = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;
    let (definition_id, _) = create_net(&app, &cookie).await;
    let definition_ids = set_three_connections(&app, &cookie, &definition_id).await;
    let session_id = start_session(&app, &cookie, &definition_id).await;

    let snapshot = stored_snapshot(&app, &session_id).await;
    let carried = snapshot["connections"]
        .as_array()
        .expect("the stored snapshot carries a connections array")
        .clone();

    assert_eq!(carried.len(), 3, "all three connections are carried");
    for (index, connection) in carried.iter().enumerate() {
        assert_eq!(
            connection["position"], index as i64,
            "position order survives the copy"
        );
        // The DEFINITION's uuid, not a freshly minted one — `via` is keyed on
        // exactly this value.
        assert_eq!(
            connection["id"].as_str().expect("id"),
            definition_ids[index],
            "connection {index} keeps the definition's own id"
        );
    }
    assert_eq!(carried[0]["kind"], "hf");
    assert_eq!(carried[1]["kind"], "echolink");
    assert_eq!(carried[2]["kind"], "dmr");
    // Band/mode/frequency MOVED into connections[0]; they are no longer
    // top-level snapshot fields.
    assert_eq!(carried[0]["band"], "20m");
    assert_eq!(carried[0]["mode"], "ssb");
    assert_eq!(carried[0]["plannedFrequencyHz"], 14_230_000);
    assert!(
        snapshot.get("band").is_none(),
        "the top-level band moved into connections[0]"
    );
    assert!(
        snapshot.get("mode").is_none(),
        "the top-level mode moved into connections[0]"
    );
    assert!(
        snapshot.get("plannedFrequencyHz").is_none(),
        "the top-level planned frequency moved into connections[0]"
    );
}

#[tokio::test]
async fn editing_the_definitions_connections_does_not_change_a_running_sessions_snapshot() {
    // The session COPIES, it does not reference. EXPECTED RED before the
    // implementation (there is nothing carried to be stale).
    let app = test_app().await;
    let cookie = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;
    let (definition_id, _) = create_net(&app, &cookie).await;
    let before = set_three_connections(&app, &cookie, &definition_id).await;
    let session_id = start_session(&app, &cookie, &definition_id).await;

    let (status, _) = send_json(
        app.router(),
        "PUT",
        &format!("/api/net-definitions/{definition_id}/connections"),
        Some(json!({
            "expectedDefinitionVersion": definition_version(&app, &definition_id).await,
            "connections": [{ "kind": "allstar", "node": "55555" }]
        })),
        Some(&cookie),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let snapshot = stored_snapshot(&app, &session_id).await;
    let carried = snapshot["connections"].as_array().expect("connections");
    assert_eq!(carried.len(), 3, "the running session keeps its own copy");
    assert_eq!(carried[0]["id"].as_str().expect("id"), before[0]);
    assert_eq!(carried[0]["kind"], "hf");
}

// --- The session-level frequency is gone ------------------------

#[tokio::test]
async fn the_session_level_operating_frequency_column_no_longer_exists() {
    // EXPECTED RED before the migration.
    let app = test_app().await;
    let count: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM information_schema.columns
          WHERE table_name = 'net_sessions' AND column_name = 'operating_frequency_hz'",
    )
    .fetch_one(&app.pool)
    .await
    .expect("column census");
    assert_eq!(count, 0, "net_sessions.operating_frequency_hz is dropped");
}

#[tokio::test]
async fn starting_an_internet_only_net_needs_no_frequency() {
    // EXPECTED RED before the implementation: the start body requires
    // `operatingFrequency`, so an internet-only net cannot start at all.
    let app = test_app().await;
    let cookie = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;
    let (definition_id, _) = create_net(&app, &cookie).await;
    let (status, body) = send_json(
        app.router(),
        "PUT",
        &format!("/api/net-definitions/{definition_id}/connections"),
        Some(json!({
            "expectedDefinitionVersion": definition_version(&app, &definition_id).await,
            "connections": [{ "kind": "echolink", "node": "12345" }]
        })),
        Some(&cookie),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");

    let (status, body) = send_json(
        app.router(),
        "POST",
        "/api/net-sessions",
        Some(json!({ "definitionId": definition_id })),
        Some(&cookie),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    assert!(
        body.get("operatingFrequencyHz").is_none(),
        "the session-level frequency is retired from the summary too"
    );
    assert_eq!(body["connections"][0]["kind"], "echolink");
}

// --- The forward frequency event addresses a connection --------------

#[tokio::test]
async fn a_frequency_change_names_the_connection_it_changes() {
    // EXPECTED RED: today the request carries no connection id and the
    // stored payload carries none either.
    let app = test_app().await;
    let cookie = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;
    let (definition_id, _) = create_net(&app, &cookie).await;
    let ids = set_three_connections(&app, &cookie, &definition_id).await;
    let session_id = start_session(&app, &cookie, &definition_id).await;

    let (status, body) = send_json(
        app.router(),
        "POST",
        &format!("/api/net-sessions/{session_id}/frequency"),
        Some(json!({ "connectionId": ids[0], "operatingFrequency": "7.200" })),
        Some(&cookie),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");

    let payload: Value = sqlx::query_scalar(
        "SELECT payload FROM session_events WHERE session_id = $1 AND kind = 'frequency.changed'",
    )
    .bind(uuid::Uuid::parse_str(&session_id).expect("uuid"))
    .fetch_one(&app.pool)
    .await
    .expect("stored payload");
    assert_eq!(payload["connectionId"].as_str().expect("id"), ids[0]);
    assert_eq!(payload["operatingFrequencyHz"], 7_200_000);

    // The folded summary moves the frequency of THAT connection and no other.
    assert_eq!(body["connections"][0]["plannedFrequencyHz"], 7_200_000);
    assert_eq!(body["connections"][1]["kind"], "echolink");
    assert!(body["connections"][1]["plannedFrequencyHz"].is_null());
}

#[tokio::test]
async fn a_frequency_change_naming_a_connection_this_session_does_not_have_is_refused() {
    // The guard the fold no longer has to make: a frequency written
    // against an id no snapshot connection carries would be invisible on every
    // surface. EXPECTED RED (the endpoint takes no connection id today).
    let app = test_app().await;
    let cookie = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;
    let (definition_id, _) = create_net(&app, &cookie).await;
    set_three_connections(&app, &cookie, &definition_id).await;
    let session_id = start_session(&app, &cookie, &definition_id).await;

    let (status, body) = send_json(
        app.router(),
        "POST",
        &format!("/api/net-sessions/{session_id}/frequency"),
        Some(json!({
            "connectionId": "00000000-0000-0000-0000-0000000000ff",
            "operatingFrequency": "7.200"
        })),
        Some(&cookie),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
    assert_eq!(body["type"], "/errors/net-connection-not-found");
    assert_eq!(
        event_count(&app, "frequency.changed").await,
        0,
        "no phantom event is appended"
    );
}

#[tokio::test]
async fn a_frequency_change_naming_a_way_in_that_has_no_frequency_is_refused() {
    // The membership guard checked that the session
    // FROZE this connection, and nothing checked that the connection is a thing
    // a frequency can belong to — so a direct POST naming the EchoLink way in
    // was accepted and appended a PERMANENT `frequency.changed`. `live_connections`
    // then stamped a frequency onto an `echolink` wire that every renderer hides,
    // making it invisible everywhere but the event log. That is exactly what the
    // membership guard's own doc says it exists to prevent — "a frequency no
    // surface could render" — one kind-check short. The only guard was the
    // browser's, and the browser is not the authority.
    let app = test_app().await;
    let cookie = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;
    let (definition_id, _) = create_net(&app, &cookie).await;
    let ids = set_three_connections(&app, &cookie, &definition_id).await;
    let session_id = start_session(&app, &cookie, &definition_id).await;

    for internet_way in [&ids[1], &ids[2]] {
        let (status, body) = send_json(
            app.router(),
            "POST",
            &format!("/api/net-sessions/{session_id}/frequency"),
            Some(json!({
                "connectionId": internet_way,
                "operatingFrequency": "7.200"
            })),
            Some(&cookie),
        )
        .await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
        assert_eq!(body["type"], "/errors/connection-has-no-frequency");
        assert!(
            body["detail"].as_str().is_some_and(|d| !d.is_empty()),
            "the refusal names the fault: {body}"
        );
    }

    assert_eq!(
        event_count(&app, "frequency.changed").await,
        0,
        "no permanent event is appended for a way in that cannot carry one"
    );

    // The RF way in still moves — the guard narrows, it does not close the door.
    let (status, body) = send_json(
        app.router(),
        "POST",
        &format!("/api/net-sessions/{session_id}/frequency"),
        Some(json!({ "connectionId": &ids[0], "operatingFrequency": "7.200" })),
        Some(&cookie),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["connections"][0]["plannedFrequencyHz"], 7_200_000);
}

// --- The refusal, and it is COMPREHENSIBLE ---------------------

/// A `definition_snapshot` of the older shape: flat `band`/`mode`/
/// `plannedFrequencyHz` and **no `connections` key**. `connections` is
/// required, so this decodes to nothing — it is not translated and it is not
/// defaulted to an empty set.
fn snapshot_without_a_connection_set() -> Value {
    json!({
        "title": "Sunday Traffic Net",
        "plannedFrequencyHz": 14_230_000_i64,
        "band": "20m",
        "mode": "ssb",
        "netCategory": "traffic",
        "netType": "open"
    })
}

async fn overwrite_snapshot_without_a_connection_set(app: &TestApp, session_id: &str) {
    sqlx::query("UPDATE net_sessions SET definition_snapshot = $2 WHERE id = $1")
        .bind(uuid::Uuid::parse_str(session_id).expect("uuid"))
        .bind(snapshot_without_a_connection_set())
        .execute(&app.pool)
        .await
        .expect("plant a snapshot with no connection set");
}

/// Asserts the ONE refusal contract, on whatever surface handed
/// back `status`/`body`.
fn assert_refusal(surface: &str, status: StatusCode, body: &Value) {
    assert_ne!(
        status,
        StatusCode::OK,
        "{surface}: a 200 with an empty connection set is the forbidden outcome"
    );
    assert_ne!(
        status,
        StatusCode::INTERNAL_SERVER_ERROR,
        "{surface}: the refusal is not a bare 500"
    );
    assert_eq!(status, StatusCode::GONE, "{surface}");
    assert_eq!(body["type"], "/errors/unreplayable-log", "{surface}");
    assert_eq!(
        body["__contentType"], "application/problem+json",
        "{surface}"
    );
    let detail = body["detail"]
        .as_str()
        .unwrap_or_else(|| panic!("{surface}: the refusal names the fault in `detail`"));
    assert!(
        !detail.is_empty(),
        "{surface}: `detail` is populated, not None"
    );
}

#[tokio::test]
async fn a_log_recorded_before_a_net_could_have_more_than_one_connection_is_refused_on_every_read_surface()
 {
    // EXPECTED RED: at the baseline this is a 500 `/errors/internal`
    // with `detail: null` on the owner view, and the export/history surfaces
    // answer the same way.
    let app = test_app().await;
    let cookie = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;
    let (definition_id, _) = create_net(&app, &cookie).await;
    set_three_connections(&app, &cookie, &definition_id).await;
    let session_id = start_session(&app, &cookie, &definition_id).await;
    overwrite_snapshot_without_a_connection_set(&app, &session_id).await;

    for (surface, uri) in [
        ("owner HTTP view", format!("/api/net-sessions/{session_id}")),
        (
            "public HTTP view",
            format!("/api/net-sessions/{session_id}/live"),
        ),
        (
            "CSV export",
            format!("/api/net-sessions/{session_id}/export?format=csv"),
        ),
        (
            "ADIF export",
            format!("/api/net-sessions/{session_id}/export?format=adif"),
        ),
    ] {
        let (status, body) = send_json(app.router(), "GET", &uri, None, Some(&cookie)).await;
        assert_refusal(surface, status, &body);
    }
}

#[tokio::test]
async fn a_snapshot_that_is_merely_corrupt_is_not_reported_as_permanent_historical_loss() {
    // `UnreplayableLog`'s own doc says it marks exactly two record shapes, both
    // of the older kind, but EVERY snapshot
    // decode failure was mapped to it — and `/errors/unreplayable-log`'s copy
    // tells the operator "Nothing you do will bring it back." So genuine
    // corruption, a future writer regression, and a snapshot written by a NEWER
    // deploy during a rollback all reached the operator as permanent historical
    // loss. That is a different confident wrong answer, and the second one is
    // recoverable by rolling forward again.
    //
    // The distinguishing fact is the one the doc already names: an older row
    // has NO `connections` key. A row that HAS one and cannot be read is
    // something else, and answers as something else.
    let app = test_app().await;
    let cookie = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;
    let (definition_id, _) = create_net(&app, &cookie).await;
    set_three_connections(&app, &cookie, &definition_id).await;
    let session_id = start_session(&app, &cookie, &definition_id).await;

    sqlx::query(
        "UPDATE net_sessions
            SET definition_snapshot =
                  jsonb_set(definition_snapshot, '{connections}', '\"not a list\"'::jsonb)
          WHERE id = $1",
    )
    .bind(uuid::Uuid::parse_str(&session_id).expect("uuid"))
    .execute(&app.pool)
    .await
    .expect("corrupt the snapshot");

    let (status, body) = send_json(
        app.router(),
        "GET",
        &format!("/api/net-sessions/{session_id}"),
        None,
        Some(&cookie),
    )
    .await;
    assert_ne!(
        body["type"], "/errors/unreplayable-log",
        "a corrupt row is not a historical row, and must not be called permanent: {body}"
    );
    assert_ne!(status, StatusCode::GONE, "{body}");
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR, "{body}");
}

#[tokio::test]
async fn check_in_history_drops_an_unreadable_session_and_not_the_whole_page() {
    // The refusal is aimed at the ROW rather than the page.
    //
    // The refusal used to `return` from inside the row loop, so ONE check-in on
    // an older session killed the entire keyset page — and, because the walk
    // is keyset, everything older than it as well. That is the whole of a
    // reader's own history gone over one old net. The named 410 still answers on
    // that session's own surfaces; this widget is a LIST of many sessions, and
    // one unreadable member of it drops itself.
    //
    // The empty array is asserted alongside the missing key because the previous
    // guard tested `is_none()` and therefore let `"connections": []` through —
    // reporting a band-less net, the confident wrong answer its own comment
    // named, on a shape `connection_set_from_wire` explicitly refuses.
    let app = test_app().await;
    let cookie = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;

    let (missing_key, _) = create_net(&app, &cookie).await;
    set_three_connections(&app, &cookie, &missing_key).await;
    let missing_key_session = start_session(&app, &cookie, &missing_key).await;
    self_check_in(&app, &missing_key_session, "owner@example.com", "w1aw").await;
    overwrite_snapshot_without_a_connection_set(&app, &missing_key_session).await;

    let (empty_array, _) = create_net(&app, &cookie).await;
    set_three_connections(&app, &cookie, &empty_array).await;
    let empty_array_session = start_session(&app, &cookie, &empty_array).await;
    self_check_in(&app, &empty_array_session, "owner@example.com", "w1aw").await;
    sqlx::query(
        "UPDATE net_sessions
            SET definition_snapshot = jsonb_set(definition_snapshot, '{connections}', '[]'::jsonb)
          WHERE id = $1",
    )
    .bind(uuid::Uuid::parse_str(&empty_array_session).expect("uuid"))
    .execute(&app.pool)
    .await
    .expect("plant an empty connection list");

    // A READABLE check-in, without which "the page answers" would be satisfied
    // by a page that answers with nothing at all.
    let (readable, _) = create_net(&app, &cookie).await;
    set_three_connections(&app, &cookie, &readable).await;
    let readable_session = start_session(&app, &cookie, &readable).await;
    self_check_in(&app, &readable_session, "owner@example.com", "w1aw").await;

    let (status, body) = send_json(
        app.router(),
        "GET",
        "/api/accounts/me/check-ins",
        None,
        Some(&cookie),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "check-in history: {body}");
    let sessions: Vec<&str> = body["items"]
        .as_array()
        .expect("items array")
        .iter()
        .filter_map(|i| i["netSessionId"].as_str())
        .collect();
    assert!(
        sessions.contains(&readable_session.as_str()),
        "the readable check-in survives its neighbours: {body}"
    );
    assert!(
        !sessions.contains(&missing_key_session.as_str()),
        "a snapshot with no `connections` key drops its own row: {body}"
    );
    assert!(
        !sessions.contains(&empty_array_session.as_str()),
        "an EMPTY connection list is refused too, not reported as a band-less net: {body}"
    );
}

#[tokio::test]
async fn a_historical_frequency_changed_payload_is_refused_rather_than_translated() {
    // A hand-constructed older event sequence — a `session.started`
    // and a `frequency.changed` in the OLD payload shape — presented to the
    // decoder. EXPECTED RED: today both decode happily and the fold attributes
    // them to a session-level frequency.
    let app = test_app().await;
    let cookie = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;
    let (definition_id, _) = create_net(&app, &cookie).await;
    set_three_connections(&app, &cookie, &definition_id).await;
    let session_id = start_session(&app, &cookie, &definition_id).await;
    let session_uuid = uuid::Uuid::parse_str(&session_id).expect("uuid");

    // The older `session.started`: a session-level operating frequency.
    sqlx::query(
        "UPDATE session_events SET payload = $2 WHERE session_id = $1 AND kind = 'session.started'",
    )
    .bind(session_uuid)
    .bind(json!({
        "definitionId": definition_id,
        "definitionVersion": 2,
        "operatingFrequencyHz": 14_250_000_i64
    }))
    .execute(&app.pool)
    .await
    .expect("plant an older session.started");

    let (status, body) = send_json(
        app.router(),
        "GET",
        &format!("/api/net-sessions/{session_id}"),
        None,
        Some(&cookie),
    )
    .await;
    assert_refusal("historical session.started", status, &body);

    // Restore the modern `session.started` so the next assertion is genuinely
    // about the OTHER historical payload and not still about this one.
    sqlx::query(
        "UPDATE session_events SET payload = $2 WHERE session_id = $1 AND kind = 'session.started'",
    )
    .bind(session_uuid)
    .bind(json!({ "definitionId": definition_id, "definitionVersion": 2 }))
    .execute(&app.pool)
    .await
    .expect("restore the modern session.started");
    let (status, body) = send_json(
        app.router(),
        "GET",
        &format!("/api/net-sessions/{session_id}"),
        None,
        Some(&cookie),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "the restore worked: {body}");

    // The older `frequency.changed`: a frequency attached to the SESSION,
    // naming no connection. There is no connection to attribute it to.
    sqlx::query(
        "INSERT INTO session_events (id, session_id, seq, kind, payload, actor, created_at)
         VALUES ($1, $2, 99, 'frequency.changed', $3, NULL, now())",
    )
    .bind(uuid::Uuid::now_v7())
    .bind(session_uuid)
    .bind(json!({ "operatingFrequencyHz": 7_200_000_i64 }))
    .execute(&app.pool)
    .await
    .expect("plant an older frequency.changed");

    let (status, body) = send_json(
        app.router(),
        "GET",
        &format!("/api/net-sessions/{session_id}/live"),
        None,
        Some(&cookie),
    )
    .await;
    assert_refusal("historical frequency.changed", status, &body);
}

// --- The tenth surface, where the failure is at RUNTIME -------------

#[tokio::test]
async fn check_in_history_sources_band_and_mode_from_the_snapshots_connection_set() {
    // `self_check_ins_page` asserted `AS "band!"` against a top-level
    // snapshot key; the moment band moves into the connection SET that query
    // answers wrongly (or fails) at RUNTIME, not at compile time.
    //
    // The band comes from the connection SET and never
    // from a top-level key — that half is what this test exists for and is
    // unchanged. What changed is WHICH connection: it is now the one THIS
    // check-in came in on, not the net's first, because on this very fixture —
    // three connections, HF then EchoLink then DMR — "the net's first" told an
    // EchoLink participant they had been on 20m. So the check-in records the way
    // in it used, and the widget reports that connection's band.
    let app = test_app().await;
    let cookie = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;
    let (definition_id, _) = create_net(&app, &cookie).await;
    set_three_connections(&app, &cookie, &definition_id).await;
    let session_id = start_session(&app, &cookie, &definition_id).await;

    let (status, summary) = send_json(
        app.router(),
        "GET",
        &format!("/api/net-sessions/{session_id}"),
        None,
        Some(&cookie),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{summary}");
    let hf = summary["connections"][0]["id"]
        .as_str()
        .expect("the HF connection's id");

    // Through the REPO, not the HTTP add: this read returns only `source =
    // self` rows, and the owner holds `LogCheckIn`, so an HTTP add from this
    // cookie takes the staff path and never reaches the widget at all.
    self_check_in_via(
        &app,
        &session_id,
        "owner@example.com",
        "w1aw",
        Some(netroll_domain::net::connection::Via::Connection(
            uuid::Uuid::parse_str(hf).expect("uuid"),
        )),
    )
    .await;

    let (status, body) = send_json(
        app.router(),
        "GET",
        "/api/accounts/me/check-ins",
        None,
        Some(&cookie),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["items"][0]["band"], "20m");
    assert_eq!(body["items"][0]["mode"], "ssb");
    assert_eq!(body["items"][0]["via"], "HF — 14.230 MHz");
}

#[tokio::test]
async fn check_in_history_reports_no_band_for_an_internet_only_net() {
    // `AS "band!"` asserted NOT NULL, which an internet-only
    // net cannot honour. EXPECTED RED — today the flat snapshot always carries a
    // band, so this case was unrepresentable.
    let app = test_app().await;
    let cookie = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;
    let (definition_id, _) = create_net(&app, &cookie).await;
    let (status, _) = send_json(
        app.router(),
        "PUT",
        &format!("/api/net-definitions/{definition_id}/connections"),
        Some(json!({
            "expectedDefinitionVersion": definition_version(&app, &definition_id).await,
            "connections": [{ "kind": "echolink", "node": "12345" }]
        })),
        Some(&cookie),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let session_id = start_session(&app, &cookie, &definition_id).await;
    self_check_in(&app, &session_id, "owner@example.com", "w1aw").await;

    let (status, body) = send_json(
        app.router(),
        "GET",
        "/api/accounts/me/check-ins",
        None,
        Some(&cookie),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(
        body["items"][0]["band"].is_null(),
        "an internet-only net has no band, and the row says so rather than failing"
    );
    assert!(body["items"][0]["mode"].is_null());
}
