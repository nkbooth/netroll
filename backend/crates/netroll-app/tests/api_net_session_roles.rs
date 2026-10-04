// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
//! Full-loop API tests for per-net roles and object-level authorization: real
//! router, real Postgres. Asserts HTTP status codes, problem+json slugs,
//! response-body values and DB row state — never message prose. Covers the
//! grant/revoke authz matrix, the widened-mutation matrix, cross-net isolation,
//! 404-before-403, and the owner regression.

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

/// Signs in a fresh email, records consent, and reserves a callsign — the full
/// gate an operator needs to be an owner or a role target. Returns the session
/// cookie.
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

/// Signs in a consented account with NO callsign — a bare Participant (an
/// authenticated, ungranted, non-owner account).
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

async fn account_id(app: &TestApp, cookie: &str) -> String {
    let (status, body) =
        send_json(app.router(), "GET", "/api/accounts/me", None, Some(cookie)).await;
    assert_eq!(status, StatusCode::OK);
    body["id"].as_str().expect("account id").to_owned()
}

async fn grant(
    app: &TestApp,
    cookie: &str,
    session_id: &str,
    callsign: &str,
    role: &str,
) -> (StatusCode, Value) {
    send_json(
        app.router(),
        "POST",
        &format!("/api/net-sessions/{session_id}/roles"),
        Some(json!({ "callsign": callsign, "role": role })),
        Some(cookie),
    )
    .await
}

async fn list_roles(app: &TestApp, cookie: Option<&str>, session_id: &str) -> (StatusCode, Value) {
    send_json(
        app.router(),
        "GET",
        &format!("/api/net-sessions/{session_id}/roles"),
        None,
        cookie,
    )
    .await
}

async fn add_check_in(app: &TestApp, cookie: &str, session_id: &str, callsign: &str) -> StatusCode {
    send_json(
        app.router(),
        "POST",
        &format!("/api/net-sessions/{session_id}/check-ins"),
        Some(json!({ "callsign": callsign })),
        Some(cookie),
    )
    .await
    .0
}

/// Attempts a QSY. The write names WHICH connection moved, read off
/// the session's own frozen snapshot so the id is one the server accepts — an
/// authorization test must fail on authorization, not on a bad connection id.
async fn change_frequency(app: &TestApp, cookie: &str, session_id: &str, mhz: &str) -> StatusCode {
    let snapshot: Value =
        sqlx::query_scalar("SELECT definition_snapshot FROM net_sessions WHERE id = $1")
            .bind(uuid::Uuid::parse_str(session_id).expect("uuid"))
            .fetch_one(&app.pool)
            .await
            .expect("stored snapshot");
    let connection_id = snapshot["connections"][0]["id"]
        .as_str()
        .expect("the snapshot carries its connection ids");
    send_json(
        app.router(),
        "POST",
        &format!("/api/net-sessions/{session_id}/frequency"),
        Some(json!({ "connectionId": connection_id, "operatingFrequency": mhz })),
        Some(cookie),
    )
    .await
    .0
}

async fn role_row_count(app: &TestApp, session_id: &str, account: &str) -> i64 {
    sqlx::query_scalar(
        "SELECT count(*) FROM net_session_roles WHERE net_session_id = $1 AND account_id = $2",
    )
    .bind(uuid::Uuid::parse_str(session_id).unwrap())
    .bind(uuid::Uuid::parse_str(account).unwrap())
    .fetch_one(&app.pool)
    .await
    .expect("count role rows")
}

// --- Grant/revoke authz matrix -----------------------------------

#[tokio::test]
async fn an_owner_grants_a_logger_role_and_gets_the_granted_body() {
    let app = test_app().await;
    let owner = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;
    let staff = sign_in_consent_callsign(&app, "staff@example.com", "w2bcd").await;
    let staff_id = account_id(&app, &staff).await;
    let definition_id = create_net(&app, &owner).await;
    let session_id = start_session(&app, &owner, &definition_id).await;

    let (status, body) = grant(&app, &owner, &session_id, "w2bcd", "logger").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["accountId"], staff_id);
    assert_eq!(body["role"], "logger");
    assert_eq!(
        role_row_count(&app, &session_id, &staff_id).await,
        1,
        "the grant persisted exactly one row"
    );
}

