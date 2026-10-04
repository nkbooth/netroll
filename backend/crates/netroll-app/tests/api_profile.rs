// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
//! Full-loop API tests for profile management: real router,
//! real Postgres (testcontainers), capturing fake mailer. Asserts status
//! codes, problem+json `type` slugs, and body values — never message prose
//! (house TDD rule).

use std::sync::{Arc, Mutex};

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use netroll_app::http::{AppState, api_router};
use netroll_domain::consent::CURRENT_TERMS_VERSION;
use netroll_domain::ports::{BoxFuture, MailError, Mailer};
use netroll_domain::profile::gravatar_url;
use serde_json::{Value, json};
use sqlx::postgres::PgPoolOptions;
use testcontainers_modules::postgres::Postgres;
use testcontainers_modules::testcontainers::ContainerAsync;
use testcontainers_modules::testcontainers::runners::AsyncRunner;
use tower::ServiceExt;

/// Mailer that records sends instead of speaking SMTP.
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
                .expect("mailer lock")
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
                .expect("mailer lock")
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
                .expect("mailer lock")
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
    let container = Postgres::default()
        .start()
        .await
        .expect("start postgres container");
    let host = container.get_host().await.expect("resolve container host");
    let port = container
        .get_host_port_ipv4(5432)
        .await
        .expect("resolve mapped postgres port");
    let pool = PgPoolOptions::new()
        .connect(&format!(
            "postgres://postgres:postgres@{host}:{port}/postgres"
        ))
        .await
        .expect("connect to containerized postgres");
    sqlx::migrate!("../../migrations")
        .run(&pool)
        .await
        .expect("run migrations");

    let mailer = Arc::new(CapturingMailer::default());
    let state = AppState::new(pool, mailer.clone(), "http://localhost:5173".into());
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
        .expect("read body");
    let json = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).expect("body is JSON")
    };
    (status, json)
}

/// Signs a fresh email all the way in and records consent (profile
/// management is gated on `ConsentedAccount`). Returns the session cookie
/// pair.
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
        .expect("link carries a token")
        .1
        .to_owned();

    let request = Request::builder()
        .method("POST")
        .uri("/api/sessions")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(json!({ "token": token }).to_string()))
        .expect("build request");
    let response = app.router().oneshot(request).await.expect("route request");
    assert_eq!(response.status(), StatusCode::CREATED);
    let cookie = response
        .headers()
        .get(header::SET_COOKIE)
        .expect("201 sets the session cookie")
        .to_str()
        .expect("cookie header is ascii")
        .split(';')
        .next()
        .expect("cookie pair")
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

fn full_profile_json() -> Value {
    json!({
        "displayName": "Maria",
        "location": "Hartford, CT",
        "grid": "fn31pr",
        "avatarUrl": "https://example.com/me.png"
    })
}

