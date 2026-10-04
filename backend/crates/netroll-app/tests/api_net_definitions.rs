// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
//! Full-loop API tests for net-definition CRUD: real router,
//! real Postgres (testcontainers), capturing fake mailer. Asserts status
//! codes, problem+json `type` slugs, body values, and DB side-effects —
//! never message prose (house TDD rule).

use std::sync::{Arc, Mutex};

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use netroll_app::http::{AppState, api_router};
use netroll_domain::consent::CURRENT_TERMS_VERSION;
use netroll_domain::net::connection::MAX_CONNECTIONS;
use netroll_domain::ports::{BoxFuture, MailError, Mailer};
use serde_json::{Value, json};
use sqlx::PgPool;
use sqlx::postgres::PgPoolOptions;
use testcontainers_modules::postgres::Postgres;
use testcontainers_modules::testcontainers::ContainerAsync;
use testcontainers_modules::testcontainers::runners::AsyncRunner;
use tower::ServiceExt;
use uuid::Uuid;

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

/// Signs in a fresh email and records consent (net creation is gated on
/// `ConsentedAccount`). Returns the session cookie.
async fn sign_in_and_consent(app: &TestApp, email: &str) -> String {
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

/// Signs in, consents, AND reserves a callsign — the full gate net creation
/// requires.
async fn sign_in_consent_callsign(app: &TestApp, email: &str, callsign: &str) -> String {
    let cookie = sign_in_and_consent(app, email).await;
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

/// The create body most tests start from: the scalar fields plus the four ways
/// in the older flat fixture used to mint (an RF way with an offset and a
/// tone, an EchoLink node, an AllStar node, and a reflector — now CLASSIFIED as
/// D-Star, because the create body can say so). NOT the body a scalar `PUT`
/// test edits: the scalar route REFUSES a
/// `connections` key (or any key it does not read) with a 400, so the `PUT`
/// tests start from `scalar_definition_json` instead.
fn full_definition_json() -> Value {
    let mut body = scalar_definition_json();
    body["connections"] = json!([
        {
            "kind": "repeater",
            "plannedFrequencyHz": 14_230_000,
            "band": "20m",
            "mode": "ssb",
            "repeaterOffsetHz": -600_000,
            "toneMode": "ctcss",
            "toneValue": "100.0",
        },
        { "kind": "echolink", "node": "12345" },
        { "kind": "allstar", "node": "54321" },
        { "kind": "dstar", "reflector": "REF030C" },
    ]);
    body
}

async fn definition_count(app: &TestApp) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM net_definitions")
        .fetch_one(&app.pool)
        .await
        .expect("count definitions")
}

#[tokio::test]
async fn create_defaults_visibility_to_listed_with_a_link_token() {
    let app = test_app().await;
    let cookie = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;

    // No `visibility` in the body → defaults to listed, with a non-empty token.
    let (status, body) = send_json(
        app.router(),
        "POST",
        "/api/net-definitions",
        Some(full_definition_json()),
        Some(&cookie),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(body["visibility"], "listed");
    let token = body["linkToken"].as_str().expect("linkToken present");
    assert!(!token.is_empty(), "every net gets a link token");
    // Owner body still exposes ownerAccountIds (unchanged).
    assert_eq!(body["ownerAccountIds"].as_array().expect("owners").len(), 1);
}

#[tokio::test]
async fn create_accepts_explicit_unlisted_and_rejects_unknown_visibility() {
    let app = test_app().await;
    let cookie = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;

    let mut unlisted = full_definition_json();
    unlisted["visibility"] = json!("unlisted");
    let (status, body) = send_json(
        app.router(),
        "POST",
        "/api/net-definitions",
        Some(unlisted),
        Some(&cookie),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(body["visibility"], "unlisted");

    // An unknown token is a field-level 400, no row written for it.
    let before = definition_count(&app).await;
    let mut bad = full_definition_json();
    bad["visibility"] = json!("public");
    let (status, problem) = send_json(
        app.router(),
        "POST",
        "/api/net-definitions",
        Some(bad),
        Some(&cookie),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(problem["type"], "/errors/net-definition-invalid");
    assert!(
        problem["detail"]
            .as_str()
            .expect("detail")
            .starts_with("visibility:"),
        "detail names the visibility field"
    );
    assert_eq!(
        definition_count(&app).await,
        before,
        "no row written for an unknown visibility"
    );
}

#[tokio::test]
async fn edit_flips_visibility_bumps_version_and_keeps_the_link_token_stable() {
    let app = test_app().await;
    let cookie = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;
    let (_, created) = send_json(
        app.router(),
        "POST",
        "/api/net-definitions",
        Some(full_definition_json()),
        Some(&cookie),
    )
    .await;
    let id = created["id"].as_str().expect("id").to_owned();
    let original_token = created["linkToken"].as_str().expect("token").to_owned();

    let mut edit = scalar_definition_json();
    edit["visibility"] = json!("unlisted");
    let (status, body) = send_json(
        app.router(),
        "PUT",
        &format!("/api/net-definitions/{id}"),
        Some(edit),
        Some(&cookie),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["visibility"], "unlisted");
    assert_eq!(body["definitionVersion"], 2);
    assert_eq!(
        body["linkToken"].as_str().expect("token"),
        original_token,
        "the permalink token is stable across edits"
    );

    // Flipping back to listed works and increments again.
    let mut back = scalar_definition_json();
    back["visibility"] = json!("listed");
    let (status, body) = send_json(
        app.router(),
        "PUT",
        &format!("/api/net-definitions/{id}"),
        Some(back),
        Some(&cookie),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["visibility"], "listed");
    assert_eq!(body["definitionVersion"], 3);
    assert_eq!(body["linkToken"].as_str().expect("token"), original_token);
}

/// Creates a net with the given visibility and returns (id, linkToken).
async fn create_net(app: &TestApp, cookie: &str, visibility: &str) -> (String, String) {
    let mut body = full_definition_json();
    body["visibility"] = json!(visibility);
    let (status, created) = send_json(
        app.router(),
        "POST",
        "/api/net-definitions",
        Some(body),
        Some(cookie),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    (
        created["id"].as_str().expect("id").to_owned(),
        created["linkToken"].as_str().expect("token").to_owned(),
    )
}

#[tokio::test]
async fn public_read_by_token_returns_a_minimal_body_for_any_visibility_without_a_session() {
    let app = test_app().await;
    let cookie = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;

    for visibility in ["unlisted", "listed"] {
        let (id, token) = create_net(&app, &cookie, visibility).await;

        // UNAUTHENTICATED (no cookie) read by the token resolves the net.
        let (status, body) = send_json(
            app.router(),
            "GET",
            &format!("/api/net-definitions/by-token/{token}"),
            None,
            None,
        )
        .await;
        assert_eq!(
            status,
            StatusCode::OK,
            "{visibility} net reachable by token"
        );
        assert_eq!(body["id"], id);
        assert_eq!(body["title"], "Sunday Traffic Net");
        assert_eq!(body["connections"][0]["plannedFrequencyHz"], 14_230_000);
        assert_eq!(body["connections"][0]["band"], "20m");
        assert_eq!(body["visibility"], visibility);
        // The public projection leaks NEITHER owner identities NOR the token.
        assert!(
            body.get("ownerAccountIds").is_none(),
            "public body must not expose ownerAccountIds"
        );
        assert!(
            body.get("linkToken").is_none(),
            "public body need not echo the token"
        );
    }
}

#[tokio::test]
async fn public_read_by_a_missing_or_wrong_token_is_a_uniform_404_never_403() {
    let app = test_app().await;
    let cookie = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;

    // A real Unlisted net exists, but we never present its token.
    let (_id, real_token) = create_net(&app, &cookie, "unlisted").await;

    // A well-formed-but-unknown token and a short garbage token both refuse
    // with the SAME uniform 404 — never 401/403.
    let mut bodies = Vec::new();
    for token in ["Zm9vYmFyYmF6cXV1eGZvb2Jhcg", "zzz"] {
        let (status, problem) = send_json(
            app.router(),
            "GET",
            &format!("/api/net-definitions/by-token/{token}"),
            None,
            None,
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(problem["type"], "/errors/net-definition-not-found");
        assert_ne!(status, StatusCode::UNAUTHORIZED);
        assert_ne!(status, StatusCode::FORBIDDEN);
        bodies.push(problem);
    }
    // The unknown-token refusal is byte-identical regardless of which wrong
    // token was tried (existence is unobservable).
    assert_eq!(bodies[0], bodies[1]);

    // Existence-hiding: the SAME endpoint returns 200 only when the caller
    // holds the real capability, and 404 otherwise — the outcomes differ only
    // by possession of the token, never by whether the net exists.
    let (status, _) = send_json(
        app.router(),
        "GET",
        &format!("/api/net-definitions/by-token/{real_token}"),
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "the real token resolves");
}

/// The "missing token" case, literally: no `{token}` segment at all.
/// `matchit` never matches an empty dynamic segment, so the bare path is
/// exactly as long as the OWNER `/api/net-definitions/{id}` route
/// (id="by-token") — without an explicit route, this would either 401 via
/// `require_session` (test harness) or, in production merged with the SPA
/// static fallback, 200 with the app shell. Both are refused with the same
/// uniform 404 as any other non-resolving token, never touching auth.
#[tokio::test]
async fn public_read_with_no_token_segment_at_all_is_the_same_uniform_404() {
    let app = test_app().await;

    for path in [
        "/api/net-definitions/by-token",
        "/api/net-definitions/by-token/",
    ] {
        let (status, problem) = send_json(app.router(), "GET", path, None, None).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{path} must be a 404");
        assert_eq!(problem["type"], "/errors/net-definition-not-found");
        assert_ne!(status, StatusCode::UNAUTHORIZED, "{path} must never 401");
        assert_ne!(status, StatusCode::FORBIDDEN, "{path} must never 403");
    }
}

#[tokio::test]
async fn create_happy_path_returns_201_with_version_1_and_sole_owner() {
    let app = test_app().await;
    let cookie = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;

    let (status, body) = send_json(
        app.router(),
        "POST",
        "/api/net-definitions",
        Some(full_definition_json()),
        Some(&cookie),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(body["definitionVersion"], 1);
    assert_eq!(body["title"], "Sunday Traffic Net");
    assert_eq!(body["connections"][0]["plannedFrequencyHz"], 14_230_000);
    assert_eq!(body["connections"][0]["band"], "20m");
    assert_eq!(body["grid"], "FN31pr", "grid canonicalized");
    assert_eq!(body["connections"][0]["repeaterOffsetHz"], -600_000);
    assert_eq!(body["expectedDurationMinutes"], 90);
    let id = body["id"].as_str().expect("id present").to_owned();
    let owners = body["ownerAccountIds"].as_array().expect("owner array");
    assert_eq!(owners.len(), 1, "sole owner recorded");

    // The owner's GET agrees.
    let (status, got) = send_json(
        app.router(),
        "GET",
        &format!("/api/net-definitions/{id}"),
        None,
        Some(&cookie),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(got["id"], id);
    assert_eq!(got["definitionVersion"], 1);
    assert_eq!(got["ownerAccountIds"], body["ownerAccountIds"]);
}

#[tokio::test]
async fn create_requires_authentication() {
    let app = test_app().await;
    let (status, problem) = send_json(
        app.router(),
        "POST",
        "/api/net-definitions",
        Some(full_definition_json()),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(problem["type"], "/errors/unauthenticated");
}

#[tokio::test]
async fn create_requires_consent() {
    let app = test_app().await;
    let cookie = {
        // Sign in WITHOUT consent.
        let (status, _) = send_json(
            app.router(),
            "POST",
            "/api/magic-links",
            Some(json!({ "email": "op@example.com" })),
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
            .expect("build");
        let response = app.router().oneshot(request).await.expect("route");
        response
            .headers()
            .get(header::SET_COOKIE)
            .expect("cookie")
            .to_str()
            .expect("ascii")
            .split(';')
            .next()
            .expect("pair")
            .to_owned()
    };

    let (status, problem) = send_json(
        app.router(),
        "POST",
        "/api/net-definitions",
        Some(full_definition_json()),
        Some(&cookie),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(problem["type"], "/errors/consent-required");
}

#[tokio::test]
async fn create_requires_a_callsign() {
    let app = test_app().await;
    // Consented but NO callsign reserved.
    let cookie = sign_in_and_consent(&app, "op@example.com").await;

    let (status, problem) = send_json(
        app.router(),
        "POST",
        "/api/net-definitions",
        Some(full_definition_json()),
        Some(&cookie),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(problem["type"], "/errors/callsign-required");
    assert_eq!(definition_count(&app).await, 0, "no row written");
}

#[tokio::test]
async fn create_refused_when_email_is_unverified() {
    // A defensive branch, unreachable via the shipped flow: force the
    // account's email_verified_at to NULL directly, then attempt create.
    let app = test_app().await;
    let cookie = sign_in_consent_callsign(&app, "op@example.com", "w1aw").await;
    sqlx::query("UPDATE accounts SET email_verified_at = NULL WHERE email = $1")
        .bind("op@example.com")
        .execute(&app.pool)
        .await
        .expect("clear verification");

    let (status, problem) = send_json(
        app.router(),
        "POST",
        "/api/net-definitions",
        Some(full_definition_json()),
        Some(&cookie),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(problem["type"], "/errors/email-unverified");
}

#[tokio::test]
async fn invalid_fields_are_400_net_definition_invalid_with_field_specific_detail() {
    let app = test_app().await;
    let cookie = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;

    // Two scalar malformations and two on the first connection: a bad band or
    // frequency is a fault of a CONNECTION, reported under the same problem
    // type with the entry's index beside it.
    type Malformation = fn(&mut Value);
    let cases: [(&str, Malformation); 4] = [
        ("grid", |body| body["grid"] = json!("nope")),
        ("band", |body| body["connections"][0]["band"] = json!("21m")),
        ("netType", |body| body["netType"] = json!("rollcall")),
        // 300 GHz, above the 250 GHz ceiling. Written as Hz; the older fixture
        // was the MHz string "300000", which is the same
        // frequency — a naive conversion to `300_000` would be 300 kHz, inside
        // the span, and the case would stop testing anything.
        ("plannedFrequencyHz", |body| {
            body["connections"][0]["plannedFrequencyHz"] = json!(300_000_000_000_i64)
        }),
    ];
    let mut details = Vec::new();
    for (field, malform) in cases {
        let mut body = full_definition_json();
        malform(&mut body);
        let (status, problem) = send_json(
            app.router(),
            "POST",
            "/api/net-definitions",
            Some(body),
            Some(&cookie),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{field} must reject");
        assert_eq!(problem["type"], "/errors/net-definition-invalid");
        let detail = problem["detail"]
            .as_str()
            .expect("detail names the field")
            .to_owned();
        assert!(!detail.is_empty());
        details.push(detail);
    }
    // Distinct malformations carry distinct detail.
    assert_ne!(details[0], details[1]);
    assert_ne!(details[1], details[3]);
    assert_eq!(
        definition_count(&app).await,
        0,
        "no row written for any invalid input"
    );
}

#[tokio::test]
async fn edit_increments_version_and_ignores_client_supplied_version() {
    let app = test_app().await;
    let cookie = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;
    let (_, created) = send_json(
        app.router(),
        "POST",
        "/api/net-definitions",
        Some(full_definition_json()),
        Some(&cookie),
    )
    .await;
    let id = created["id"].as_str().expect("id").to_owned();

    // Edit with changed fields AND an attempt to set the version to 99.
    let mut edit = scalar_definition_json();
    edit["title"] = json!("Monday Emergency Net");
    edit["netType"] = json!("roll-call");
    edit["definitionVersion"] = json!(99);
    let (status, body) = send_json(
        app.router(),
        "PUT",
        &format!("/api/net-definitions/{id}"),
        Some(edit.clone()),
        Some(&cookie),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["title"], "Monday Emergency Net");
    assert_eq!(body["netType"], "roll-call");
    assert_eq!(
        body["definitionVersion"], 2,
        "server increment, client 99 ignored"
    );

    // A second edit → 3.
    let (status, body) = send_json(
        app.router(),
        "PUT",
        &format!("/api/net-definitions/{id}"),
        Some(edit),
        Some(&cookie),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["definitionVersion"], 3);
}

#[tokio::test]
async fn a_non_owner_cannot_edit_or_delete_and_gets_403() {
    let app = test_app().await;
    let owner_cookie = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;
    let (_, created) = send_json(
        app.router(),
        "POST",
        "/api/net-definitions",
        Some(full_definition_json()),
        Some(&owner_cookie),
    )
    .await;
    let id = created["id"].as_str().expect("id").to_owned();

    let stranger_cookie = sign_in_consent_callsign(&app, "stranger@example.com", "k1abc").await;

    let mut edit = scalar_definition_json();
    edit["title"] = json!("Hijacked");
    let (status, problem) = send_json(
        app.router(),
        "PUT",
        &format!("/api/net-definitions/{id}"),
        Some(edit),
        Some(&stranger_cookie),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(problem["type"], "/errors/forbidden");

    let (status, problem) = send_json(
        app.router(),
        "DELETE",
        &format!("/api/net-definitions/{id}"),
        None,
        Some(&stranger_cookie),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(problem["type"], "/errors/forbidden");

    // A stranger cannot even read it (owner-scoped GET) — and the definition
    // is unchanged when the owner reads it back.
    let (status, _) = send_json(
        app.router(),
        "GET",
        &format!("/api/net-definitions/{id}"),
        None,
        Some(&stranger_cookie),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, body) = send_json(
        app.router(),
        "GET",
        &format!("/api/net-definitions/{id}"),
        None,
        Some(&owner_cookie),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["title"], "Sunday Traffic Net", "not hijacked");
    assert_eq!(body["definitionVersion"], 1, "unchanged");
}

#[tokio::test]
async fn missing_definition_is_404() {
    let app = test_app().await;
    let cookie = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;
    let missing = uuid::Uuid::now_v7();

    for method in ["GET", "PUT", "DELETE"] {
        let body = if method == "PUT" {
            Some(full_definition_json())
        } else {
            None
        };
        let (status, problem) = send_json(
            app.router(),
            method,
            &format!("/api/net-definitions/{missing}"),
            body,
            Some(&cookie),
        )
        .await;
        assert_eq!(
            status,
            StatusCode::NOT_FOUND,
            "{method} on a missing id is 404"
        );
        assert_eq!(problem["type"], "/errors/net-definition-not-found");
    }
}

#[tokio::test]
async fn delete_archives_the_definition_and_the_owner_can_still_read_it() {
    // Owner-initiated delete ARCHIVES rather than hard-deletes.
    // The row survives (never 404), archivedAt is set, and the owner GET still
    // resolves it (so a client can show "archived").
    let app = test_app().await;
    let cookie = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;
    let (_, created) = send_json(
        app.router(),
        "POST",
        "/api/net-definitions",
        Some(full_definition_json()),
        Some(&cookie),
    )
    .await;
    let id = created["id"].as_str().expect("id").to_owned();

    let (status, _) = send_json(
        app.router(),
        "DELETE",
        &format!("/api/net-definitions/{id}"),
        None,
        Some(&cookie),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    // The row survives (archived, not hard-deleted) and the owner can read it.
    let (status, body) = send_json(
        app.router(),
        "GET",
        &format!("/api/net-definitions/{id}"),
        None,
        Some(&cookie),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "the archived net is still readable");
    assert!(
        body["archivedAt"].as_str().is_some(),
        "archivedAt is populated after delete-as-archive"
    );
    assert_eq!(
        definition_count(&app).await,
        1,
        "the row survives for provenance"
    );
}

#[tokio::test]
async fn list_owned_returns_only_the_callers_active_nets_with_live_and_next_occurrence() {
    // The My Nets Owned tab (netsApi.ts's `getOwnedNets`): scoped to the
    // caller's own active (non-archived) nets, each carrying `liveSessionId`
    // and `nextOccurrenceAt` — `null` for a net with neither.
    let app = test_app().await;
    let cookie = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;
    let (_, mine) = send_json(
        app.router(),
        "POST",
        "/api/net-definitions",
        Some(full_definition_json()),
        Some(&cookie),
    )
    .await;
    let mine_id = mine["id"].as_str().expect("id").to_owned();

    // Another account's net must never appear in my list.
    let other_cookie = sign_in_consent_callsign(&app, "other@example.com", "n0call").await;
    send_json(
        app.router(),
        "POST",
        "/api/net-definitions",
        Some(full_definition_json()),
        Some(&other_cookie),
    )
    .await;

    // An archived net of mine must not appear either.
    let (_, archived) = send_json(
        app.router(),
        "POST",
        "/api/net-definitions",
        Some(full_definition_json()),
        Some(&cookie),
    )
    .await;
    let archived_id = archived["id"].as_str().expect("id").to_owned();
    let (status, _) = send_json(
        app.router(),
        "DELETE",
        &format!("/api/net-definitions/{archived_id}"),
        None,
        Some(&cookie),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    let (status, body) = send_json(
        app.router(),
        "GET",
        "/api/net-definitions",
        None,
        Some(&cookie),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let nets = body["items"].as_array().expect("items array");
    let ids: Vec<&str> = nets.iter().map(|n| n["id"].as_str().expect("id")).collect();
    assert_eq!(
        ids,
        vec![mine_id.as_str()],
        "excludes another account's net and my archived one"
    );
    assert_eq!(nets[0]["liveSessionId"], Value::Null, "not live yet");
    assert_eq!(
        nets[0]["nextOccurrenceAt"],
        Value::Null,
        "unscheduled — no occurrence yet"
    );
}

#[tokio::test]
async fn list_owned_populates_live_session_id_once_a_session_is_started() {
    let app = test_app().await;
    let cookie = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;
    let (_, created) = send_json(
        app.router(),
        "POST",
        "/api/net-definitions",
        Some(full_definition_json()),
        Some(&cookie),
    )
    .await;
    let id = created["id"].as_str().expect("id").to_owned();

    let (status, session) = send_json(
        app.router(),
        "POST",
        "/api/net-sessions",
        Some(json!({ "definitionId": id })),
        Some(&cookie),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "start the session");
    let session_id = session["id"].as_str().expect("session id").to_owned();

    let (status, body) = send_json(
        app.router(),
        "GET",
        "/api/net-definitions",
        None,
        Some(&cookie),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let nets = body["items"].as_array().expect("items array");
    assert_eq!(
        nets[0]["liveSessionId"], session_id,
        "the Owned tab's live badge / Open-console affordance reads this"
    );
}

#[tokio::test]
async fn list_owned_requires_authentication() {
    let app = test_app().await;
    let (status, problem) =
        send_json(app.router(), "GET", "/api/net-definitions", None, None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(problem["type"], "/errors/unauthenticated");
}

// ---- Paged read ---------------------------------------------------

/// Walks `uri` by echoing `nextCursor`, returning every page's `items`. Capped
/// so a cursor that stops advancing fails instead of hanging CI.
async fn walk_pages(app: &TestApp, uri: &str, cookie: &str, limit: usize) -> Vec<Vec<Value>> {
    let mut pages = Vec::new();
    let mut next = format!("{uri}?limit={limit}");
    for _ in 0..10 {
        let (status, body) = send_json(app.router(), "GET", &next, None, Some(cookie)).await;
        assert_eq!(status, StatusCode::OK, "got {body}");
        let items = body["items"].as_array().expect("items array").clone();
        assert!(
            items.len() <= limit,
            "a page never exceeds the requested limit: {} > {limit}",
            items.len()
        );
        pages.push(items);
        // `.get`, not `body["nextCursor"]`: serde_json's `Index` answers `Null`
        // for an ABSENT key, which would read a body that lost the key as "last
        // page, done" — the one contract break this fence exists to catch.
        match body.get("nextCursor") {
            None => panic!("nextCursor is absent from the page body: {body}"),
            Some(Value::Null) => return pages,
            Some(Value::String(cursor)) => {
                next = format!("{uri}?limit={limit}&cursor={cursor}");
            }
            Some(other) => panic!("nextCursor is a string or null, got {other}"),
        }
    }
    panic!("the cursor never reached null in ten pages — it is not advancing");
}

/// Seeds one owned net per entry of `offsets` through the real POST route, then
/// pins each `created_at` to `base + offset` by UPDATE — two POSTs cannot land
/// in one millisecond on purpose, so a shared instant is a fixture fact, not a
/// hope. Returns the seeded `(created_at_millis, id)` pairs in seeding order.
async fn seed_owned_nets_at(app: &TestApp, cookie: &str, offsets: &[u64]) -> Vec<(u64, Uuid)> {
    let base = 1_754_000_000_000_u64;
    let mut seeded: Vec<(u64, Uuid)> = Vec::with_capacity(offsets.len());
    for offset in offsets {
        let (status, created) = send_json(
            app.router(),
            "POST",
            "/api/net-definitions",
            Some(full_definition_json()),
            Some(cookie),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED, "got {created}");
        let id: Uuid = created["id"].as_str().expect("id").parse().expect("uuid");
        sqlx::query(
            "UPDATE net_definitions SET created_at = to_timestamp($2::double precision / 1000)
             WHERE id = $1",
        )
        .bind(id)
        .bind((base + offset) as i64)
        .execute(&app.pool)
        .await
        .expect("pin created_at");
        seeded.push((base + offset, id));
    }
    seeded
}

#[tokio::test]
async fn the_owned_nets_read_pages_through_every_net_without_gap_or_repeat() {
    // Five owned nets (under the default cap of 7), their
    // `created_at` pinned so the middle pair shares a millisecond and STRADDLES
    // the limit=2 first-page edge.
    let app = test_app().await;
    let cookie = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;
    let seeded = seed_owned_nets_at(&app, &cookie, &[1_000, 2_000, 3_000, 3_000, 4_000]).await;

    let pages = walk_pages(&app, "/api/net-definitions", &cookie, 2).await;

    // THE FIXTURE'S OWN PRECONDITION: the tie really does straddle the first edge.
    assert!(pages.len() >= 2, "more than one page: {pages:?}");
    assert_eq!(
        pages[0][1]["createdAt"], pages[1][0]["createdAt"],
        "page 1's last row and page 2's first row must share a createdAt, \
         or this test proves nothing about a shared-timestamp boundary"
    );

    let seen: Vec<String> = pages
        .iter()
        .flatten()
        .map(|n| n["id"].as_str().expect("id").to_owned())
        .collect();
    // Newest-created first; within a shared millisecond, the higher id first
    // (`DESC, DESC` — the row-value cursor's total order).
    let mut expected = seeded.clone();
    expected.sort_by(|a, b| b.cmp(a));
    let expected: Vec<String> = expected.iter().map(|(_, id)| id.to_string()).collect();
    assert_eq!(
        seen, expected,
        "every owned net exactly once, newest-created first, across every boundary"
    );
    let unique: std::collections::HashSet<_> = seen.iter().collect();
    assert_eq!(unique.len(), seen.len(), "no net is served twice");
    // The per-row body is untouched by paging.
    assert!(pages[0][0]["liveSessionId"].is_null(), "not live");
    assert!(
        pages[0][0]["connections"].is_array(),
        "connections still ride each row"
    );
}

#[tokio::test]
async fn a_damaged_owned_net_inside_a_limited_page_shortens_the_page_and_the_walk_stays_total() {
    // The owned-nets half — the review found the favorites walk
    // fenced the split-before-skip ordering and this read did not: the only
    // damaged-row owned test read with no `?limit=`, and at the default limit
    // `has_more` is false on either side of the skip, so swapping `into_page`
    // and `resolve_definitions` in `list_owned_page` stayed green.
    //
    // Newest-first the read serves [4, 3(damaged), 2, 1, 0]; limit=3 puts the
    // damaged row SECOND on page 1 — inside it, where the over-fetch probe row
    // (2) would be promoted onto the page if the skip ran first, `has_more`
    // would read false, and row 0 would be lost with a null cursor.
    let app = test_app().await;
    let cookie = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;
    let seeded = seed_owned_nets_at(&app, &cookie, &[1_000, 2_000, 3_000, 4_000, 5_000]).await;
    let damaged = seeded[3].1;
    sqlx::query("DELETE FROM net_connections WHERE definition_id = $1")
        .bind(damaged)
        .execute(&app.pool)
        .await
        .expect("remove every connection row");

    let pages = walk_pages(&app, "/api/net-definitions", &cookie, 3).await;
    assert_eq!(pages.len(), 2, "two pages: {pages:?}");
    assert_eq!(
        pages[0].len(),
        2,
        "page 1 is short by exactly the damaged row it held"
    );
    assert_eq!(
        pages[1].len(),
        2,
        "page 2 is the remaining two healthy rows"
    );

    let seen: Vec<String> = pages
        .iter()
        .flatten()
        .map(|n| n["id"].as_str().expect("id").to_owned())
        .collect();
    let mut expected: Vec<(u64, Uuid)> = seeded
        .iter()
        .copied()
        .filter(|(_, id)| *id != damaged)
        .collect();
    expected.sort_by(|a, b| b.cmp(a));
    let expected: Vec<String> = expected.iter().map(|(_, id)| id.to_string()).collect();
    assert_eq!(
        seen, expected,
        "every healthy net exactly once, newest first, with the damaged one skipped"
    );
}

#[tokio::test]
async fn the_owned_nets_read_refuses_what_it_does_not_recognise() {
    // STRICT on forward rule. An unknown key, a
    // malformed limit, and a MALFORMED cursor (not `millis:uuid`) are each
    // `400 application/problem+json /errors/validation`; unauthenticated is `401`
    // before any parsing. `detail` is deliberately not pinned. A well-formed
    // cursor is honoured whatever read issued it — `PageQuery::cursor()` parses,
    // it does not verify — so "a cursor this server did not issue" is not a case
    // this test can express.
    let app = test_app().await;
    let cookie = sign_in_consent_callsign(&app, "strict@example.com", "w1aw").await;

    for uri in [
        "/api/net-definitions?limitt=5",
        "/api/net-definitions?limit=abc",
        "/api/net-definitions?cursor=garbage",
    ] {
        let request = Request::builder()
            .method("GET")
            .uri(uri)
            .header(header::COOKIE, &cookie)
            .body(Body::empty())
            .expect("build request");
        let response = app.router().oneshot(request).await.expect("route");
        assert_eq!(response.status(), StatusCode::BAD_REQUEST, "{uri} is a 400");
        assert_eq!(
            response
                .headers()
                .get(header::CONTENT_TYPE)
                .and_then(|v| v.to_str().ok()),
            Some("application/problem+json"),
            "{uri} media type"
        );
        let bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024)
            .await
            .expect("body");
        let problem: Value = serde_json::from_slice(&bytes).expect("problem body is JSON");
        assert_eq!(problem["type"], "/errors/validation", "{uri} slug");
        assert_eq!(problem["status"], 400, "{uri} status field");
    }

    let (status, problem) = send_json(
        app.router(),
        "GET",
        "/api/net-definitions?limitt=5",
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "401 before any parsing");
    assert_eq!(problem["type"], "/errors/unauthenticated");
}

#[tokio::test]
async fn a_multi_line_description_round_trips_as_lf_whatever_line_ending_the_client_sent() {
    // The value must survive the real write and come back
    // canonical through POST → GET against real Postgres, for all three
    // line-ending conventions a browser or paste buffer can produce.
    let app = test_app().await;
    let cookie = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;

    let mut canonical: Option<String> = None;
    for raw in [
        "Net control opens at 0100Z.\n\nCheck-ins welcome.",
        "Net control opens at 0100Z.\r\n\r\nCheck-ins welcome.",
        "Net control opens at 0100Z.\r\rCheck-ins welcome.",
    ] {
        let mut body = full_definition_json();
        body["description"] = json!(raw);
        let (status, created) = send_json(
            app.router(),
            "POST",
            "/api/net-definitions",
            Some(body),
            Some(&cookie),
        )
        .await;
        assert_eq!(
            status,
            StatusCode::CREATED,
            "a multi-line description must be accepted"
        );
        let id = created["id"].as_str().expect("id present").to_owned();

        let (status, got) = send_json(
            app.router(),
            "GET",
            &format!("/api/net-definitions/{id}"),
            None,
            Some(&cookie),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let stored = got["description"]
            .as_str()
            .expect("description present")
            .to_owned();
        assert!(
            stored.contains('\n'),
            "the paragraph break survives the write"
        );
        assert!(!stored.contains('\r'), "no CR survives the write");
        match &canonical {
            None => canonical = Some(stored),
            Some(first) => assert_eq!(
                &stored, first,
                "all three line-ending forms store one canonical value"
            ),
        }
    }
}

#[tokio::test]
async fn a_description_full_of_blank_lines_comes_back_as_one_blank_line() {
    // `"a" + "\n"×1998 + "b"` is exactly the
    // description bound, was once stored verbatim, and renders
    // ~1998 empty line boxes on the link-token-reachable public net page.
    // Proving it against real Postgres, POST → GET, because the collapse has to
    // survive the write to be worth anything.
    let app = test_app().await;
    let cookie = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;

    let mut body = full_definition_json();
    body["description"] = json!(format!("a{}b", "\n".repeat(1998)));
    let (status, created) = send_json(
        app.router(),
        "POST",
        "/api/net-definitions",
        Some(body),
        Some(&cookie),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::CREATED,
        "the hazard input is accepted, not rejected"
    );
    let id = created["id"].as_str().expect("id present").to_owned();

    let (status, got) = send_json(
        app.router(),
        "GET",
        &format!("/api/net-definitions/{id}"),
        None,
        Some(&cookie),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let stored = got["description"].as_str().expect("description present");
    assert!(
        !stored.contains("\n\n\n"),
        "no run of blank lines survives the write: {stored:?}"
    );
    assert_eq!(stored, "a\n\nb");
}

#[tokio::test]
async fn a_description_with_a_bidi_control_character_is_still_rejected_at_the_boundary() {
    // At the HTTP boundary: widening the field to prose does not disarm
    // the guard — the bidi reordering primitives stay a field-level 400 and
    // no row is written.
    let app = test_app().await;
    let cookie = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;

    let before = definition_count(&app).await;
    let mut body = full_definition_json();
    body["description"] = json!("Weekly NTS traffic\u{202E}flip");
    let (status, problem) = send_json(
        app.router(),
        "POST",
        "/api/net-definitions",
        Some(body),
        Some(&cookie),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(problem["type"], "/errors/net-definition-invalid");
    assert_eq!(
        definition_count(&app).await,
        before,
        "no row written for a bidi-control description"
    );
}

// --- Connections ----------------------------------------

/// Creates a net through the ordinary create path and returns `(id, cookie)`.
async fn create_definition(app: &TestApp, email: &str, callsign: &str) -> (String, String) {
    let cookie = sign_in_consent_callsign(app, email, callsign).await;
    let (status, body) = send_json(
        app.router(),
        "POST",
        "/api/net-definitions",
        Some(full_definition_json()),
        Some(&cookie),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    (body["id"].as_str().expect("id").to_owned(), cookie)
}

fn connections_write(expected_version: i64, connections: Value) -> Value {
    json!({
        "expectedDefinitionVersion": expected_version,
        "connections": connections,
    })
}

fn hf_connection(band: &str) -> Value {
    json!({
        "kind": "hf",
        "plannedFrequencyHz": 14_230_000,
        "band": band,
        "mode": "ssb",
    })
}

#[tokio::test]
async fn a_connection_list_write_holding_a_stale_version_is_refused_as_a_conflict() {
    let app = test_app().await;
    let (id, cookie) = create_definition(&app, "owner@example.com", "w1aw").await;

    // First write wins and moves the version on.
    let (status, body) = send_json(
        app.router(),
        "PUT",
        &format!("/api/net-definitions/{id}/connections"),
        Some(connections_write(1, json!([hf_connection("20m")]))),
        Some(&cookie),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["definitionVersion"], 2);

    // A second writer still holding version 1 is refused.
    let (status, body) = send_json(
        app.router(),
        "PUT",
        &format!("/api/net-definitions/{id}/connections"),
        Some(connections_write(1, json!([hf_connection("40m")]))),
        Some(&cookie),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(body["type"], "/errors/stale-version");

    // And it wrote nothing: the list still holds the winner's band.
    let (status, body) = send_json(
        app.router(),
        "GET",
        &format!("/api/net-definitions/{id}"),
        None,
        Some(&cookie),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["connections"][0]["band"], "20m");
}

#[tokio::test]
async fn a_connection_list_write_bumps_the_definition_version_by_one() {
    let app = test_app().await;
    let (id, cookie) = create_definition(&app, "owner@example.com", "w1aw").await;

    let (_, before) = send_json(
        app.router(),
        "GET",
        &format!("/api/net-definitions/{id}"),
        None,
        Some(&cookie),
    )
    .await;
    let version = before["definitionVersion"].as_i64().expect("version");

    let (status, body) = send_json(
        app.router(),
        "PUT",
        &format!("/api/net-definitions/{id}/connections"),
        Some(connections_write(
            version,
            json!([
                hf_connection("20m"),
                { "kind": "echolink", "node": "12345" },
            ]),
        )),
        Some(&cookie),
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["definitionVersion"], version + 1);
    assert_eq!(
        body["connections"].as_array().expect("connections").len(),
        2
    );
    assert_eq!(body["connections"][1]["kind"], "echolink");
    assert_eq!(body["connections"][1]["node"], "12345");
    assert_eq!(body["connections"][1]["position"], 1);
}

#[tokio::test]
async fn a_net_cannot_be_left_with_no_way_to_reach_it() {
    let app = test_app().await;
    let (id, cookie) = create_definition(&app, "owner@example.com", "w1aw").await;

    let (status, _) = send_json(
        app.router(),
        "PUT",
        &format!("/api/net-definitions/{id}/connections"),
        Some(connections_write(1, json!([]))),
        Some(&cookie),
    )
    .await;

    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn the_scalar_edit_keeps_its_last_write_wins_and_never_conflicts() {
    let app = test_app().await;
    let (id, cookie) = create_definition(&app, "owner@example.com", "w1aw").await;

    // Two full-replace edits in a row, neither carrying a version. Both win.
    for title in ["Monday Traffic Net", "Tuesday Traffic Net"] {
        let mut edit = scalar_definition_json();
        edit["title"] = json!(title);
        let (status, body) = send_json(
            app.router(),
            "PUT",
            &format!("/api/net-definitions/{id}"),
            Some(edit),
            Some(&cookie),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "the scalar edit takes no version");
        assert_eq!(body["title"], title);
    }
}

#[tokio::test]
async fn connections_reach_the_owner_body_the_owned_list_and_the_public_token_body() {
    let app = test_app().await;
    let (id, cookie) = create_definition(&app, "owner@example.com", "w1aw").await;

    let (status, owner) = send_json(
        app.router(),
        "GET",
        &format!("/api/net-definitions/{id}"),
        None,
        Some(&cookie),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let kinds: Vec<&str> = owner["connections"]
        .as_array()
        .expect("connections on the owner body")
        .iter()
        .map(|c| c["kind"].as_str().expect("kind"))
        .collect();
    assert_eq!(kinds, vec!["repeater", "echolink", "allstar", "dstar"]);
    let token = owner["linkToken"].as_str().expect("linkToken").to_owned();

    let (status, owned) = send_json(
        app.router(),
        "GET",
        "/api/net-definitions",
        None,
        Some(&cookie),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        owned["items"][0]["connections"]
            .as_array()
            .expect("connections on the owned-nets list")
            .len(),
        4
    );

    let (status, public) = send_json(
        app.router(),
        "GET",
        &format!("/api/net-definitions/by-token/{token}"),
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        public["connections"]
            .as_array()
            .expect("connections on the public token body")
            .len(),
        4
    );
    // The public projection still leaks no owner identity.
    assert!(public.get("ownerAccountIds").is_none());
}

#[tokio::test]
async fn a_stored_connection_the_domain_refuses_reads_back_as_other_and_the_net_still_loads() {
    let app = test_app().await;
    let (id, cookie) = create_definition(&app, "owner@example.com", "w1aw").await;
    let definition_id = uuid::Uuid::parse_str(&id).expect("uuid");

    // A shape the domain forbids, written past it: `echolink` with a band and
    // no node. Only a non-Rust writer can produce this — which is exactly the
    // case the read path must survive rather than 500 on.
    sqlx::query(
        "INSERT INTO net_connections (id, definition_id, position, kind, band)
         VALUES ($1, $2, 99, 'echolink', '20m')",
    )
    .bind(uuid::Uuid::now_v7())
    .bind(definition_id)
    .execute(&app.pool)
    .await
    .expect("write a domain-forbidden row");

    let (status, body) = send_json(
        app.router(),
        "GET",
        &format!("/api/net-definitions/{id}"),
        None,
        Some(&cookie),
    )
    .await;

    assert_eq!(
        status,
        StatusCode::OK,
        "one unreadable row must never make an owner's net unopenable"
    );
    let degraded = body["connections"]
        .as_array()
        .expect("connections")
        .iter()
        .find(|c| c["label"] == "unclassified-echolink")
        .expect("the forbidden row reads back as `other`");
    assert_eq!(degraded["kind"], "other");
    assert!(
        degraded["detail"].as_str().expect("detail").contains("20m"),
        "the original value must survive the degradation"
    );
}

// --- Connection-kind write coverage ----------------------------------------

/// Reads one definition's connections straight from storage, in owner order.
async fn stored_kinds(app: &TestApp, id: &str) -> Vec<String> {
    sqlx::query_scalar(
        "SELECT kind FROM net_connections WHERE definition_id = $1 ORDER BY position",
    )
    .bind(uuid::Uuid::parse_str(id).expect("uuid"))
    .fetch_all(&app.pool)
    .await
    .expect("read connections back")
}

/// The entries of a served connection list, exactly as served — what a client
/// doing a read-modify-write sends back, no key converted,
/// added or dropped. The helper that used to turn served Hz into decimal-MHz
/// strings here is gone with the asymmetry: the write now speaks the read's
/// vocabulary.
fn served_entries(connections: &Value) -> Vec<Value> {
    connections.as_array().expect("connections").clone()
}

fn repeater_connection(frequency_hz: i64) -> Value {
    json!({
        "kind": "repeater",
        "plannedFrequencyHz": frequency_hz,
        "band": "2m",
        "mode": "fm",
        "repeaterOffsetHz": -600_000,
        "toneMode": "ctcss",
        "toneValue": "100.0",
    })
}

#[tokio::test]
async fn a_two_repeater_net_keeps_both_repeaters_when_the_title_is_edited() {
    let app = test_app().await;
    let (id, cookie) = create_definition(&app, "owner@example.com", "w1aw").await;

    let (status, body) = send_json(
        app.router(),
        "PUT",
        &format!("/api/net-definitions/{id}/connections"),
        Some(connections_write(
            1,
            json!([
                repeater_connection(146_940_000),
                repeater_connection(147_060_000)
            ]),
        )),
        Some(&cookie),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let second_id = body["connections"][1]["id"]
        .as_str()
        .expect("the second repeater has an id")
        .to_owned();

    // The shipped form's own request shape: every scalar field, nothing about
    // connections. It is what an owner sends to change the title.
    let mut edit = scalar_definition_json();
    edit["title"] = json!("Tuesday Traffic Net");
    let (status, body) = send_json(
        app.router(),
        "PUT",
        &format!("/api/net-definitions/{id}"),
        Some(edit),
        Some(&cookie),
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        stored_kinds(&app, &id).await,
        vec!["repeater", "repeater"],
        "the scalar request shape says nothing about any connection, so it cannot delete one"
    );
    let ids: Vec<&str> = body["connections"]
        .as_array()
        .expect("connections")
        .iter()
        .map(|c| c["id"].as_str().expect("id"))
        .collect();
    assert!(
        ids.contains(&second_id.as_str()),
        "the second repeater keeps its identity across the edit"
    );
}

#[tokio::test]
async fn a_definition_carrying_a_reflector_can_write_its_own_connection_list_back() {
    let app = test_app().await;
    let (id, cookie) = create_definition(&app, "owner@example.com", "w1aw").await;
    let definition_id = uuid::Uuid::parse_str(&id).expect("uuid");

    // Machine-minted residue, the shape backfill left behind for a
    // reflector nothing could classify. The create body can no longer produce
    // it — an owner names the network as a kind — so it is planted the way it
    // arrived: past the domain.
    sqlx::query(
        "INSERT INTO net_connections (id, definition_id, position, kind, label, detail)
         VALUES ($1, $2, 4, 'other', 'unclassified-reflector', 'XLX307 B')",
    )
    .bind(uuid::Uuid::now_v7())
    .bind(definition_id)
    .execute(&app.pool)
    .await
    .expect("plant the residue row");

    // What a client that read this definition holds.
    let (status, before) = send_json(
        app.router(),
        "GET",
        &format!("/api/net-definitions/{id}"),
        None,
        Some(&cookie),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let version = before["definitionVersion"].as_i64().expect("version");
    let echoed = served_entries(&before["connections"]);

    let (status, body) = send_json(
        app.router(),
        "PUT",
        &format!("/api/net-definitions/{id}/connections"),
        Some(connections_write(version, json!(echoed))),
        Some(&cookie),
    )
    .await;

    assert_eq!(
        status,
        StatusCode::OK,
        "a client that reads a definition and writes its list straight back must not be \
         refused for echoing a label the server itself minted"
    );
    assert_eq!(
        body["connections"].as_array().expect("connections").len(),
        5
    );
    assert!(
        body["connections"]
            .as_array()
            .expect("connections")
            .iter()
            .any(|c| c["label"] == "unclassified-reflector"),
        "the echoed residue survives the round trip verbatim"
    );
}

#[tokio::test]
async fn an_owner_still_cannot_mint_a_reserved_label_on_a_connection_of_their_own() {
    let app = test_app().await;
    let (id, cookie) = create_definition(&app, "owner@example.com", "w1aw").await;

    let (status, _) = send_json(
        app.router(),
        "PUT",
        &format!("/api/net-definitions/{id}/connections"),
        Some(connections_write(
            1,
            json!([
                hf_connection("20m"),
                { "kind": "other", "label": "Unclassified Reflector", "detail": "mine" },
            ]),
        )),
        Some(&cookie),
    )
    .await;

    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "the reserved namespace separates machine residue from owner input, and case or a \
         space for the hyphen must not walk a label out of it"
    );
}

#[tokio::test]
async fn an_internet_only_net_has_no_band_anywhere_and_discovery_does_not_match_it_on_one() {
    // An internet-only net's `net_definitions.band` used to go stale, and
    // discovery had to not match on it. The flat column is gone, so what remains
    // is that discovery no longer reads it. With the column retired the first
    // half of that claim cannot be stated at all. The claim now is: an
    // internet-only net has no band ANYWHERE on the definition — not on the
    // owner body, not in the schema — and discovery still does not match it on
    // one. The test is kept rather than deleted because the discovery half is
    // still the fact worth pinning, and the schema half is what makes the
    // absence a fact rather than an omission.
    let app = test_app().await;
    let (id, cookie) = create_definition(&app, "owner@example.com", "w1aw").await;

    let (status, _) = send_json(
        app.router(),
        "PUT",
        &format!("/api/net-definitions/{id}/connections"),
        Some(connections_write(
            1,
            json!([{ "kind": "echolink", "node": "12345" }]),
        )),
        Some(&cookie),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    // The net must be genuinely discoverable, or the second half below passes
    // for the wrong reason — an unscheduled net is absent from `upcoming`
    // whatever the filter says.
    let (status, _) = send_json(
        app.router(),
        "PUT",
        &format!("/api/net-definitions/{id}/schedule"),
        Some(json!({
            "kind": "one-off",
            "timezone": "UTC",
            "oneOffStartAt": "2027-01-01T20:00:00Z",
        })),
        Some(&cookie),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    // No band on the OWNER BODY: the top-level `band`/`mode`/`plannedFrequencyHz`
    // keys that carried the stale `20m` are not `null`, they are ABSENT, and the
    // only band-bearing shape left is a connection, of which this net has none.
    let (status, owner_body) = send_json(
        app.router(),
        "GET",
        &format!("/api/net-definitions/{id}"),
        None,
        Some(&cookie),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    for key in ["band", "mode", "plannedFrequencyHz"] {
        assert!(
            owner_body.get(key).is_none(),
            "`{key}` is not a fact about a definition any more; it is a fact about a connection"
        );
    }
    assert!(
        owner_body["connections"]
            .as_array()
            .expect("connections")
            .iter()
            .all(|c| c["band"].is_null()),
        "an internet-only net has no connection with a band"
    );

    // No band in the SCHEMA. This is the assertion that turns the owner-body
    // absence from a serializer choice into a fact: there is no column left for
    // any reader to fall back to.
    //
    // The reader count this test carried is re-derived here, not carried
    // forward — its own previous comment said so. Measured 2026-09-02 on the
    // tree that dropped the columns. The count is a DATED SNAPSHOT — the same
    // command has recorded several different values as the tree moved — so
    // re-derive it rather than trusting it:
    //
    // grep -rnE 'def\.band|definition\.band|d\.band|row\.band' --include=*.rs \
    // backend/crates/netroll-app/src backend/crates/netroll-adapters/src \
    // backend/crates/netroll-domain/src | grep -v connection
    //
    // → 2 lines: one comment (`pg/discovery.rs`, explaining why nothing
    // COALESCEs back to a definition band) and ONE production reader,
    // `http/check_in_history.rs`, whose `row.band` is the nullable band of the
    // connection a check-in's `via` resolved to — a connection
    // fact, not a definition column. Zero readers of `net_definitions.band`
    // remain, which is the only number this test can now be about.
    let flat_columns: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM information_schema.columns
          WHERE table_name = 'net_definitions'
            AND column_name IN ('planned_frequency_hz', 'band', 'mode', 'repeater_offset_hz',
                                'tone_mode', 'tone_value', 'echolink_node', 'reflector',
                                'allstar_node')",
    )
    .fetch_one(&app.pool)
    .await
    .expect("count the flat connection columns");
    assert_eq!(
        flat_columns, 0,
        "the nine flat connection columns are gone from `net_definitions`"
    );

    // A CO-RESIDENT NET THAT REALLY IS ON 20m, and a schedule for it. Without
    // this the absence assertion below is one broken fixture away from passing
    // vacuously: an unscheduled net, a page that 500s into an empty array, or a
    // filter that matches nothing at all would each satisfy "not present" while
    // proving nothing. Its default connection set carries 20m, so `?band=20m`
    // genuinely has something to return.
    let (rf_id, rf_cookie) = create_definition(&app, "rf@example.com", "k1abc").await;
    let (status, _) = send_json(
        app.router(),
        "PUT",
        &format!("/api/net-definitions/{rf_id}/schedule"),
        Some(json!({
            "kind": "one-off",
            "timezone": "UTC",
            "oneOffStartAt": "2027-01-01T21:00:00Z",
        })),
        Some(&rf_cookie),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    // Discovery's predicate is an EXISTS over `net_connections`,
    // and there is no longer a definition column it COULD fall back to.
    let (status, body) =
        send_json(app.router(), "GET", "/api/discovery?band=20m", None, None).await;
    assert_eq!(status, StatusCode::OK);
    let upcoming_ids: Vec<&str> = body["upcoming"]
        .as_array()
        .expect("upcoming")
        .iter()
        .filter_map(|row| row["id"].as_str())
        .collect();
    assert!(
        upcoming_ids.contains(&rf_id.as_str()),
        "?band=20m still finds the net whose connection genuinely carries 20m — \
         without this the assertion below can go green on an empty page"
    );
    assert!(
        !upcoming_ids.contains(&id.as_str()),
        "an internet-only net is not a way to get on 20m"
    );

    // The second positive control, in the other direction: the internet-only net
    // IS discoverable, just not through a band it does not have. If it dropped
    // out of `upcoming` entirely — an unscheduled fixture, a visibility change —
    // the absence above would be true for a reason that has nothing to do with
    // the predicate this test exists to pin.
    let (status, unfiltered) = send_json(app.router(), "GET", "/api/discovery", None, None).await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        unfiltered["upcoming"]
            .as_array()
            .expect("upcoming")
            .iter()
            .any(|row| row["id"].as_str() == Some(id.as_str())),
        "the internet-only net is discoverable; only the band filter excludes it"
    );
}

#[tokio::test]
async fn a_connection_list_naming_one_id_twice_is_a_400_not_a_500() {
    let app = test_app().await;
    let (id, cookie) = create_definition(&app, "owner@example.com", "w1aw").await;

    let (_, before) = send_json(
        app.router(),
        "GET",
        &format!("/api/net-definitions/{id}"),
        None,
        Some(&cookie),
    )
    .await;
    let existing_id = before["connections"][0]["id"].as_str().expect("id");
    let mut first = hf_connection("20m");
    first["id"] = json!(existing_id);
    let mut second = hf_connection("40m");
    second["id"] = json!(existing_id);

    let (status, _) = send_json(
        app.router(),
        "PUT",
        &format!("/api/net-definitions/{id}/connections"),
        Some(connections_write(1, json!([first, second]))),
        Some(&cookie),
    )
    .await;

    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn a_connection_id_belonging_to_another_account_is_neither_a_500_nor_an_oracle() {
    let app = test_app().await;
    let (mine, my_cookie) = create_definition(&app, "owner@example.com", "w1aw").await;
    let (theirs, their_cookie) = create_definition(&app, "other@example.com", "k1abc").await;

    let (_, theirs_body) = send_json(
        app.router(),
        "GET",
        &format!("/api/net-definitions/{theirs}"),
        None,
        Some(&their_cookie),
    )
    .await;
    let foreign_id = theirs_body["connections"][0]["id"]
        .as_str()
        .expect("id")
        .to_owned();

    let mut entry = hf_connection("20m");
    entry["id"] = json!(foreign_id);
    let (status, body) = send_json(
        app.router(),
        "PUT",
        &format!("/api/net-definitions/{mine}/connections"),
        Some(connections_write(1, json!([entry]))),
        Some(&my_cookie),
    )
    .await;

    assert_eq!(
        status,
        StatusCode::OK,
        "an id is only ever an echo of a value this definition was given; an id it does not \
         hold names a new connection and must not collide in storage"
    );
    assert_ne!(
        body["connections"][0]["id"], foreign_id,
        "and the response must not confirm that the foreign id exists"
    );
}

#[tokio::test]
async fn a_definition_whose_connection_rows_are_gone_refuses_its_own_read_and_is_skipped_by_the_list()
 {
    // A definition with zero `net_connections` rows used to reconstruct its set
    // from the flat mirror columns; the mirror is gone, so what remains is
    // damage — the backfill and the at-least-one-connection constructor mean
    // no such row should exist. A read that names
    // the damaged net refuses, because a 200 claiming the net is reachable by
    // nothing would be a lie about that net; a read that LISTS nets skips it and
    // logs, because one damaged net must not empty an owner's whole page — nor,
    // through favorites, a stranger's.
    let app = test_app().await;
    let (damaged, cookie) = create_definition(&app, "owner@example.com", "w1aw").await;
    let (healthy, _) = create_net(&app, &cookie, "listed").await;
    let damaged_id = uuid::Uuid::parse_str(&damaged).expect("uuid");

    sqlx::query("DELETE FROM net_connections WHERE definition_id = $1")
        .bind(damaged_id)
        .execute(&app.pool)
        .await
        .expect("remove every connection row");

    let (status, body) = send_json(
        app.router(),
        "GET",
        &format!("/api/net-definitions/{damaged}"),
        None,
        Some(&cookie),
    )
    .await;
    assert!(
        status.is_server_error(),
        "the damaged net's own read is a refusal, never a net with no way on — got {status} {body}"
    );

    let (status, body) = send_json(
        app.router(),
        "GET",
        "/api/net-definitions",
        None,
        Some(&cookie),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "one damaged net must not take the owned-nets list down — got {body}"
    );
    let ids: Vec<&str> = body["items"]
        .as_array()
        .expect("items array")
        .iter()
        .map(|n| n["id"].as_str().expect("id"))
        .collect();
    assert_eq!(
        ids,
        vec![healthy.as_str()],
        "the healthy net is listed and the damaged one is skipped, not rendered empty"
    );
}

#[tokio::test]
async fn a_scalar_write_carrying_a_connection_key_is_refused_rather_than_ignored() {
    // `NetDefinitionRequest` has no `deny_unknown_fields`, so a `connections`
    // key — or a
    // retired flat key such as `band` — on the scalar route was dropped with a
    // 200 and the caller believed its edit had landed. A key naming a
    // CONNECTION fact is now a 400 under the definition's own slug, on POST and
    // PUT alike, and nothing is written. Other unread keys stay ignored —
    // `edit_increments_version_and_ignores_client_supplied_version` pins that.
    let app = test_app().await;
    let (id, cookie) = create_definition(&app, "owner@example.com", "w1aw").await;

    let mut with_list = scalar_definition_json();
    with_list["title"] = json!("Edited Title");
    with_list["connections"] = json!([{ "kind": "echolink", "node": "12345" }]);
    let mut with_flat_key = scalar_definition_json();
    with_flat_key["band"] = json!("40m");

    for body in [with_list, with_flat_key] {
        let (status, problem) = send_json(
            app.router(),
            "PUT",
            &format!("/api/net-definitions/{id}"),
            Some(body),
            Some(&cookie),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "got {problem}");
        assert_eq!(problem["type"], "/errors/net-definition-invalid");
    }

    let (status, current) = send_json(
        app.router(),
        "GET",
        &format!("/api/net-definitions/{id}"),
        None,
        Some(&cookie),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        current["definitionVersion"], 1,
        "a refused write bumps nothing"
    );
    assert_ne!(
        current["title"], "Edited Title",
        "a refused write lands nothing"
    );
    assert_eq!(
        current["connections"]
            .as_array()
            .expect("connections")
            .len(),
        4,
        "the connection set the net was born with is untouched"
    );

    let before = definition_count(&app).await;
    let mut both_shapes = full_definition_json();
    both_shapes["band"] = json!("40m");
    let (status, problem) = send_json(
        app.router(),
        "POST",
        "/api/net-definitions",
        Some(both_shapes),
        Some(&cookie),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "got {problem}");
    assert_eq!(problem["type"], "/errors/net-definition-invalid");
    assert_eq!(
        definition_count(&app).await,
        before,
        "a refused create writes no row"
    );
}

/// The Hz spelling is the one the API teaches, so a client that puts a
/// connection fact at the top level of the
/// scalar route most plausibly spells it `plannedFrequencyHz` — and that is the
/// spelling the screen never listed: `200`, version bumped, frequency
/// gone (the silently-dropped-meaning class). Both Hz spellings are refused
/// like their flat predecessors, naming the key, and nothing is written.
#[tokio::test]
async fn a_scalar_write_carrying_the_hz_spelling_of_a_connection_fact_is_refused() {
    let app = test_app().await;
    let (id, cookie) = create_definition(&app, "owner@example.com", "w1aw").await;

    for (key, value) in [
        ("plannedFrequencyHz", json!(14_230_000)),
        ("repeaterOffsetHz", json!(-600_000)),
    ] {
        let mut body = scalar_definition_json();
        body["title"] = json!("Edited Title");
        body[key] = value;
        let (status, problem) = send_json(
            app.router(),
            "PUT",
            &format!("/api/net-definitions/{id}"),
            Some(body),
            Some(&cookie),
        )
        .await;
        assert_eq!(
            status,
            StatusCode::BAD_REQUEST,
            "`{key}` is a connection fact and the scalar route must refuse it, not drop it: {problem}"
        );
        assert_eq!(problem["type"], "/errors/net-definition-invalid");
        assert!(
            names_key(problem["detail"].as_str().unwrap_or_default(), key),
            "the refusal names the offending key `{key}`: {problem}"
        );
    }

    let (status, current) = send_json(
        app.router(),
        "GET",
        &format!("/api/net-definitions/{id}"),
        None,
        Some(&cookie),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        current["definitionVersion"], 1,
        "a refused write bumps nothing"
    );
    assert_ne!(
        current["title"], "Edited Title",
        "a refused write lands nothing"
    );
}

#[tokio::test]
async fn a_connection_carried_through_a_scalar_edit_keeps_the_instant_it_was_created() {
    let app = test_app().await;
    let (id, cookie) = create_definition(&app, "owner@example.com", "w1aw").await;
    let definition_id = uuid::Uuid::parse_str(&id).expect("uuid");

    let created: Vec<(uuid::Uuid, chrono::DateTime<chrono::Utc>)> =
        sqlx::query_as("SELECT id, created_at FROM net_connections WHERE definition_id = $1")
            .bind(definition_id)
            .fetch_all(&app.pool)
            .await
            .expect("read created_at");

    let mut edit = scalar_definition_json();
    edit["title"] = json!("Tuesday Traffic Net");
    let (status, _) = send_json(
        app.router(),
        "PUT",
        &format!("/api/net-definitions/{id}"),
        Some(edit),
        Some(&cookie),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let after: Vec<(uuid::Uuid, chrono::DateTime<chrono::Utc>)> =
        sqlx::query_as("SELECT id, created_at FROM net_connections WHERE definition_id = $1")
            .bind(definition_id)
            .fetch_all(&app.pool)
            .await
            .expect("read created_at");

    for (id, created_at) in &created {
        let (_, after_at) = after
            .iter()
            .find(|(other, _)| other == id)
            .expect("the connection survived the edit");
        assert_eq!(
            after_at, created_at,
            "a replace is a delete and a re-insert; resetting created_at on a row nobody \
             changed makes the column a lie"
        );
    }
}

// --- The owner-facing connection editor's contract -------------

/// Whether a problem `detail` names `key` as a whole wire key. A key name is a
/// contract the client can act on, so a test may pin it; the sentence around
/// it is copy, so a test may not. Token-bounded on both sides because the
/// retired `plannedFrequency` is a prefix of its replacement
/// `plannedFrequencyHz` — a bare `contains` would count one as naming the
/// other.
fn names_key(detail: &str, key: &str) -> bool {
    let is_boundary = |c: char| !c.is_ascii_alphanumeric();
    detail.match_indices(key).any(|(at, _)| {
        let before = detail[..at].chars().next_back().is_none_or(is_boundary);
        let after = detail[at + key.len()..]
            .chars()
            .next()
            .is_none_or(is_boundary);
        before && after
    })
}

async fn read_connections(app: &TestApp, id: &str, cookie: &str) -> Value {
    let (status, body) = send_json(
        app.router(),
        "GET",
        &format!("/api/net-definitions/{id}"),
        None,
        Some(cookie),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    body["connections"].clone()
}

#[tokio::test]
async fn a_refused_entry_names_its_own_index_on_the_problem() {
    let app = test_app().await;
    let (id, cookie) = create_definition(&app, "owner@example.com", "w1aw").await;

    let (status, body) = send_json(
        app.router(),
        "PUT",
        &format!("/api/net-definitions/{id}/connections"),
        Some(connections_write(
            1,
            // The second entry names `hf` and carries none of `hf`'s properties.
            json!([hf_connection("20m"), { "kind": "hf" }]),
        )),
        Some(&cookie),
    )
    .await;

    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(
        body["type"], "/errors/net-definition-invalid",
        "no new problem type is minted for a per-entry failure"
    );
    assert_eq!(
        body["connectionIndex"], 1,
        "an owner editing a list of ten is told which entry to fix, in a field a client can \
         read without parsing prose"
    );
}

#[tokio::test]
async fn a_set_level_refusal_carries_no_connection_index() {
    let app = test_app().await;
    let (id, cookie) = create_definition(&app, "owner@example.com", "w1aw").await;

    let (status, body) = send_json(
        app.router(),
        "PUT",
        &format!("/api/net-definitions/{id}/connections"),
        Some(connections_write(1, json!([]))),
        Some(&cookie),
    )
    .await;

    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(
        body.get("connectionIndex").is_none(),
        "an empty list is a fact about the SET, and pinning it to a row would point an owner \
         at a connection that does not exist"
    );
}

#[tokio::test]
async fn a_list_written_straight_back_unchanged_stores_byte_identical_connections() {
    let app = test_app().await;
    let (id, cookie) = create_definition(&app, "owner@example.com", "w1aw").await;

    let before = read_connections(&app, &id, &cookie).await;
    // VERBATIM — no key added, dropped or renamed. The write body speaks the
    // read's vocabulary, so a client doing a read-modify-write no
    // longer converts anything. Position 0 is a repeater carrying both a
    // frequency and an offset, which is what makes this echo prove something.
    let echoed = served_entries(&before);

    let (status, problem) = send_json(
        app.router(),
        "PUT",
        &format!("/api/net-definitions/{id}/connections"),
        Some(connections_write(1, json!(echoed))),
        Some(&cookie),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "got {problem}");

    // The served lists are compared WHOLE, not projected onto a key list: the
    // echo is verbatim and ids survive a resolved echo, so nothing on the wire
    // is expected to differ — and a projection would drop a key a later story
    // adds from both sides before the comparison ever saw it.
    let after = read_connections(&app, &id, &cookie).await;
    assert_eq!(
        after, before,
        "the backfill's own values must survive a save the owner made no edit in — same ids, \
         same order, same properties"
    );
}

/// Exactly one spelling of each connection property exists on
/// the wire. The retired decimal-MHz spellings are REFUSED with the entry's
/// index, never silently dropped: a `plannedFrequency` string on an entry that
/// carries no `plannedFrequencyHz` would be a missing-property refusal with no
/// screen at all, so the discriminating case is a `repeaterOffset` string on a
/// repeater whose frequency is otherwise valid — without the screen, that write
/// is a `200` and the offset the owner typed is gone.
#[tokio::test]
async fn a_retired_decimal_mhz_spelling_on_an_entry_is_refused_with_its_index() {
    let app = test_app().await;
    let (id, cookie) = create_definition(&app, "owner@example.com", "w1aw").await;
    let before = read_connections(&app, &id, &cookie).await;

    let retired_frequency = json!([{
        "kind": "hf",
        "plannedFrequency": "14.230",
        "band": "20m",
        "mode": "ssb",
    }]);
    let retired_offset = json!([
        { "kind": "echolink", "node": "12345" },
        {
            "kind": "repeater",
            "plannedFrequencyHz": 146_940_000,
            "band": "2m",
            "mode": "fm",
            "repeaterOffset": "-0.600",
        },
    ]);

    for (spelling, replacement, body, index) in [
        (
            "plannedFrequency",
            "plannedFrequencyHz",
            retired_frequency,
            0,
        ),
        ("repeaterOffset", "repeaterOffsetHz", retired_offset, 1),
    ] {
        let (status, problem) = send_json(
            app.router(),
            "PUT",
            &format!("/api/net-definitions/{id}/connections"),
            Some(connections_write(1, body)),
            Some(&cookie),
        )
        .await;
        assert_eq!(
            status,
            StatusCode::BAD_REQUEST,
            "`{spelling}` is retired from the write body and must be refused, not ignored: {problem}"
        );
        assert_eq!(problem["type"], "/errors/net-definition-invalid");
        assert_eq!(
            problem["connectionIndex"], index,
            "the refusal names the entry carrying the retired key"
        );
        // The pair table exists so the refusal can say where the value went —
        // the one thing `deny_unknown_fields` could not. Both halves of the
        // pair are pinned as key names; the sentence around them is not.
        let detail = problem["detail"].as_str().unwrap_or_default();
        assert!(
            names_key(detail, spelling),
            "the refusal names the retired key `{spelling}`: {detail:?}"
        );
        assert!(
            names_key(detail, replacement),
            "the refusal names the replacement key `{replacement}`: {detail:?}"
        );
    }

    let (status, current) = send_json(
        app.router(),
        "GET",
        &format!("/api/net-definitions/{id}"),
        None,
        Some(&cookie),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        current["definitionVersion"], 1,
        "a refused write bumps nothing"
    );
    assert_eq!(
        current["connections"], before,
        "a refused write lands nothing"
    );
}

/// One entry carrying BOTH retired spellings is
/// refused ONCE, naming both — the `refuse_connection_keys` posture on the
/// scalar route — rather than one key per round trip. `connectionIndex` stays
/// singular, so the collection is within one entry, never across entries.
#[tokio::test]
async fn an_entry_carrying_both_retired_spellings_is_refused_once_naming_both() {
    let app = test_app().await;
    let (id, cookie) = create_definition(&app, "owner@example.com", "w1aw").await;

    let (status, problem) = send_json(
        app.router(),
        "PUT",
        &format!("/api/net-definitions/{id}/connections"),
        Some(connections_write(
            1,
            json!([{
                "kind": "repeater",
                "plannedFrequency": "146.940",
                "band": "2m",
                "mode": "fm",
                "repeaterOffset": "-0.600",
            }]),
        )),
        Some(&cookie),
    )
    .await;

    assert_eq!(status, StatusCode::BAD_REQUEST, "got {problem}");
    assert_eq!(problem["type"], "/errors/net-definition-invalid");
    assert_eq!(problem["connectionIndex"], 0);
    let detail = problem["detail"].as_str().unwrap_or_default();
    for key in [
        "plannedFrequency",
        "plannedFrequencyHz",
        "repeaterOffset",
        "repeaterOffsetHz",
    ] {
        assert!(
            names_key(detail, key),
            "one refusal names every retired key on the entry and its replacement — `{key}` is \
             missing from {detail:?}"
        );
    }
}

/// The set-level cap decides an over-long list, and
/// it must be reported BEFORE any per-entry wire screen: a client sending 500
/// entries with a retired key at index 0 would otherwise be told about entry 0,
/// then entry 1, and never reach the refusal that actually governs the request.
/// The refusal is the domain's `TooMany`, so it carries no `connectionIndex` —
/// the discriminator between "too many" and "entry 0 carries a retired key".
#[tokio::test]
async fn an_over_cap_list_is_refused_for_its_size_before_any_entry_is_screened() {
    let app = test_app().await;
    let (id, cookie) = create_definition(&app, "owner@example.com", "w1aw").await;

    let mut entries: Vec<Value> = (0..=MAX_CONNECTIONS)
        .map(|i| json!({ "kind": "echolink", "node": i.to_string() }))
        .collect();
    entries[0] = json!({
        "kind": "hf",
        "plannedFrequency": "14.230",
        "band": "20m",
        "mode": "ssb",
    });

    let (status, problem) = send_json(
        app.router(),
        "PUT",
        &format!("/api/net-definitions/{id}/connections"),
        Some(connections_write(1, json!(entries))),
        Some(&cookie),
    )
    .await;

    assert_eq!(status, StatusCode::BAD_REQUEST, "got {problem}");
    assert_eq!(problem["type"], "/errors/net-definition-invalid");
    assert!(
        problem.get("connectionIndex").is_none(),
        "the cap is a fact about the SET and answers first; a `connectionIndex` here means a \
         per-entry screen ran ahead of it: {problem}"
    );
    let detail = problem["detail"].as_str().unwrap_or_default();
    assert!(
        !names_key(detail, "plannedFrequency"),
        "the over-cap refusal is about the list's size, not entry 0's spelling: {detail:?}"
    );

    let (_, current) = send_json(
        app.router(),
        "GET",
        &format!("/api/net-definitions/{id}"),
        None,
        Some(&cookie),
    )
    .await;
    assert_eq!(current["definitionVersion"], 1, "nothing was written");
}

/// The read serves `position` on every entry, so an unmodified
/// echo carries it and the write must say what it does with it: a `position`
/// that agrees with the entry's array index is accepted; one that disagrees is
/// refused with that entry's index and nothing is written (a `200` that moved
/// nothing is the silently-dropped-meaning class this API refuses); an
/// absent `position` means the array order is the owner's order, as before.
///
/// Internet-only entries on purpose: the position rule has nothing to do with
/// frequency, and keeping RF out of the fixture keeps this RED about `position`
/// alone.
#[tokio::test]
async fn a_position_that_disagrees_with_the_entrys_place_is_refused_and_a_matching_one_accepted() {
    let app = test_app().await;
    let (id, cookie) = create_definition(&app, "owner@example.com", "w1aw").await;

    // Values swapped, array order not: entry 0 claims position 1.
    let (status, problem) = send_json(
        app.router(),
        "PUT",
        &format!("/api/net-definitions/{id}/connections"),
        Some(connections_write(
            1,
            json!([
                { "kind": "echolink", "node": "12345", "position": 1 },
                { "kind": "allstar", "node": "54321", "position": 0 },
            ]),
        )),
        Some(&cookie),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "a `position` that disagrees with the entry's place is refused: {problem}"
    );
    assert_eq!(problem["type"], "/errors/net-definition-invalid");
    assert_eq!(problem["connectionIndex"], 0);

    let (_, current) = send_json(
        app.router(),
        "GET",
        &format!("/api/net-definitions/{id}"),
        None,
        Some(&cookie),
    )
    .await;
    assert_eq!(current["definitionVersion"], 1, "nothing was written");
    assert_eq!(
        current["connections"]
            .as_array()
            .expect("connections")
            .len(),
        4,
        "the set the net was born with is untouched"
    );

    // Present and equal to the array index: accepted.
    let (status, body) = send_json(
        app.router(),
        "PUT",
        &format!("/api/net-definitions/{id}/connections"),
        Some(connections_write(
            1,
            json!([
                { "kind": "echolink", "node": "12345", "position": 0 },
                { "kind": "allstar", "node": "54321", "position": 1 },
            ]),
        )),
        Some(&cookie),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "got {body}");
    assert_eq!(body["connections"][0]["kind"], "echolink");
    assert_eq!(body["connections"][0]["position"], 0);
    assert_eq!(body["connections"][1]["kind"], "allstar");
    assert_eq!(body["connections"][1]["position"], 1);

    // Absent on every entry: the array order is the owner's order, as today.
    let (status, body) = send_json(
        app.router(),
        "PUT",
        &format!("/api/net-definitions/{id}/connections"),
        Some(connections_write(
            2,
            json!([
                { "kind": "allstar", "node": "54321" },
                { "kind": "echolink", "node": "12345" },
            ]),
        )),
        Some(&cookie),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "got {body}");
    assert_eq!(body["connections"][0]["kind"], "allstar");
    assert_eq!(body["connections"][0]["position"], 0);
    assert_eq!(body["connections"][1]["kind"], "echolink");
    assert_eq!(body["connections"][1]["position"], 1);
}

#[tokio::test]
async fn editing_one_row_leaves_every_other_rows_hz_exactly_where_it_was() {
    let app = test_app().await;
    let (id, cookie) = create_definition(&app, "owner@example.com", "w1aw").await;

    let (status, _) = send_json(
        app.router(),
        "PUT",
        &format!("/api/net-definitions/{id}/connections"),
        Some(connections_write(
            1,
            json!([
                repeater_connection(146_940_000),
                hf_connection("20m"),
                { "kind": "other", "label": "Wires-X", "detail": "room 21493" },
            ]),
        )),
        Some(&cookie),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let before = read_connections(&app, &id, &cookie).await;
    let version = 2;
    let mut echoed = served_entries(&before);
    // One string on one row — the edit an owner actually makes.
    echoed[2]["detail"] = json!("room 21493, Wednesdays");

    let (status, _) = send_json(
        app.router(),
        "PUT",
        &format!("/api/net-definitions/{id}/connections"),
        Some(connections_write(version, json!(echoed))),
        Some(&cookie),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let after = read_connections(&app, &id, &cookie).await;
    for row in [0usize, 1] {
        assert_eq!(
            after[row]["plannedFrequencyHz"], before[row]["plannedFrequencyHz"],
            "a row the owner never touched must come back on the same integer Hz"
        );
        assert_eq!(
            after[row]["repeaterOffsetHz"], before[row]["repeaterOffsetHz"],
            "a signed offset is the value a decimal-MHz round trip is most likely to move"
        );
    }
    assert_eq!(after[2]["detail"], json!("room 21493, Wednesdays"));
}

#[tokio::test]
async fn a_title_edit_through_the_scalar_put_leaves_an_internet_only_nets_connections_untouched() {
    // The scalar PUT once could not say "this net has no RF way": frequency,
    // band and mode were required on it
    // and NOT NULL underneath, so a title edit re-minted one connection per
    // populated flat slot, and the editor had to send the connection
    // list LAST so its full replace deleted the transient row. This test pinned
    // that ordering. The columns are gone and the scalar request carries no
    // connection fact at all, so the claim worth pinning is the one the ordering
    // existed to approximate: a scalar edit does not touch the connection set —
    // no transient row, in any order, with no second write to clean up after it.
    let app = test_app().await;
    let (id, cookie) = create_definition(&app, "owner@example.com", "w1aw").await;

    let (status, _) = send_json(
        app.router(),
        "PUT",
        &format!("/api/net-definitions/{id}/connections"),
        Some(connections_write(
            1,
            json!([{ "kind": "echolink", "node": "12345" }]),
        )),
        Some(&cookie),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let before = read_connections(&app, &id, &cookie).await;

    let mut edit = scalar_definition_json();
    edit["title"] = json!("Tuesday EchoLink Net");
    let (status, scalar) = send_json(
        app.router(),
        "PUT",
        &format!("/api/net-definitions/{id}"),
        Some(edit),
        Some(&cookie),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{scalar}");
    assert_eq!(scalar["title"], "Tuesday EchoLink Net");

    assert_eq!(
        stored_kinds(&app, &id).await,
        vec!["echolink"],
        "no transient RF row: the scalar shape cannot name a connection, so it cannot mint one"
    );
    assert_eq!(
        scalar["connections"], before,
        "the body the edit returns carries the list the net already had — same ids, same \
         order, same values — with no re-read of a mirror to reconcile against"
    );
    assert_eq!(read_connections(&app, &id, &cookie).await, before);
}

// --- Every connection kind, written and read back ---------------------------

/// Every kind the domain names, written and read back.
///
/// The suite built `hf`, `repeater`, `echolink`, `dstar` and `other` and none
/// of `dmr`, `allstar`, `ysf` or `urf` — the same shape of omission that let
/// a kind's defects ship green. A kind nothing writes is a kind nothing checks.
#[tokio::test]
async fn every_named_kind_survives_a_write_and_reads_back_with_its_own_properties() {
    let app = test_app().await;
    let (id, cookie) = create_definition(&app, "owner@example.com", "w1aw").await;

    let (status, _) = send_json(
        app.router(),
        "PUT",
        &format!("/api/net-definitions/{id}/connections"),
        Some(connections_write(
            1,
            json!([
                hf_connection("20m"),
                repeater_connection(146_940_000),
                { "kind": "echolink", "node": "12345" },
                { "kind": "allstar", "node": "56789" },
                { "kind": "dmr", "talkgroup": "31000", "network": "Brandmeister" },
                { "kind": "dstar", "reflector": "REF030 C" },
                { "kind": "ysf", "reflector": "US-Ohio" },
                { "kind": "urf", "reflector": "URF001 B" },
                { "kind": "other", "label": "Moon bounce", "detail": "EME sked" },
            ]),
        )),
        Some(&cookie),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    assert_eq!(
        stored_kinds(&app, &id).await,
        vec![
            "hf", "repeater", "echolink", "allstar", "dmr", "dstar", "ysf", "urf", "other",
        ],
        "every kind the domain names must survive a write in the owner's order"
    );

    let (status, body) = send_json(
        app.router(),
        "GET",
        &format!("/api/net-definitions/{id}"),
        None,
        Some(&cookie),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let served = body["connections"].as_array().expect("connections").clone();

    // Each kind carries ITS OWN property and no other kind's.
    assert_eq!(served[3]["node"], "56789");
    assert!(served[3]["talkgroup"].is_null());
    assert_eq!(served[4]["talkgroup"], "31000");
    assert_eq!(served[4]["network"], "Brandmeister");
    assert!(served[4]["node"].is_null());
    assert_eq!(served[6]["reflector"], "US-Ohio");
    assert!(served[6]["node"].is_null());
    assert_eq!(served[7]["reflector"], "URF001 B");
    assert!(served[7]["talkgroup"].is_null());

    // And the whole list round-trips verbatim, so a client that GETs and PUTs
    // back loses none of these four.
    let echoed = served_entries(&body["connections"]);
    let (status, _) = send_json(
        app.router(),
        "PUT",
        &format!("/api/net-definitions/{id}/connections"),
        Some(connections_write(2, json!(echoed))),
        Some(&cookie),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let (_, after) = send_json(
        app.router(),
        "GET",
        &format!("/api/net-definitions/{id}"),
        None,
        Some(&cookie),
    )
    .await;
    assert_eq!(
        after["connections"], body["connections"],
        "a straight echo of every named kind must store byte-identical connections"
    );
}

// --- A DMR connection names its network ------------------------

/// Reads the stored `network` column of one definition's connections, in owner
/// order, straight from storage.
///
/// Storage rather than the served body: `StoragePayload` and the `INSERT` are
/// the two sites the compiler cannot see, and a body-only assertion passes
/// while every write stores NULL.
async fn stored_networks(app: &TestApp, id: &str) -> Vec<Option<String>> {
    sqlx::query_scalar(
        "SELECT network FROM net_connections WHERE definition_id = $1 ORDER BY position",
    )
    .bind(uuid::Uuid::parse_str(id).expect("uuid"))
    .fetch_all(&app.pool)
    .await
    .expect("read stored networks back")
}

#[tokio::test]
async fn a_dmr_connection_with_no_network_reads_back_carrying_the_key_as_null() {
    let app = test_app().await;
    let (id, cookie) = create_definition(&app, "owner@example.com", "w1aw").await;

    let (status, _) = send_json(
        app.router(),
        "PUT",
        &format!("/api/net-definitions/{id}/connections"),
        Some(connections_write(
            1,
            json!([{ "kind": "dmr", "talkgroup": "3100" }]),
        )),
        Some(&cookie),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let served = read_connections(&app, &id, &cookie).await;
    let entry = served[0].as_object().expect("one served connection");
    assert_eq!(
        entry["kind"], "dmr",
        "a network-less DMR row is still a DMR"
    );
    assert_eq!(entry["talkgroup"], "3100");
    // Key PRESENCE, never `is_null()`: `Value` indexing answers `Null` for a
    // missing key too, so `is_null()` alone passes before the field exists.
    assert!(
        entry.contains_key("network"),
        "the served body must carry `network` even when nothing was recorded"
    );
    assert!(entry["network"].is_null());
    assert_eq!(stored_networks(&app, &id).await, vec![None]);
}

#[tokio::test]
async fn a_stored_dmr_connection_with_no_network_survives_a_straight_echo() {
    let app = test_app().await;
    let (id, cookie) = create_definition(&app, "owner@example.com", "w1aw").await;

    let (status, _) = send_json(
        app.router(),
        "PUT",
        &format!("/api/net-definitions/{id}/connections"),
        Some(connections_write(
            1,
            json!([{ "kind": "dmr", "talkgroup": "3100" }]),
        )),
        Some(&cookie),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let served = read_connections(&app, &id, &cookie).await;
    let echoed = served_entries(&served);
    let (status, body) = send_json(
        app.router(),
        "PUT",
        &format!("/api/net-definitions/{id}/connections"),
        Some(connections_write(2, json!(echoed))),
        Some(&cookie),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "a DMR net with no recorded network must stay saveable: {body}"
    );
    assert_eq!(
        stored_networks(&app, &id).await,
        vec![None],
        "the echo must not invent a network nobody chose"
    );
}

#[tokio::test]
async fn a_dmr_network_outside_the_suggestions_round_trips_byte_identical() {
    let app = test_app().await;
    let (id, cookie) = create_definition(&app, "owner@example.com", "w1aw").await;

    // A private Hytera XPT system: no list will ever enumerate it, and the
    // casing and the hyphens are the operator's own.
    let written = "Hytera-XPT-Local";
    let (status, _) = send_json(
        app.router(),
        "PUT",
        &format!("/api/net-definitions/{id}/connections"),
        Some(connections_write(
            1,
            json!([{ "kind": "dmr", "talkgroup": "9", "network": written }]),
        )),
        Some(&cookie),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let served = read_connections(&app, &id, &cookie).await;
    assert_eq!(
        served[0]["network"], written,
        "the network is kept as the operator wrote it — no case-fold, no collapse"
    );
    assert_eq!(
        stored_networks(&app, &id).await,
        vec![Some(written.to_owned())],
        "and the value reached storage, not merely the response"
    );
}

#[tokio::test]
async fn a_dmr_network_survives_an_ordinary_title_edit_through_the_scalar_put() {
    let app = test_app().await;
    let (id, cookie) = create_definition(&app, "owner@example.com", "w1aw").await;

    let (status, _) = send_json(
        app.router(),
        "PUT",
        &format!("/api/net-definitions/{id}/connections"),
        Some(connections_write(
            1,
            json!([
                hf_connection("20m"),
                { "kind": "dmr", "talkgroup": "3100", "network": "Brandmeister" },
            ]),
        )),
        Some(&cookie),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    // The whole rationale is the ORDINARY TITLE EDIT, and that is the scalar
    // `PUT /api/net-definitions/{id}`, not the `/connections` sub-resource every
    // other network test here drives. The scalar request cannot name a
    // connection at all; silence must not be read as a delete or a blank.
    let mut edit = scalar_definition_json();
    edit["title"] = json!("Tuesday Traffic Net");
    let (status, body) = send_json(
        app.router(),
        "PUT",
        &format!("/api/net-definitions/{id}"),
        Some(edit),
        Some(&cookie),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "the title edit must succeed: {body}"
    );
    assert_eq!(body["title"], "Tuesday Traffic Net");

    let kinds = stored_kinds(&app, &id).await;
    let dmr_at = kinds
        .iter()
        .position(|kind| kind == "dmr")
        .expect("the DMR connection must survive a scalar edit that never mentioned it");
    assert_eq!(
        stored_networks(&app, &id).await[dmr_at],
        Some("Brandmeister".to_owned()),
        "a title edit must not blank a network the flat request cannot even express"
    );

    // And the owner reads it back, not merely the column.
    let served = read_connections(&app, &id, &cookie).await;
    let dmr = served
        .as_array()
        .expect("connections")
        .iter()
        .find(|entry| entry["kind"] == "dmr")
        .expect("the served list still carries the DMR connection");
    assert_eq!(dmr["talkgroup"], "3100");
    assert_eq!(dmr["network"], "Brandmeister");
}

#[tokio::test]
async fn a_network_sent_on_a_non_dmr_kind_is_dropped_rather_than_stored() {
    let app = test_app().await;
    let (id, cookie) = create_definition(&app, "owner@example.com", "w1aw").await;

    // `network` is a known wire key on every entry, so an EchoLink connection
    // carrying one is accepted and ignored — the same posture the neighbouring
    // `talkgroup` assertion pins for AllStar. A kind gets ONLY the properties
    // it has; the wrong implementation stores the stray value and serves it.
    let (status, body) = send_json(
        app.router(),
        "PUT",
        &format!("/api/net-definitions/{id}/connections"),
        Some(connections_write(
            1,
            json!([{ "kind": "echolink", "node": "12345", "network": "Brandmeister" }]),
        )),
        Some(&cookie),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "a property the kind does not have is ignored, not refused: {body}"
    );

    let served = read_connections(&app, &id, &cookie).await;
    let entry = served[0].as_object().expect("one served connection");
    assert_eq!(entry["kind"], "echolink");
    assert_eq!(entry["node"], "12345");
    // Key PRESENCE first: `Value` indexing answers `Null` for a missing key
    // too, so `is_null()` alone would pass even if the key were gone entirely.
    assert!(
        entry.contains_key("network"),
        "every connection body carries the key; only DMR ever carries a value"
    );
    assert!(
        entry["network"].is_null(),
        "an EchoLink connection must not serve a network it cannot have"
    );
    assert_eq!(
        stored_networks(&app, &id).await,
        vec![None],
        "and nothing reached the column — a body-only check would pass while storage held it"
    );
}

// --- The create path carries its first connection ---------

/// The scalar fields of a definition and nothing else — no flat connection
/// property, no connection list. The body the scalar `PUT` speaks once the nine
/// flat columns are gone.
fn scalar_definition_json() -> Value {
    json!({
        "title": "Sunday Traffic Net",
        "description": "Weekly NTS traffic",
        "country": "USA",
        "state": "CT",
        "grid": "fn31pr",
        "netCategory": "traffic",
        "netType": "open",
        "expectedDuration": "90"
    })
}

/// A create body: the scalar fields plus the connection list the net is born
/// with.
fn create_json(connections: Value) -> Value {
    let mut body = scalar_definition_json();
    body["connections"] = connections;
    body
}

#[tokio::test]
async fn a_created_net_holds_exactly_the_connections_its_create_body_named() {
    let app = test_app().await;
    let cookie = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;

    let (status, body) = send_json(
        app.router(),
        "POST",
        "/api/net-definitions",
        Some(create_json(json!([
            { "kind": "echolink", "node": "12345" },
            hf_connection("40m"),
        ]))),
        Some(&cookie),
    )
    .await;

    assert_eq!(status, StatusCode::CREATED, "{body}");
    let served: Vec<&str> = body["connections"]
        .as_array()
        .expect("connections")
        .iter()
        .map(|c| c["kind"].as_str().expect("kind"))
        .collect();
    assert_eq!(
        served,
        vec!["echolink", "hf"],
        "the list the create body named, in its order — nothing minted from any other source"
    );
    let id = body["id"].as_str().expect("id");
    assert_eq!(stored_kinds(&app, id).await, vec!["echolink", "hf"]);
    assert_eq!(body["connections"][0]["position"], 0);
    assert!(
        body["connections"][0]["band"].is_null(),
        "an EchoLink way has no band, and leading with it is the owner's call"
    );
}

#[tokio::test]
async fn a_create_body_carrying_only_the_old_flat_keys_is_refused_not_silently_accepted() {
    let app = test_app().await;
    let cookie = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;

    // The older create shape: every connection fact as a flat key and no
    // `connections` list. The request has no `deny_unknown_fields`, so the
    // failure this guards against is not a 400 — it is a 201 whose connection
    // set nobody asked for, or a refusal that names nothing.
    let mut legacy = scalar_definition_json();
    legacy["plannedFrequency"] = json!("14.230");
    legacy["band"] = json!("20m");
    legacy["mode"] = json!("ssb");
    let (status, body) = send_json(
        app.router(),
        "POST",
        "/api/net-definitions",
        Some(legacy),
        Some(&cookie),
    )
    .await;

    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(body["type"], "/errors/net-definition-invalid");
    assert!(
        body["detail"].as_str().is_some_and(|d| !d.is_empty()),
        "the refusal states a reason"
    );
    assert_eq!(
        definition_count(&app).await,
        0,
        "a refused create writes no row"
    );
}

/// retired-spelling screen, pinned on the CREATE route. Every other
/// pin drives `PUT …/connections`; this one proves the create path is guarded
/// too, rather than relying on both routes happening to share one function.
/// The discriminating fixture again: a valid `plannedFrequencyHz` beside a
/// retired `repeaterOffset`, which no missing-property check would catch.
#[tokio::test]
async fn a_create_body_whose_connection_carries_a_retired_spelling_is_refused_with_its_index() {
    let app = test_app().await;
    let cookie = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;

    let (status, problem) = send_json(
        app.router(),
        "POST",
        "/api/net-definitions",
        Some(create_json(json!([
            { "kind": "echolink", "node": "12345" },
            {
                "kind": "repeater",
                "plannedFrequencyHz": 146_940_000,
                "band": "2m",
                "mode": "fm",
                "repeaterOffset": "-0.600",
            },
        ]))),
        Some(&cookie),
    )
    .await;

    assert_eq!(status, StatusCode::BAD_REQUEST, "{problem}");
    assert_eq!(problem["type"], "/errors/net-definition-invalid");
    assert_eq!(
        problem["connectionIndex"], 1,
        "the refusal names the entry carrying the retired key"
    );
    assert_eq!(
        definition_count(&app).await,
        0,
        "a refused create writes no row"
    );
}

/// `position` rule, pinned on the CREATE route for the same reason
/// as its retired-spelling sibling above.
#[tokio::test]
async fn a_create_body_whose_connection_position_disagrees_is_refused_with_its_index() {
    let app = test_app().await;
    let cookie = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;

    let (status, problem) = send_json(
        app.router(),
        "POST",
        "/api/net-definitions",
        Some(create_json(json!([
            { "kind": "echolink", "node": "12345", "position": 1 },
            { "kind": "allstar", "node": "54321", "position": 0 },
        ]))),
        Some(&cookie),
    )
    .await;

    assert_eq!(status, StatusCode::BAD_REQUEST, "{problem}");
    assert_eq!(problem["type"], "/errors/net-definition-invalid");
    assert_eq!(
        problem["connectionIndex"], 0,
        "the refusal names the first entry whose `position` disagrees with its place"
    );
    assert_eq!(
        definition_count(&app).await,
        0,
        "a refused create writes no row"
    );
}