#[tokio::test]
async fn an_ncs_may_grant_a_logger_but_not_a_peer_or_owner() {
    let app = test_app().await;
    let owner = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;
    let ncs = sign_in_consent_callsign(&app, "ncs@example.com", "w2bcd").await;
    let _logger = sign_in_consent_callsign(&app, "logger@example.com", "w3efg").await;
    let definition_id = create_net(&app, &owner).await;
    let session_id = start_session(&app, &owner, &definition_id).await;

    // Owner promotes w2bcd to NCS.
    assert_eq!(
        grant(&app, &owner, &session_id, "w2bcd", "net-control")
            .await
            .0,
        StatusCode::OK
    );

    // NCS may grant a strictly-lower role.
    assert_eq!(
        grant(&app, &ncs, &session_id, "w3efg", "logger").await.0,
        StatusCode::OK
    );

    // NCS may NOT grant a peer (net-control) or the apex (owner) — the ceiling.
    let (peer_status, peer_body) = grant(&app, &ncs, &session_id, "w3efg", "net-control").await;
    assert_eq!(peer_status, StatusCode::FORBIDDEN);
    assert_eq!(peer_body["type"], "/errors/forbidden");
    let (owner_status, owner_body) = grant(&app, &ncs, &session_id, "w3efg", "owner").await;
    assert_eq!(owner_status, StatusCode::FORBIDDEN);
    assert_eq!(owner_body["type"], "/errors/forbidden");
}

#[tokio::test]
async fn an_owner_may_never_be_granted_through_the_session_surface() {
    // Even the owner themselves cannot grant `owner` — no role outranks Owner.
    let app = test_app().await;
    let owner = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;
    let _staff = sign_in_consent_callsign(&app, "staff@example.com", "w2bcd").await;
    let definition_id = create_net(&app, &owner).await;
    let session_id = start_session(&app, &owner, &definition_id).await;

    let (status, body) = grant(&app, &owner, &session_id, "w2bcd", "owner").await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(body["type"], "/errors/forbidden");
}

#[tokio::test]
async fn a_logger_and_a_participant_cannot_grant_roles() {
    let app = test_app().await;
    let owner = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;
    let logger = sign_in_consent_callsign(&app, "logger@example.com", "w2bcd").await;
    let participant = sign_in_consent(&app, "participant@example.com").await;
    let _target = sign_in_consent_callsign(&app, "target@example.com", "w3efg").await;
    let definition_id = create_net(&app, &owner).await;
    let session_id = start_session(&app, &owner, &definition_id).await;
    assert_eq!(
        grant(&app, &owner, &session_id, "w2bcd", "logger").await.0,
        StatusCode::OK
    );

    // A Logger holds no ManageRoles capability.
    assert_eq!(
        grant(&app, &logger, &session_id, "w3efg", "relay").await.0,
        StatusCode::FORBIDDEN
    );
    // A bare Participant (no grant) likewise.
    assert_eq!(
        grant(&app, &participant, &session_id, "w3efg", "relay")
            .await
            .0,
        StatusCode::FORBIDDEN
    );
}

#[tokio::test]
async fn an_unknown_role_string_is_a_400_role_invalid() {
    let app = test_app().await;
    let owner = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;
    let _staff = sign_in_consent_callsign(&app, "staff@example.com", "w2bcd").await;
    let definition_id = create_net(&app, &owner).await;
    let session_id = start_session(&app, &owner, &definition_id).await;

    let (status, body) = grant(&app, &owner, &session_id, "w2bcd", "supreme-leader").await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["type"], "/errors/role-invalid");
}