#[tokio::test]
async fn putting_a_full_profile_persists_normalizes_grid_and_me_agrees() {
    let app = test_app().await;
    let cookie = sign_in_and_consent(&app, "op@example.com").await;

    let (status, body) = send_json(
        app.router(),
        "PUT",
        "/api/accounts/me/profile",
        Some(full_profile_json()),
        Some(&cookie),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["displayName"], "Maria");
    assert_eq!(body["location"], "Hartford, CT");
    assert_eq!(body["grid"], "FN31pr", "grid comes back canonicalized");
    assert_eq!(body["avatarUrl"], "https://example.com/me.png");

    let (status, me) =
        send_json(app.router(), "GET", "/api/accounts/me", None, Some(&cookie)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(me["displayName"], "Maria");
    assert_eq!(me["location"], "Hartford, CT");
    assert_eq!(me["grid"], "FN31pr");
    assert_eq!(me["avatarUrl"], "https://example.com/me.png");
}

#[tokio::test]
async fn gravatar_url_defaults_from_the_stored_email_and_survives_a_custom_avatar() {
    let app = test_app().await;
    let cookie = sign_in_and_consent(&app, "op@example.com").await;

    // Before any avatar is set: avatarUrl null, gravatarUrl derived from
    // the stored email — pinned to the known vector for op@example.com.
    let (status, me) =
        send_json(app.router(), "GET", "/api/accounts/me", None, Some(&cookie)).await;
    assert_eq!(status, StatusCode::OK);
    assert!(me["avatarUrl"].is_null());
    assert_eq!(me["gravatarUrl"], gravatar_url("op@example.com"));
    assert_eq!(
        me["gravatarUrl"],
        "https://gravatar.com/avatar/3d1832bf4b7de99f5b04a00c14b543740c80792908d458daa4de697a6d536034?d=mp"
    );

    // A custom avatarUrl rides alongside — gravatarUrl is unchanged, so
    // effective avatar stays the client's one-liner: avatarUrl ?? gravatarUrl.
    let (status, body) = send_json(
        app.router(),
        "PUT",
        "/api/accounts/me/profile",
        Some(json!({ "avatarUrl": "https://example.com/me.png" })),
        Some(&cookie),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["avatarUrl"], "https://example.com/me.png");
    assert_eq!(body["gravatarUrl"], gravatar_url("op@example.com"));
}

#[tokio::test]
async fn invalid_grid_is_a_400_grid_invalid_with_reason_specific_detail() {
    let app = test_app().await;
    let cookie = sign_in_and_consent(&app, "op@example.com").await;

    let (status_a, problem_a) = send_json(
        app.router(),
        "PUT",
        "/api/accounts/me/profile",
        Some(json!({ "grid": "FN3" })), // bad length
        Some(&cookie),
    )
    .await;
    assert_eq!(status_a, StatusCode::BAD_REQUEST);
    assert_eq!(problem_a["type"], "/errors/grid-invalid");
    let detail_a = problem_a["detail"]
        .as_str()
        .expect("detail is a non-empty string")
        .to_owned();
    assert!(!detail_a.is_empty());

    let (status_b, problem_b) = send_json(
        app.router(),
        "PUT",
        "/api/accounts/me/profile",
        Some(json!({ "grid": "SS11" })), // field pair beyond A–R
        Some(&cookie),
    )
    .await;
    assert_eq!(status_b, StatusCode::BAD_REQUEST);
    assert_eq!(problem_b["type"], "/errors/grid-invalid");
    let detail_b = problem_b["detail"]
        .as_str()
        .expect("detail is a non-empty string")
        .to_owned();

    assert_ne!(
        detail_a, detail_b,
        "distinct grid malformations must carry distinct detail text"
    );
}

#[tokio::test]
async fn invalid_display_name_and_avatar_url_are_400_validation_problems() {
    let app = test_app().await;
    let cookie = sign_in_and_consent(&app, "op@example.com").await;

    let (status, problem) = send_json(
        app.router(),
        "PUT",
        "/api/accounts/me/profile",
        Some(json!({ "displayName": "x".repeat(65) })),
        Some(&cookie),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(problem["type"], "/errors/validation");
    assert!(
        problem["detail"].as_str().is_some_and(|d| !d.is_empty()),
        "detail names the failing field"
    );

    let (status, problem) = send_json(
        app.router(),
        "PUT",
        "/api/accounts/me/profile",
        Some(json!({ "avatarUrl": "http://example.com/me.png" })),
        Some(&cookie),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(problem["type"], "/errors/validation");
    assert!(problem["detail"].as_str().is_some_and(|d| !d.is_empty()));
}

#[tokio::test]
async fn a_too_long_and_a_control_character_display_name_carry_distinct_detail() {
    // `parse_display_name` already distinguishes TooLong
    // from IllegalCharacter; the adapter discarded it with `.map_err(|_| ..)`
    // and answered by reciting BOTH rules. This asserts WHICH FAULT reached the
    // wire — behaviour, not wording — so the sentence stays free to change.
    let app = test_app().await;
    let cookie = sign_in_and_consent(&app, "op@example.com").await;

    let mut details = Vec::new();
    for bad in [json!("x".repeat(65)), json!("Mar\u{0085}ia")] {
        let (status, problem) = send_json(
            app.router(),
            "PUT",
            "/api/accounts/me/profile",
            Some(json!({ "displayName": bad })),
            Some(&cookie),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(problem["type"], "/errors/validation", "slug is unchanged");
        details.push(problem["detail"].as_str().expect("detail").to_owned());
    }
    assert_ne!(
        details[0], details[1],
        "a length failure and a character failure are different faults and must not collapse into one answer"
    );

    // The same distinction on the OTHER field the same guard serves.
    let mut location_details = Vec::new();
    for bad in [json!("x".repeat(129)), json!("Hart\u{000B}ford")] {
        let (status, problem) = send_json(
            app.router(),
            "PUT",
            "/api/accounts/me/profile",
            Some(json!({ "location": bad })),
            Some(&cookie),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(problem["type"], "/errors/validation");
        location_details.push(problem["detail"].as_str().expect("detail").to_owned());
    }
    assert_ne!(location_details[0], location_details[1]);
    assert_ne!(
        details[0], location_details[0],
        "two fields with different ceilings must not answer identically"
    );
}

#[tokio::test]
async fn null_omitted_and_empty_all_clear_a_previously_set_field() {
    let app = test_app().await;
    let cookie = sign_in_and_consent(&app, "op@example.com").await;

    for clearing_body in [
        json!({ "displayName": null, "location": "Hartford, CT" }),
        json!({ "location": "Hartford, CT" }), // key omitted
        json!({ "displayName": "", "location": "Hartford, CT" }), // empty string
    ] {
        let (status, _) = send_json(
            app.router(),
            "PUT",
            "/api/accounts/me/profile",
            Some(full_profile_json()),
            Some(&cookie),
        )
        .await;
        assert_eq!(status, StatusCode::OK);

        let (status, body) = send_json(
            app.router(),
            "PUT",
            "/api/accounts/me/profile",
            Some(clearing_body.clone()),
            Some(&cookie),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert!(
            body["displayName"].is_null(),
            "displayName must clear via {clearing_body}"
        );
        assert_eq!(body["location"], "Hartford, CT");
        assert!(body["grid"].is_null(), "omitted grid clears too");

        let (_, me) = send_json(app.router(), "GET", "/api/accounts/me", None, Some(&cookie)).await;
        assert!(me["displayName"].is_null(), "a subsequent /me agrees");
    }
}

/// `parse_profile_field` is shared by all four fields, but the test above
/// only ever clears `displayName` (and `grid` via bare omission) — this
/// closes the gap by asserting `location`, `grid`, and `avatarUrl` each
/// clear via an explicit `null` AND via `""`, not just by being left out.
#[tokio::test]
async fn null_and_empty_clear_location_grid_and_avatar_url_explicitly() {
    let app = test_app().await;
    let cookie = sign_in_and_consent(&app, "op@example.com").await;

    for (field, clearing_value) in [
        ("location", Value::Null),
        ("location", json!("")),
        ("grid", Value::Null),
        ("grid", json!("")),
        ("avatarUrl", Value::Null),
        ("avatarUrl", json!("")),
    ] {
        let (status, _) = send_json(
            app.router(),
            "PUT",
            "/api/accounts/me/profile",
            Some(full_profile_json()),
            Some(&cookie),
        )
        .await;
        assert_eq!(status, StatusCode::OK);

        let mut clearing_body = full_profile_json();
        clearing_body[field] = clearing_value.clone();
        let (status, body) = send_json(
            app.router(),
            "PUT",
            "/api/accounts/me/profile",
            Some(clearing_body),
            Some(&cookie),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert!(
            body[field].is_null(),
            "{field} must clear via {clearing_value} (got {body})"
        );

        let (_, me) = send_json(app.router(), "GET", "/api/accounts/me", None, Some(&cookie)).await;
        assert!(
            me[field].is_null(),
            "{field} clearing via {clearing_value} survives a subsequent /me"
        );
    }
}

#[tokio::test]
async fn unauthenticated_put_is_401() {
    let app = test_app().await;

    let (status, problem) = send_json(
        app.router(),
        "PUT",
        "/api/accounts/me/profile",
        Some(full_profile_json()),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(problem["type"], "/errors/unauthenticated");
}

#[tokio::test]
async fn signed_in_but_unconsented_account_is_403_consent_required() {
    let app = test_app().await;

    // Sign in WITHOUT recording consent.
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
        .expect("link carries a token")
        .1
        .to_owned();
    let request = Request::builder()
        .method("POST")
        .uri("/api/sessions")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(json!({ "token": token }).to_string()))
        .expect("build request");
    let response = app.router().oneshot(request).await.expect("route request");
    assert_eq!(response.status(), StatusCode::CREATED);
    let cookie = response
        .headers()
        .get(header::SET_COOKIE)
        .expect("201 sets the session cookie")
        .to_str()
        .expect("cookie header is ascii")
        .split(';')
        .next()
        .expect("cookie pair")
        .to_owned();

    let (status, problem) = send_json(
        app.router(),
        "PUT",
        "/api/accounts/me/profile",
        Some(full_profile_json()),
        Some(&cookie),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(problem["type"], "/errors/consent-required");
}

#[tokio::test]
async fn profile_put_never_touches_the_callsign_and_me_reads_null_before_set() {
    let app = test_app().await;
    let cookie = sign_in_and_consent(&app, "op@example.com").await;

    // Before anything is set: the four new fields read null on /me and all
    // already-shipped fields are still present.
    let (status, me) =
        send_json(app.router(), "GET", "/api/accounts/me", None, Some(&cookie)).await;
    assert_eq!(status, StatusCode::OK);
    for field in ["displayName", "location", "grid", "avatarUrl"] {
        assert!(me[field].is_null(), "{field} must read null before set");
    }
    assert_eq!(me["email"], "op@example.com");
    assert!(me["emailVerifiedAt"].is_string());
    assert_eq!(me["consentRequired"], false);
    assert!(me["requiredTermsVersion"].is_string());
    assert!(me["callsign"].is_null());

    // Reserve a callsign, then PUT a profile: callsign must be untouched
    // and the callsign route itself unchanged.
    let (status, _) = send_json(
        app.router(),
        "PUT",
        "/api/accounts/me/callsign",
        Some(json!({ "callsign": "w1aw" })),
        Some(&cookie),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let (status, body) = send_json(
        app.router(),
        "PUT",
        "/api/accounts/me/profile",
        Some(full_profile_json()),
        Some(&cookie),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        body["callsign"], "W1AW",
        "PUT profile never writes callsign"
    );

    let (status, body) = send_json(
        app.router(),
        "PUT",
        "/api/accounts/me/callsign",
        Some(json!({ "callsign": "k1abc" })),
        Some(&cookie),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "callsign route behavior unchanged");
    assert_eq!(body["callsign"], "K1ABC");
    assert_eq!(
        body["displayName"], "Maria",
        "and the callsign route's body carries the profile fields too"
    );
}