#[tokio::test]
async fn granting_to_an_unknown_callsign_is_a_404() {
    let app = test_app().await;
    let owner = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;
    let definition_id = create_net(&app, &owner).await;
    let session_id = start_session(&app, &owner, &definition_id).await;

    let (status, _) = grant(&app, &owner, &session_id, "w9zzz", "logger").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn revoking_a_grant_removes_it_and_a_missing_grant_is_404() {
    let app = test_app().await;
    let owner = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;
    let staff = sign_in_consent_callsign(&app, "staff@example.com", "w2bcd").await;
    let staff_id = account_id(&app, &staff).await;
    let definition_id = create_net(&app, &owner).await;
    let session_id = start_session(&app, &owner, &definition_id).await;
    assert_eq!(
        grant(&app, &owner, &session_id, "w2bcd", "logger").await.0,
        StatusCode::OK
    );

    let (status, _) = send_json(
        app.router(),
        "DELETE",
        &format!("/api/net-sessions/{session_id}/roles/{staff_id}"),
        None,
        Some(&owner),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert_eq!(
        role_row_count(&app, &session_id, &staff_id).await,
        0,
        "the grant row was deleted"
    );

    // A second revoke of the now-absent grant is a 404 role-grant-not-found.
    let (status, body) = send_json(
        app.router(),
        "DELETE",
        &format!("/api/net-sessions/{session_id}/roles/{staff_id}"),
        None,
        Some(&owner),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body["type"], "/errors/role-grant-not-found");
}

#[tokio::test]
async fn granting_a_role_to_a_current_owner_is_refused() {
    // Review finding: a grant to a current co-owner would be silently inert
    // (resolve_role checks Owner first) and would persist as a dormant row
    // that reactivates as a live, un-re-vetted staff grant the moment the
    // target is later removed as owner. Refusing at write time closes
    // that creation path.
    let app = test_app().await;
    let owner = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;
    let co_owner = sign_in_consent_callsign(&app, "coowner@example.com", "w2bcd").await;
    let co_owner_id = account_id(&app, &co_owner).await;
    let definition_id = create_net(&app, &owner).await;
    let session_id = start_session(&app, &owner, &definition_id).await;

    let (status, _) = send_json(
        app.router(),
        "POST",
        &format!("/api/net-definitions/{definition_id}/owners"),
        Some(json!({ "callsign": "w2bcd" })),
        Some(&owner),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "co-owner add succeeds");

    let (grant_status, grant_body) = grant(&app, &owner, &session_id, "w2bcd", "net-control").await;
    assert_eq!(grant_status, StatusCode::FORBIDDEN);
    assert_eq!(grant_body["type"], "/errors/forbidden");
    assert_eq!(
        role_row_count(&app, &session_id, &co_owner_id).await,
        0,
        "no dormant role row was created for the current owner"
    );
}

#[tokio::test]
async fn a_session_scoped_grant_survives_a_later_owner_add_and_remove() {
    // Documents the INTENDED fallback
    // behavior when a session-scoped grant, made while the target was NOT an
    // owner, coexists across a later definition-level ownership change. While
    // the target is an owner, resolve_role's Owner-first precedence makes the
    // stored grant inert (Owner outranks it); once removed as owner, the
    // account falls back to its own previously-granted role — NOT
    // Participant — because that grant was made legitimately, before any
    // ownership existed, by an actor with the authority to make it. This is
    // The resolution order exercised end-to-end, not a bug: a granted role
    // and definition ownership are deliberately independent, and
    // `granting_a_role_to_a_current_owner_is_refused` above closes the
    // adjacent path where a NEW grant could be created dormant while the
    // target already holds ownership.
    let app = test_app().await;
    let owner = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;
    let staff = sign_in_consent_callsign(&app, "staff@example.com", "w2bcd").await;
    let staff_id = account_id(&app, &staff).await;
    let definition_id = create_net(&app, &owner).await;
    let session_id = start_session(&app, &owner, &definition_id).await;

    // Grant Logger to `staff` while `staff` is NOT an owner.
    assert_eq!(
        grant(&app, &owner, &session_id, "w2bcd", "logger").await.0,
        StatusCode::OK
    );

    // Now make `staff` a co-owner via the owner endpoint.
    let (add_status, _) = send_json(
        app.router(),
        "POST",
        &format!("/api/net-definitions/{definition_id}/owners"),
        Some(json!({ "callsign": "w2bcd" })),
        Some(&owner),
    )
    .await;
    assert_eq!(add_status, StatusCode::OK);

    // While an owner, staff holds every capability (Owner-first precedence),
    // regardless of the dormant Logger grant underneath.
    assert_eq!(
        change_frequency(&app, &staff, &session_id, "14.330").await,
        StatusCode::OK,
        "as owner, RunSession succeeds even though the stored grant is only Logger"
    );

    // Remove staff as owner again via the owner endpoint.
    let (remove_status, _) = send_json(
        app.router(),
        "DELETE",
        &format!("/api/net-definitions/{definition_id}/owners/{staff_id}"),
        None,
        Some(&owner),
    )
    .await;
    assert_eq!(remove_status, StatusCode::NO_CONTENT);

    // Post-removal: staff resolves back to the previously-granted Logger role
    // (LogCheckIn yes), not Participant, and NOT RunSession (Logger doesn't
    // hold it) — the grant row survived the ownership round-trip untouched.
    assert_eq!(
        add_check_in(&app, &staff, &session_id, "k7abc").await,
        StatusCode::CREATED,
        "staff falls back to its own Logger grant, not Participant"
    );
    assert_eq!(
        change_frequency(&app, &staff, &session_id, "14.340").await,
        StatusCode::FORBIDDEN,
        "Logger does not hold RunSession — confirms staff is no longer Owner"
    );
    assert_eq!(
        role_row_count(&app, &session_id, &staff_id).await,
        1,
        "the original Logger grant row was never touched by the ownership round-trip"
    );
}

#[tokio::test]
async fn a_relay_is_refused_run_session_and_manage_roles() {
    // Relay is the lowest staff tier: it holds ViewConsole/LogCheckIn (Relay
    // threshold) but not RunSession/ManageRoles (NetControl threshold). The
    // domain unit tests already prove this exhaustively over the pure
    // function; this is the end-to-end confirmation that the HTTP wiring
    // matches (review finding: Relay was only ever exercised positively).
    let app = test_app().await;
    let owner = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;
    let relay = sign_in_consent_callsign(&app, "relay@example.com", "w2bcd").await;
    let _target = sign_in_consent_callsign(&app, "target@example.com", "w3efg").await;
    let definition_id = create_net(&app, &owner).await;
    let session_id = start_session(&app, &owner, &definition_id).await;
    assert_eq!(
        grant(&app, &owner, &session_id, "w2bcd", "relay").await.0,
        StatusCode::OK
    );

    assert_eq!(
        change_frequency(&app, &relay, &session_id, "14.300").await,
        StatusCode::FORBIDDEN,
        "Relay does not hold RunSession"
    );
    let (close_status, _) = send_json(
        app.router(),
        "POST",
        &format!("/api/net-sessions/{session_id}/close"),
        None,
        Some(&relay),
    )
    .await;
    assert_eq!(
        close_status,
        StatusCode::FORBIDDEN,
        "Relay does not hold RunSession"
    );
    let (grant_status, grant_body) = grant(&app, &relay, &session_id, "w3efg", "participant").await;
    assert_eq!(
        grant_status,
        StatusCode::FORBIDDEN,
        "Relay does not hold ManageRoles"
    );
    assert_eq!(grant_body["type"], "/errors/forbidden");
}

#[tokio::test]
async fn revoking_an_owners_never_stored_role_is_a_404() {
    // Owner is derived, never stored in net_session_roles — an attempt
    // to "revoke" an owner's authority through this surface must resolve to
    // no grant (404), never a 200 (review finding: only the symmetric
    // grant-side invariant had a test).
    let app = test_app().await;
    let owner_a = sign_in_consent_callsign(&app, "ownera@example.com", "w1aw").await;
    let owner_b = sign_in_consent_callsign(&app, "ownerb@example.com", "w2bcd").await;
    let owner_b_id = account_id(&app, &owner_b).await;
    let definition_id = create_net(&app, &owner_a).await;
    let session_id = start_session(&app, &owner_a, &definition_id).await;

    // Make owner_b a co-owner too, so both resolve to Owner (never a grant
    // row) on this session.
    let (add_status, _) = send_json(
        app.router(),
        "POST",
        &format!("/api/net-definitions/{definition_id}/owners"),
        Some(json!({ "callsign": "w2bcd" })),
        Some(&owner_a),
    )
    .await;
    assert_eq!(add_status, StatusCode::OK);

    let (status, body) = send_json(
        app.router(),
        "DELETE",
        &format!("/api/net-sessions/{session_id}/roles/{owner_b_id}"),
        None,
        Some(&owner_a),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body["type"], "/errors/role-grant-not-found");
}

#[tokio::test]
async fn an_ncs_cannot_revoke_a_peer_ncs() {
    let app = test_app().await;
    let owner = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;
    let ncs_a = sign_in_consent_callsign(&app, "ncsa@example.com", "w2bcd").await;
    let ncs_b = sign_in_consent_callsign(&app, "ncsb@example.com", "w3efg").await;
    let ncs_b_id = account_id(&app, &ncs_b).await;
    let definition_id = create_net(&app, &owner).await;
    let session_id = start_session(&app, &owner, &definition_id).await;
    assert_eq!(
        grant(&app, &owner, &session_id, "w2bcd", "net-control")
            .await
            .0,
        StatusCode::OK
    );
    assert_eq!(
        grant(&app, &owner, &session_id, "w3efg", "net-control")
            .await
            .0,
        StatusCode::OK
    );

    // NCS-A tries to revoke NCS-B (a peer) — refused, and the grant survives.
    let (status, body) = send_json(
        app.router(),
        "DELETE",
        &format!("/api/net-sessions/{session_id}/roles/{ncs_b_id}"),
        None,
        Some(&ncs_a),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(body["type"], "/errors/forbidden");
    assert_eq!(
        role_row_count(&app, &session_id, &ncs_b_id).await,
        1,
        "the peer's grant was untouched"
    );
}

// --- Widened-mutation matrix -------------------------------------

#[tokio::test]
async fn an_ncs_may_qsy_and_close_a_logger_may_only_check_in() {
    let app = test_app().await;
    let owner = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;
    let ncs = sign_in_consent_callsign(&app, "ncs@example.com", "w2bcd").await;
    let logger = sign_in_consent_callsign(&app, "logger@example.com", "w3efg").await;
    let definition_id = create_net(&app, &owner).await;
    let session_id = start_session(&app, &owner, &definition_id).await;
    assert_eq!(
        grant(&app, &owner, &session_id, "w2bcd", "net-control")
            .await
            .0,
        StatusCode::OK
    );
    assert_eq!(
        grant(&app, &owner, &session_id, "w3efg", "logger").await.0,
        StatusCode::OK
    );

    // Logger: LogCheckIn yes, RunSession (frequency/close) no.
    assert_eq!(
        add_check_in(&app, &logger, &session_id, "k7abc").await,
        StatusCode::CREATED
    );
    assert_eq!(
        change_frequency(&app, &logger, &session_id, "14.300").await,
        StatusCode::FORBIDDEN
    );

    // NCS: RunSession yes — QSY then close.
    assert_eq!(
        change_frequency(&app, &ncs, &session_id, "14.310").await,
        StatusCode::OK
    );
    let (close_status, _) = send_json(
        app.router(),
        "POST",
        &format!("/api/net-sessions/{session_id}/close"),
        None,
        Some(&ncs),
    )
    .await;
    assert_eq!(close_status, StatusCode::OK);
}

#[tokio::test]
async fn a_relay_may_add_a_check_in() {
    let app = test_app().await;
    let owner = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;
    let relay = sign_in_consent_callsign(&app, "relay@example.com", "w2bcd").await;
    let definition_id = create_net(&app, &owner).await;
    let session_id = start_session(&app, &owner, &definition_id).await;
    assert_eq!(
        grant(&app, &owner, &session_id, "w2bcd", "relay").await.0,
        StatusCode::OK
    );

    assert_eq!(
        add_check_in(&app, &relay, &session_id, "k7abc").await,
        StatusCode::CREATED
    );
}

// --- Relay end-to-end: attribution + post-revoke lockout ----

#[tokio::test]
async fn a_relay_check_in_is_logged_on_the_same_session_attributed_to_the_relay() {
    let app = test_app().await;
    let owner = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;
    let ncs = sign_in_consent_callsign(&app, "ncs@example.com", "w2bcd").await;
    let relay = sign_in_consent_callsign(&app, "relay@example.com", "w3efg").await;
    let relay_id = account_id(&app, &relay).await;
    let definition_id = create_net(&app, &owner).await;
    let session_id = start_session(&app, &owner, &definition_id).await;

    // The full grant chain: owner promotes an NCS, the NCS grants a Relay
    // strictly below itself.
    assert_eq!(
        grant(&app, &owner, &session_id, "w2bcd", "net-control")
            .await
            .0,
        StatusCode::OK
    );
    assert_eq!(
        grant(&app, &ncs, &session_id, "w3efg", "relay").await.0,
        StatusCode::OK
    );

    // The relay logs a callsign-only check-in on the SAME session.
    assert_eq!(
        add_check_in(&app, &relay, &session_id, "k7abc").await,
        StatusCode::CREATED
    );

    // The committed roster entry is ATTRIBUTED to the relay's own account id
    // (addedBy — per-operator attribution). This is the "attributed to the
    // relaying operator", via the entering-operator id, NOT a new badge or
    // provenance. The Staff-entered source is the frontend's derived badge on
    // the same staff-LogCheckIn write path; there is no server "source" field.
    let (status, body) = send_json(
        app.router(),
        "GET",
        &format!("/api/net-sessions/{session_id}"),
        None,
        Some(&owner),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let roster = body["roster"].as_array().expect("roster");
    let entry = roster
        .iter()
        .find(|e| {
            e["callsign"]
                .as_str()
                .is_some_and(|c| c.eq_ignore_ascii_case("k7abc"))
        })
        .expect("the relay's check-in is on the roster");
    assert_eq!(
        entry["addedBy"], relay_id,
        "the entry is attributed to the relaying operator's account id"
    );
}

#[tokio::test]
async fn a_revoked_relay_can_no_longer_log_an_arbitrary_station() {
    let app = test_app().await;
    let owner = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;
    let relay = sign_in_consent_callsign(&app, "relay@example.com", "w2bcd").await;
    let relay_id = account_id(&app, &relay).await;
    let definition_id = create_net(&app, &owner).await;
    let session_id = start_session(&app, &owner, &definition_id).await;
    assert_eq!(
        grant(&app, &owner, &session_id, "w2bcd", "relay").await.0,
        StatusCode::OK
    );
    // While granted, the relay may check in.
    assert_eq!(
        add_check_in(&app, &relay, &session_id, "k7abc").await,
        StatusCode::CREATED
    );

    // Revoke the grant → the account resolves back to Participant.
    let (revoke_status, _) = send_json(
        app.router(),
        "DELETE",
        &format!("/api/net-sessions/{session_id}/roles/{relay_id}"),
        None,
        Some(&owner),
    )
    .await;
    assert_eq!(revoke_status, StatusCode::NO_CONTENT);

    // A revoked relay loses LogCheckIn — the STAFF power to log an ARBITRARY
    // station. The widened endpoint routes the resolved-Participant
    // to the SELF path rather than a 403: the add succeeds, but the callsign is
    // FORCED to their own and the entry is source=self — they can no longer add
    // another station on the operator's behalf.
    let (status, body) = send_json(
        app.router(),
        "POST",
        &format!("/api/net-sessions/{session_id}/check-ins"),
        Some(json!({ "callsign": "k9zzz" })),
        Some(&relay),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    let roster = body["roster"].as_array().expect("roster");
    assert!(
        !roster.iter().any(|e| e["callsign"] == "K9ZZZ"),
        "the arbitrary staff callsign is never logged by a revoked relay"
    );
    let self_entry = roster
        .iter()
        .find(|e| e["source"] == "self")
        .expect("a self-check-in entry for the revoked relay");
    assert_eq!(self_entry["callsign"], "W2BCD");
}

#[tokio::test]
async fn a_participant_is_refused_every_staff_action() {
    let app = test_app().await;
    let owner = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;
    let participant = sign_in_consent(&app, "participant@example.com").await;
    let _target = sign_in_consent_callsign(&app, "target@example.com", "w3efg").await;
    let definition_id = create_net(&app, &owner).await;
    let session_id = start_session(&app, &owner, &definition_id).await;

    // The server-side floor: a Participant holds NO staff capability.
    assert_eq!(
        add_check_in(&app, &participant, &session_id, "k7abc").await,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        change_frequency(&app, &participant, &session_id, "14.300").await,
        StatusCode::FORBIDDEN
    );
    let (close_status, _) = send_json(
        app.router(),
        "POST",
        &format!("/api/net-sessions/{session_id}/close"),
        None,
        Some(&participant),
    )
    .await;
    assert_eq!(close_status, StatusCode::FORBIDDEN);
    // Reading the staff console is ViewConsole — also denied.
    let (view_status, _) = send_json(
        app.router(),
        "GET",
        &format!("/api/net-sessions/{session_id}"),
        None,
        Some(&participant),
    )
    .await;
    assert_eq!(view_status, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn staff_may_view_the_console_and_the_owner_regression_holds() {
    let app = test_app().await;
    let owner = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;
    let logger = sign_in_consent_callsign(&app, "logger@example.com", "w2bcd").await;
    let definition_id = create_net(&app, &owner).await;
    let session_id = start_session(&app, &owner, &definition_id).await;
    assert_eq!(
        grant(&app, &owner, &session_id, "w2bcd", "logger").await.0,
        StatusCode::OK
    );

    // A Logger may view the staff console (ViewConsole).
    let (logger_view, _) = send_json(
        app.router(),
        "GET",
        &format!("/api/net-sessions/{session_id}"),
        None,
        Some(&logger),
    )
    .await;
    assert_eq!(logger_view, StatusCode::OK);

    // Owner regression: the owner (apex) still holds every capability.
    let (owner_view, _) = send_json(
        app.router(),
        "GET",
        &format!("/api/net-sessions/{session_id}"),
        None,
        Some(&owner),
    )
    .await;
    assert_eq!(owner_view, StatusCode::OK);
    assert_eq!(
        change_frequency(&app, &owner, &session_id, "14.320").await,
        StatusCode::OK
    );
    assert_eq!(
        add_check_in(&app, &owner, &session_id, "k7abc").await,
        StatusCode::CREATED
    );
}

// --- 404-before-403 -------------------------------------------------

#[tokio::test]
async fn a_nonexistent_session_is_404_before_any_authorization() {
    let app = test_app().await;
    let owner = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;
    let participant = sign_in_consent(&app, "participant@example.com").await;
    // A real session exists for a DIFFERENT id — but we hit a random one.
    let definition_id = create_net(&app, &owner).await;
    let _real = start_session(&app, &owner, &definition_id).await;
    let ghost = uuid::Uuid::now_v7();

    // A stranger mutating a session that does not exist gets 404, not 403 —
    // existence is decided before authority.
    let (status, body) = send_json(
        app.router(),
        "POST",
        &format!("/api/net-sessions/{ghost}/check-ins"),
        Some(json!({ "callsign": "k7abc" })),
        Some(&participant),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body["type"], "/errors/net-session-not-found");

    // Same for the role surface.
    let (grant_status, _) = grant(&app, &participant, &ghost.to_string(), "w1aw", "logger").await;
    assert_eq!(grant_status, StatusCode::NOT_FOUND);
}

// --- Cross-net isolation ----------------------------------------

#[tokio::test]
async fn a_role_granted_on_one_session_grants_nothing_on_another() {
    let app = test_app().await;
    let owner_a = sign_in_consent_callsign(&app, "ownera@example.com", "w1aw").await;
    let owner_b = sign_in_consent_callsign(&app, "ownerb@example.com", "w2bcd").await;
    let staff = sign_in_consent_callsign(&app, "staff@example.com", "w3efg").await;

    let def_a = create_net(&app, &owner_a).await;
    let session_a = start_session(&app, &owner_a, &def_a).await;
    let def_b = create_net(&app, &owner_b).await;
    let session_b = start_session(&app, &owner_b, &def_b).await;

    // Grant Logger to staff on session A only.
    assert_eq!(
        grant(&app, &owner_a, &session_a, "w3efg", "logger").await.0,
        StatusCode::OK
    );

    // Staff may check in on session A ...
    assert_eq!(
        add_check_in(&app, &staff, &session_a, "k7abc").await,
        StatusCode::CREATED
    );
    // ... but on session B the same account holds NO staff role — it is a
    // Participant. The widened endpoint routes it to the SELF path:
    // it may check ITSELF in (own callsign forced, source=self) but cannot log the
    // arbitrary "k7abc" a staff operator would. No global role; isolation by
    // construction — the staff LogCheckIn power did not leak across.
    let (status, body) = send_json(
        app.router(),
        "POST",
        &format!("/api/net-sessions/{session_b}/check-ins"),
        Some(json!({ "callsign": "k7abc" })),
        Some(&staff),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    let roster = body["roster"].as_array().expect("roster");
    assert!(
        !roster.iter().any(|e| e["callsign"] == "K7ABC"),
        "no staff role leaked cross-session: the arbitrary callsign is not logged"
    );
    let self_entry = roster
        .iter()
        .find(|e| e["source"] == "self")
        .expect("a self-check-in entry on session B");
    assert_eq!(self_entry["callsign"], "W3EFG");
}

// --- Roles-list read -----------------------------

#[tokio::test]
async fn an_owner_and_an_ncs_can_list_the_grants_with_callsigns() {
    let app = test_app().await;
    let owner = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;
    let ncs = sign_in_consent_callsign(&app, "ncs@example.com", "w2bcd").await;
    let ncs_id = account_id(&app, &ncs).await;
    let relay = sign_in_consent_callsign(&app, "relay@example.com", "w3efg").await;
    let relay_id = account_id(&app, &relay).await;
    let definition_id = create_net(&app, &owner).await;
    let session_id = start_session(&app, &owner, &definition_id).await;

    // Owner grants NCS, then NCS grants a Relay below itself.
    assert_eq!(
        grant(&app, &owner, &session_id, "w2bcd", "net-control")
            .await
            .0,
        StatusCode::OK
    );
    assert_eq!(
        grant(&app, &ncs, &session_id, "w3efg", "relay").await.0,
        StatusCode::OK
    );

    // Both an Owner and an NCS (ManageRoles holders) may read the list.
    for reader in [&owner, &ncs] {
        let (status, body) = list_roles(&app, Some(reader), &session_id).await;
        assert_eq!(status, StatusCode::OK);
        let grants = body.as_array().expect("array of grants");
        assert_eq!(grants.len(), 2, "the two explicit grants are listed");

        let by_account: std::collections::HashMap<_, _> = grants
            .iter()
            .map(|g| (g["accountId"].as_str().unwrap().to_owned(), g))
            .collect();
        let ncs_grant = by_account.get(&ncs_id).expect("ncs grant present");
        assert_eq!(ncs_grant["role"], "net-control");
        assert_eq!(
            ncs_grant["callsign"].as_str().unwrap().to_uppercase(),
            "W2BCD"
        );
        // grantedBy is the granting account; grantedAt is an RFC 3339 string.
        assert_eq!(
            ncs_grant["grantedBy"].as_str().unwrap(),
            account_id(&app, &owner).await
        );
        assert!(ncs_grant["grantedAt"].as_str().unwrap().contains('T'));
        let relay_grant = by_account.get(&relay_id).expect("relay grant present");
        assert_eq!(relay_grant["role"], "relay");
    }
}

#[tokio::test]
async fn the_roles_list_excludes_owners_and_never_carries_email() {
    let app = test_app().await;
    let owner = sign_in_consent_callsign(&app, "owner-secret@example.com", "w1aw").await;
    let _relay = sign_in_consent_callsign(&app, "relay@example.com", "w3efg").await;
    let definition_id = create_net(&app, &owner).await;
    let session_id = start_session(&app, &owner, &definition_id).await;
    assert_eq!(
        grant(&app, &owner, &session_id, "w3efg", "relay").await.0,
        StatusCode::OK
    );

    let (status, body) = list_roles(&app, Some(&owner), &session_id).await;
    assert_eq!(status, StatusCode::OK);
    let grants = body.as_array().expect("array");
    // The owner is derived, never a stored grant — only the explicit relay
    // grant appears, not the owner.
    assert_eq!(grants.len(), 1);
    assert_eq!(grants[0]["role"], "relay");
    // No email ever crosses this surface.
    assert!(
        !body.to_string().contains('@'),
        "the roles list must never carry an email address"
    );
}

#[tokio::test]
async fn a_logger_relay_or_participant_cannot_list_the_grants() {
    let app = test_app().await;
    let owner = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;
    let logger = sign_in_consent_callsign(&app, "logger@example.com", "w2bcd").await;
    let relay = sign_in_consent_callsign(&app, "relay@example.com", "w3efg").await;
    let participant = sign_in_consent(&app, "participant@example.com").await;
    let definition_id = create_net(&app, &owner).await;
    let session_id = start_session(&app, &owner, &definition_id).await;
    assert_eq!(
        grant(&app, &owner, &session_id, "w2bcd", "logger").await.0,
        StatusCode::OK
    );
    assert_eq!(
        grant(&app, &owner, &session_id, "w3efg", "relay").await.0,
        StatusCode::OK
    );

    // Only a ManageRoles holder may audit who holds a role. A Logger and a
    // Relay hold ViewConsole but NOT ManageRoles; a Participant holds nothing.
    for below in [&logger, &relay, &participant] {
        let (status, _) = list_roles(&app, Some(below), &session_id).await;
        assert_eq!(status, StatusCode::FORBIDDEN);
    }
}

#[tokio::test]
async fn listing_roles_on_a_nonexistent_session_is_404_before_403() {
    let app = test_app().await;
    // A Participant (no ManageRoles) hitting a session that does not exist gets
    // 404, not 403 — existence is decided before authority (404-before-403).
    let participant = sign_in_consent(&app, "participant@example.com").await;
    let ghost = uuid::Uuid::now_v7();
    let (status, body) = list_roles(&app, Some(&participant), &ghost.to_string()).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body["type"], "/errors/net-session-not-found");
}

#[tokio::test]
async fn the_roles_list_is_not_reachable_account_less() {
    let app = test_app().await;
    let owner = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;
    let definition_id = create_net(&app, &owner).await;
    let session_id = start_session(&app, &owner, &definition_id).await;

    // No cookie: the roles list sits behind the account gate, NOT on the
    // account-less/public surface — an unauthenticated read is refused.
    let (status, _) = list_roles(&app, None, &session_id).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

// --- viewerRole on the staff summary, redacted from public --

#[tokio::test]
async fn the_staff_summary_carries_the_viewers_own_resolved_role() {
    let app = test_app().await;
    let owner = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;
    let relay = sign_in_consent_callsign(&app, "relay@example.com", "w2bcd").await;
    let logger = sign_in_consent_callsign(&app, "logger@example.com", "w3efg").await;
    let definition_id = create_net(&app, &owner).await;
    let session_id = start_session(&app, &owner, &definition_id).await;
    assert_eq!(
        grant(&app, &owner, &session_id, "w2bcd", "relay").await.0,
        StatusCode::OK
    );
    assert_eq!(
        grant(&app, &owner, &session_id, "w3efg", "logger").await.0,
        StatusCode::OK
    );

    // Each viewer's OWN role is computed server-side via resolve_role and
    // returned on the staff summary: owner → owner, granted relay → relay,
    // granted logger → logger.
    for (cookie, expected) in [(&owner, "owner"), (&relay, "relay"), (&logger, "logger")] {
        let (status, body) = send_json(
            app.router(),
            "GET",
            &format!("/api/net-sessions/{session_id}"),
            None,
            Some(cookie),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["viewerRole"], expected);
    }
}

#[tokio::test]
async fn the_public_summary_never_carries_a_viewer_role() {
    let app = test_app().await;
    let owner = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;
    let definition_id = create_net(&app, &owner).await;
    let session_id = start_session(&app, &owner, &definition_id).await;

    // The account-less public view carries NO viewer role — role information
    // never crosses the public wire. A missing key reads as JSON null.
    let (status, body) = send_json(
        app.router(),
        "GET",
        &format!("/api/net-sessions/{session_id}/live"),
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        body.get("viewerRole").is_none(),
        "the public summary must not carry viewerRole"
    );
}
