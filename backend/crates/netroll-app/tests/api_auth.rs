// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
//! Full-loop API tests for magic-link auth: real router, real
//! Postgres (testcontainers), capturing fake mailer. Asserts status codes,
//! problem+json `type` slugs, cookie attributes, and storage effects — never
//! message strings.

use std::sync::{Arc, Mutex};

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use netroll_app::bot_mitigation::BotMitigation;
use netroll_app::config::resolve_bot_mitigation_secret;
use netroll_app::http::{AppState, api_router};
use netroll_domain::auth::hash_token;
use netroll_domain::bot_mitigation::issue_form_token;
use netroll_domain::ports::{BoxFuture, MailError, Mailer};
use serde_json::{Value, json};
use sqlx::PgPool;
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

    /// Number of mails sent — the side-effect witness the bot-mitigation tests
    /// assert on (a silently-dropped signup sends none).
    fn sent_count(&self) -> usize {
        self.sent.lock().expect("mailer lock").len()
    }
}

/// The bot-mitigation secret the enabled test instances key on. The trimmed
/// bytes (`b"test-bot-mitigation-secret"`) are what [`issue_form_token`] must
/// sign with. Long enough to clear the config-level minimum-secret-length
/// floor.
const BOT_SECRET_RAW: &str = "test-bot-mitigation-secret";
const BOT_SECRET_BYTES: &[u8] = b"test-bot-mitigation-secret";

/// Current wall-clock millis, matching the `SystemClock` the handler reads.
fn wall_now_millis() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("after epoch")
        .as_millis() as u64
}

/// A form token backdated far enough to clear the min-fill floor while staying
/// well within the TTL — i.e. a plausible human fill.
fn human_form_token() -> String {
    issue_form_token(BOT_SECRET_BYTES, wall_now_millis() - 5_000)
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
    /// The container's pool, so a test can assert storage effects directly
    /// (a capped request must write NO `magic_link_tokens` row) and
    /// build a SECOND `AppState` over the same database.
    pool: PgPool,
}

impl TestApp {
    fn router(&self) -> Router {
        api_router(self.state.clone())
    }
}

async fn test_app() -> TestApp {
    test_app_configured(|state| state).await
}

/// Builds a test app whose `AppState` is passed through `customize` before use —
/// the seam the bot-mitigation tests use to install an enabled control.
async fn test_app_configured(customize: impl FnOnce(AppState) -> AppState) -> TestApp {
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
    let state = customize(AppState::new(
        pool.clone(),
        mailer.clone(),
        "http://localhost:5173".into(),
    ));
    TestApp {
        _container: container,
        state,
        mailer,
        pool,
    }
}

/// A test app with bot mitigation ENABLED under [`BOT_SECRET_RAW`].
async fn bot_mitigated_app() -> TestApp {
    test_app_configured(|state| {
        let secret = resolve_bot_mitigation_secret(Some(BOT_SECRET_RAW.into()))
            .expect("a long-enough secret resolves")
            .expect("a set secret enables mitigation");
        state.with_bot_mitigation(BotMitigation::with_secret(secret))
    })
    .await
}

async fn send_json(
    router: Router,
    method: &str,
    uri: &str,
    body: Option<Value>,
    cookie: Option<&str>,
) -> (StatusCode, Option<String>, Value) {
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
    let set_cookie = response
        .headers()
        .get(header::SET_COOKIE)
        .map(|v| v.to_str().expect("cookie header is ascii").to_owned());
    let bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024)
        .await
        .expect("read body");
    let json = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).expect("body is JSON")
    };
    (status, set_cookie, json)
}

fn token_from_link(link: &str) -> String {
    link.split_once("token=")
        .expect("link carries a token query param")
        .1
        .to_owned()
}

fn cookie_pair(set_cookie: &str) -> String {
    set_cookie
        .split(';')
        .next()
        .expect("cookie pair before attributes")
        .to_owned()
}

#[tokio::test]
async fn requesting_a_link_returns_202_with_no_enumeration_oracle() {
    let app = test_app().await;

    let (status_unknown, _, body_unknown) = send_json(
        app.router(),
        "POST",
        "/api/magic-links",
        Some(json!({"email": "unknown@example.com"})),
        None,
    )
    .await;
    assert_eq!(status_unknown, StatusCode::ACCEPTED);

    // Register the first email fully, then request another link for it: a
    // known account must be indistinguishable from an unknown one.
    let token = token_from_link(&app.mailer.last_link());
    let (created, _, _) = send_json(
        app.router(),
        "POST",
        "/api/sessions",
        Some(json!({"token": token})),
        None,
    )
    .await;
    assert_eq!(created, StatusCode::CREATED);

    let (status_known, _, body_known) = send_json(
        app.router(),
        "POST",
        "/api/magic-links",
        Some(json!({"email": "unknown@example.com"})),
        None,
    )
    .await;
    assert_eq!(status_known, StatusCode::ACCEPTED);
    assert_eq!(
        body_unknown, body_known,
        "response must be identical whether or not the email has an account"
    );
}

#[tokio::test]
async fn invalid_email_shape_is_a_400_validation_problem() {
    let app = test_app().await;

    // "a@b@example.com" and the overlong address pass a naive contains-@ check but
    // are not deliverable; they must be refused up front, not after the
    // rate-limit slot and token row are spent.
    let overlong = format!("{}@example.com", "x".repeat(255));
    for bad in [
        "not-an-address",
        "a@b@example.com",
        "a@b.",
        overlong.as_str(),
    ] {
        let (status, _, body) = send_json(
            app.router(),
            "POST",
            "/api/magic-links",
            Some(json!({"email": bad})),
            None,
        )
        .await;

        assert_eq!(status, StatusCode::BAD_REQUEST, "email {bad:?}");
        assert_eq!(body["type"], "/errors/validation", "email {bad:?}");
        assert_eq!(body["status"], 400);
    }
}

#[tokio::test]
async fn mail_delivery_failure_still_returns_202_and_no_oracle() {
    /// Mailer that always fails, modeling an SMTP relay rejecting `RCPT TO`.
    struct FailingMailer;
    impl Mailer for FailingMailer {
        fn send_magic_link<'a>(
            &'a self,
            _to: &'a str,
            _link: &'a str,
        ) -> BoxFuture<'a, Result<(), MailError>> {
            Box::pin(async { Err(MailError("mailbox unavailable: op@example.com".into())) })
        }

        fn send_email_change<'a>(
            &'a self,
            _to: &'a str,
            _link: &'a str,
        ) -> BoxFuture<'a, Result<(), MailError>> {
            Box::pin(async { Err(MailError("mailbox unavailable: op@example.com".into())) })
        }

        fn send_email_change_notice<'a>(
            &'a self,
            _to: &'a str,
            _new_email: &'a str,
        ) -> BoxFuture<'a, Result<(), MailError>> {
            Box::pin(async { Err(MailError("mailbox unavailable: op@example.com".into())) })
        }
    }

    let app = test_app().await;
    let state = AppState {
        mailer: Arc::new(FailingMailer),
        ..app.state.clone()
    };

    // A delivery failure must be indistinguishable from success on the wire:
    // a 5xx here is exactly the enumeration/probing channel to avoid.
    let (status, _, _) = send_json(
        api_router(state),
        "POST",
        "/api/magic-links",
        Some(json!({"email": "op@example.com"})),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED);
}

#[tokio::test]
async fn malformed_request_bodies_get_the_problem_json_contract() {
    let app = test_app().await;

    // (body, content-type): broken JSON, a missing field, and a JSON body
    // sent without the JSON content type.
    let cases: Vec<(&str, Option<&str>)> = vec![
        ("{not json", Some("application/json")),
        (r#"{"unexpected": true}"#, Some("application/json")),
        (r#"{"email": "op@example.com"}"#, None),
    ];

    for (body, content_type) in cases {
        let mut builder = Request::builder().method("POST").uri("/api/magic-links");
        if let Some(ct) = content_type {
            builder = builder.header(header::CONTENT_TYPE, ct);
        }
        let request = builder
            .body(Body::from(body.to_owned()))
            .expect("build request");

        let response = app.router().oneshot(request).await.expect("route request");
        let status = response.status();
        let content_type_header = response
            .headers()
            .get(header::CONTENT_TYPE)
            .map(|v| v.to_str().expect("ascii header").to_owned());
        let bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024)
            .await
            .expect("read body");
        let problem: Value = serde_json::from_slice(&bytes)
            .unwrap_or_else(|_| panic!("body must be problem json for {body:?}"));

        assert_eq!(status, StatusCode::BAD_REQUEST, "body {body:?}");
        assert_eq!(
            content_type_header.as_deref(),
            Some("application/problem+json"),
            "body {body:?}"
        );
        assert_eq!(problem["type"], "/errors/validation", "body {body:?}");
    }
}

#[tokio::test]
async fn full_loop_sign_in_me_second_use_rejected_sign_out() {
    let app = test_app().await;

    // Request a link; the mail goes to the normalized address.
    let (status, _, _) = send_json(
        app.router(),
        "POST",
        "/api/magic-links",
        Some(json!({"email": "  Op@Example.COM "})),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED);
    let link = app.mailer.last_link();
    assert!(link.starts_with("http://localhost:5173/auth/verify?token="));

    // Consume: 201, session cookie with the pinned attributes, verified email.
    let token = token_from_link(&link);
    let (status, set_cookie, body) = send_json(
        app.router(),
        "POST",
        "/api/sessions",
        Some(json!({"token": token})),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    let set_cookie = set_cookie.expect("201 sets the session cookie");
    assert!(set_cookie.starts_with("__Host-netroll-session="));
    for attribute in ["Secure", "HttpOnly", "SameSite=Lax", "Path=/"] {
        assert!(
            set_cookie.contains(attribute),
            "cookie must carry {attribute}: {set_cookie}"
        );
    }
    assert_eq!(body["email"], "op@example.com");
    assert!(
        !body["emailVerifiedAt"].is_null(),
        "consuming the link verifies the email"
    );

    // Authenticated /me round-trips the account.
    let cookie = cookie_pair(&set_cookie);
    let (status, _, me) =
        send_json(app.router(), "GET", "/api/accounts/me", None, Some(&cookie)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(me["id"], body["id"]);
    assert_eq!(me["email"], "op@example.com");

    // Second use of the same link is rejected as consumed.
    let (status, _, problem) = send_json(
        app.router(),
        "POST",
        "/api/sessions",
        Some(json!({"token": token})),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(problem["type"], "/errors/magic-link-consumed");

    // Sign out: 204, session revoked server-side, cookie cleared.
    let (status, clear_cookie, _) = send_json(
        app.router(),
        "DELETE",
        "/api/sessions/current",
        None,
        Some(&cookie),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let clear_cookie = clear_cookie.expect("sign-out clears the cookie");
    assert!(clear_cookie.contains("Max-Age=0"), "{clear_cookie}");

    // The same cookie no longer authenticates — revocation is server-side.
    let (status, _, problem) =
        send_json(app.router(), "GET", "/api/accounts/me", None, Some(&cookie)).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(problem["type"], "/errors/unauthenticated");
}

#[tokio::test]
async fn expired_link_is_a_401_expired_problem() {
    let app = test_app().await;

    // Seed an already-expired token directly; requesting one over HTTP can't
    // produce this state without clock control.
    let raw = b"expired-raw-token-need-32-bytes!";
    let hash = hash_token(raw);
    let past = 1_000; // epoch + 1s, long past
    app.state
        .magic_links
        .issue("op@example.com", hash, past)
        .await
        .expect("seed expired token");

    use base64::Engine;
    let token = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(raw);
    let (status, _, problem) = send_json(
        app.router(),
        "POST",
        "/api/sessions",
        Some(json!({"token": token})),
        None,
    )
    .await;

    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(problem["type"], "/errors/magic-link-expired");
}

#[tokio::test]
async fn unknown_or_malformed_tokens_are_401_invalid_problems() {
    let app = test_app().await;

    for bad in ["%%%not-base64%%%", "aGVsbG8"] {
        let (status, _, problem) = send_json(
            app.router(),
            "POST",
            "/api/sessions",
            Some(json!({"token": bad})),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "token {bad:?}");
        assert_eq!(problem["type"], "/errors/magic-link-invalid");
    }
}

#[tokio::test]
async fn per_email_rate_limit_rejects_the_fourth_request_before_sending() {
    let app = test_app().await;

    for n in 1..=3 {
        let (status, _, _) = send_json(
            app.router(),
            "POST",
            "/api/magic-links",
            Some(json!({"email": "op@example.com"})),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::ACCEPTED, "request {n} within quota");
    }

    // Fourth request: 429 problem with a Retry-After hint.
    let request = Request::builder()
        .method("POST")
        .uri("/api/magic-links")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(
            json!({"email": "Op@Example.com"}).to_string(), // same email once normalized
        ))
        .expect("build request");
    let response = app.router().oneshot(request).await.expect("route request");
    assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
    assert!(
        response.headers().get(header::RETRY_AFTER).is_some(),
        "429 must carry Retry-After"
    );
    let bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024)
        .await
        .expect("read body");
    let problem: Value = serde_json::from_slice(&bytes).expect("problem json");
    assert_eq!(problem["type"], "/errors/rate-limited");

    // The limit fired BEFORE issuing a token or sending mail.
    assert_eq!(
        app.mailer.sent.lock().expect("mailer lock").len(),
        3,
        "no mail may be sent for the rejected request"
    );

    // A different email is unaffected.
    let (status, _, _) = send_json(
        app.router(),
        "POST",
        "/api/magic-links",
        Some(json!({"email": "other@example.com"})),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED);
}

#[tokio::test]
async fn ip_layer_backstops_the_auth_routes() {
    use axum::extract::ConnectInfo;
    use std::net::SocketAddr;

    let app = test_app().await;
    let router = netroll_app::http::api_router_ip_limited(app.state.clone());
    let peer = SocketAddr::from(([127, 0, 0, 1], 40_000));

    // Unique emails keep the per-email quota out of the picture; the 11th
    // request from one IP trips the 10-burst blunt-instrument layer.
    let mut last_status = StatusCode::ACCEPTED;
    for n in 0..11 {
        let mut request = Request::builder()
            .method("POST")
            .uri("/api/magic-links")
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(
                json!({"email": format!("op{n}@example.com")}).to_string(),
            ))
            .expect("build request");
        request.extensions_mut().insert(ConnectInfo(peer));
        let response = router
            .clone()
            .oneshot(request)
            .await
            .expect("route request");
        last_status = response.status();
        if n < 10 {
            assert_eq!(last_status, StatusCode::ACCEPTED, "request {n} in burst");
        }
    }
    assert_eq!(last_status, StatusCode::TOO_MANY_REQUESTS);
}

#[tokio::test]
async fn ip_layer_keys_on_the_forwarded_client_ip_behind_a_proxy() {
    use axum::extract::ConnectInfo;
    use std::net::SocketAddr;

    let app = test_app().await;
    let router = netroll_app::http::api_router_ip_limited(app.state.clone());
    // Every request arrives from the same TCP peer (the reverse proxy).
    let proxy_peer = SocketAddr::from(([127, 0, 0, 1], 40_000));

    let send = |router: Router, n: usize, client_ip: &str| {
        let mut request = Request::builder()
            .method("POST")
            .uri("/api/magic-links")
            .header(header::CONTENT_TYPE, "application/json")
            .header("x-forwarded-for", client_ip.to_owned())
            .body(Body::from(
                json!({"email": format!("fwd{n}@example.com")}).to_string(),
            ))
            .expect("build request");
        request.extensions_mut().insert(ConnectInfo(proxy_peer));
        async move { router.oneshot(request).await.expect("route request") }
    };

    // 15 requests from 15 DIFFERENT forwarded clients through one proxy
    // peer: no shared bucket, nobody is limited.
    for n in 0..15 {
        let response = send(router.clone(), n, &format!("203.0.113.{n}")).await;
        assert_eq!(
            response.status(),
            StatusCode::ACCEPTED,
            "distinct clients must not share the proxy's bucket (request {n})"
        );
    }

    // 11 requests from ONE forwarded client still trip the 10-burst limit.
    let mut last_status = StatusCode::ACCEPTED;
    for n in 100..111 {
        last_status = send(router.clone(), n, "198.51.100.7").await.status();
    }
    assert_eq!(last_status, StatusCode::TOO_MANY_REQUESTS);
}

#[tokio::test]
async fn me_without_a_session_is_401_unauthenticated() {
    let app = test_app().await;

    let (status, _, problem) = send_json(app.router(), "GET", "/api/accounts/me", None, None).await;

    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(problem["type"], "/errors/unauthenticated");
}

// ---- Bot mitigation on signup -----------------------------

#[tokio::test]
async fn bot_mitigation_disabled_signup_needs_no_token_and_still_mails() {
    // With mitigation off (the default), the endpoint behaves exactly as
    // before — a tokenless request issues a link.
    let app = test_app().await;

    let (status, _, _) = send_json(
        app.router(),
        "POST",
        "/api/magic-links",
        Some(json!({ "email": "op@example.com" })),
        None,
    )
    .await;

    assert_eq!(status, StatusCode::ACCEPTED);
    assert_eq!(app.mailer.sent_count(), 1, "a link is mailed when disabled");
}

#[tokio::test]
async fn bot_mitigation_drops_a_filled_honeypot_signup_with_no_mail() {
    // A filled honeypot yields the SAME 202 a real request gets, but no
    // token is issued and no mail is sent — indistinguishable from success.
    let app = bot_mitigated_app().await;

    let (status, _, _) = send_json(
        app.router(),
        "POST",
        "/api/magic-links",
        Some(json!({
            "email": "op@example.com",
            "formToken": human_form_token(),
            "hpField": "http://spam.example",
        })),
        None,
    )
    .await;

    assert_eq!(status, StatusCode::ACCEPTED, "success-shaped 202");
    assert_eq!(app.mailer.sent_count(), 0, "no mail on a dropped signup");
}

#[tokio::test]
async fn bot_mitigation_drops_a_tokenless_signup_with_no_mail() {
    // With mitigation enabled, a missing form token is a bot — dropped
    // silently (202, no mail).
    let app = bot_mitigated_app().await;

    let (status, _, _) = send_json(
        app.router(),
        "POST",
        "/api/magic-links",
        Some(json!({ "email": "op@example.com" })),
        None,
    )
    .await;

    assert_eq!(status, StatusCode::ACCEPTED);
    assert_eq!(app.mailer.sent_count(), 0, "no mail on a tokenless signup");
}

#[tokio::test]
async fn bot_mitigation_lets_a_valid_human_signup_through() {
    // A valid, well-timed token with an empty honeypot proceeds normally
    // and mails the link.
    let app = bot_mitigated_app().await;

    let (status, _, _) = send_json(
        app.router(),
        "POST",
        "/api/magic-links",
        Some(json!({
            "email": "op@example.com",
            "formToken": human_form_token(),
            "hpField": "",
        })),
        None,
    )
    .await;

    assert_eq!(status, StatusCode::ACCEPTED);
    assert_eq!(
        app.mailer.sent_count(),
        1,
        "a human signup is mailed a link"
    );
}

// ---- Instance-wide aggregate magic-link send cap ------

/// A test app whose instance-wide magic-link budget is pinned to
/// `sends_per_hour`, so saturation is reachable in a handful of requests.
async fn aggregate_capped_app(sends_per_hour: u32) -> TestApp {
    // The reserve is pinned to a tiny, KNOWN value rather than left at the
    // 120/hour default, so a test that saturates the general pool is testing the
    // two-tier cap it means to test and not the default second tier.
    two_tier_capped_app(sends_per_hour, 1).await
}

/// A test app with BOTH magic-link budgets pinned: `general` sends per hour for
/// everyone, `reserve` sends per hour that only a proven-control address may
/// fall back to once the general pool is spent.
async fn two_tier_capped_app(general: u32, reserve: u32) -> TestApp {
    test_app_configured(move |state| {
        state
            .with_magic_link_aggregate_cap(general)
            .with_magic_link_reserve_cap(reserve)
    })
    .await
}

/// The full observable surface of a magic-link response: status, sorted header
/// name/value pairs, and the RAW body bytes.
///
/// [`send_json`] normalizes an empty body to `Value::Null`, which makes a
/// "same body" comparison between two empty responses vacuously true. The real
/// bytes and the real headers are needed, because a body appearing on one
/// branch — or a `Retry-After` that announces the cap fired — is exactly the
/// side channel this must not have.
async fn magic_link_surface(
    router: Router,
    email: &str,
) -> (StatusCode, Vec<(String, String)>, Vec<u8>) {
    let request = Request::builder()
        .method("POST")
        .uri("/api/magic-links")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(json!({ "email": email }).to_string()))
        .expect("build request");
    let response = router.oneshot(request).await.expect("route request");
    let status = response.status();
    let mut headers: Vec<(String, String)> = response
        .headers()
        .iter()
        .map(|(name, value)| {
            (
                name.as_str().to_owned(),
                String::from_utf8_lossy(value.as_bytes()).into_owned(),
            )
        })
        .collect();
    headers.sort();
    let body = axum::body::to_bytes(response.into_body(), 1024 * 1024)
        .await
        .expect("read body")
        .to_vec();
    (status, headers, body)
}

/// Admin-disables `email` directly in storage — the abuse-response action whose
/// interaction with the proven-control set this suite pins. A runtime query (not
/// the `query!` macro) so no `.sqlx` metadata is needed.
async fn disable_account(pool: &PgPool, email: &str) {
    let disabled = sqlx::query("UPDATE accounts SET disabled_at = now() WHERE email = $1")
        .bind(email)
        .execute(pool)
        .await
        .expect("disable account")
        .rows_affected();
    assert_eq!(disabled, 1, "exactly one account must have been disabled");
}

/// Requests a magic link for `email` and returns the status.
async fn request_link(app: &TestApp, email: &str) -> StatusCode {
    let (status, _, _) = send_json(
        app.router(),
        "POST",
        "/api/magic-links",
        Some(json!({ "email": email })),
        None,
    )
    .await;
    status
}

/// How many `magic_link_tokens` rows exist for `email` — the storage witness
/// that a capped request issued no token. A runtime query (not the `query!`
/// macro) so this test needs no `.sqlx` metadata.
async fn token_rows_for(pool: &PgPool, email: &str) -> i64 {
    sqlx::query_scalar::<_, i64>("SELECT count(*) FROM magic_link_tokens WHERE email = $1")
        .bind(email)
        .fetch_one(pool)
        .await
        .expect("count magic link tokens")
}

#[tokio::test]
async fn the_aggregate_cap_bounds_total_mail_across_distinct_addresses() {
    // The property no per-address limiter can produce — three DIFFERENT
    // addresses, each well inside the 3/15min per-address quota, still draw on
    // one instance-wide budget.
    let app = aggregate_capped_app(2).await;

    for address in ["a@example.com", "b@example.com", "c@example.com"] {
        assert_eq!(
            request_link(&app, address).await,
            StatusCode::ACCEPTED,
            "every request is a uniform 202, capped or not"
        );
    }

    assert_eq!(
        app.mailer.sent_count(),
        2,
        "the cap bounds total outbound magic-link mail across all addresses"
    );
}

#[tokio::test]
async fn a_capped_request_is_a_202_with_no_token_row_and_no_mail() {
    // The capped response reuses the bot-verdict 202 path — same status as
    // a successful send, no problem+json, no token issued, no mail.
    let app = aggregate_capped_app(1).await;
    let (sent_status, sent_headers, sent_body) =
        magic_link_surface(app.router(), "first@example.com").await;
    assert_eq!(sent_status, StatusCode::ACCEPTED);

    let (capped_status, capped_headers, capped_body) =
        magic_link_surface(app.router(), "capped@example.com").await;

    assert_eq!(
        capped_status,
        StatusCode::ACCEPTED,
        "a capped request is indistinguishable from a successful one"
    );
    // Compared as RAW BYTES against the successful send, not as parsed JSON:
    // both must be genuinely empty, so a future refactor that starts returning
    // any body — a problem+json for the cap denial above all — breaks here.
    assert!(
        capped_body.is_empty(),
        "a capped 202 carries no body at all, so it cannot surface a cap denial"
    );
    assert_eq!(
        capped_body, sent_body,
        "…and a successful 202 carries none either"
    );
    assert!(
        !capped_headers
            .iter()
            .any(|(name, _)| name == header::RETRY_AFTER.as_str()),
        "a Retry-After would itself announce that the cap fired"
    );
    assert_eq!(
        capped_headers, sent_headers,
        "no header may distinguish a capped response from a successful one"
    );
    assert_eq!(
        app.mailer.sent_count(),
        1,
        "no mail is sent for the capped request"
    );
    assert_eq!(
        token_rows_for(&app.pool, "capped@example.com").await,
        0,
        "a capped request issues no magic-link token"
    );
}

#[tokio::test]
async fn the_aggregate_cap_enforces_whatever_number_the_instance_configures() {
    // Instance-configurable: the same scenario at a different cap
    // enforces the different number.
    let app = aggregate_capped_app(4).await;

    for n in 0..6 {
        assert_eq!(
            request_link(&app, &format!("op{n}@example.com")).await,
            StatusCode::ACCEPTED
        );
    }

    assert_eq!(
        app.mailer.sent_count(),
        4,
        "the configured budget, not a hardcoded one, is what bounds the sends"
    );
}

#[tokio::test]
async fn a_returning_user_still_signs_in_while_the_cap_is_saturated() {
    // An address that has completed a sign-in here can fall back to the
    // RESERVE budget, so an abuse campaign that saturates the general pool cannot
    // lock out real users — a total auth outage.
    let app = aggregate_capped_app(2).await;

    // A returning user: request a link and complete the sign-in (the only thing
    // that can mark an address as proven).
    assert_eq!(
        request_link(&app, "returning@example.com").await,
        StatusCode::ACCEPTED
    );
    let token = token_from_link(&app.mailer.last_link());
    let (created, _, _) = send_json(
        app.router(),
        "POST",
        "/api/sessions",
        Some(json!({ "token": token })),
        None,
    )
    .await;
    assert_eq!(created, StatusCode::CREATED);

    // A campaign drains the rest of the instance budget and is then capped.
    assert_eq!(
        request_link(&app, "harvested1@example.com").await,
        StatusCode::ACCEPTED
    );
    assert_eq!(
        request_link(&app, "harvested2@example.com").await,
        StatusCode::ACCEPTED
    );
    assert_eq!(
        app.mailer.sent_count(),
        2,
        "the campaign is capped: the second harvested address gets no mail"
    );

    // The returning user asks for another link mid-campaign and IS mailed one.
    assert_eq!(
        request_link(&app, "returning@example.com").await,
        StatusCode::ACCEPTED
    );
    assert_eq!(
        app.mailer.sent_count(),
        3,
        "a proven-control address reaches the reserve once the general pool is spent"
    );
}

#[tokio::test]
async fn having_an_account_is_not_what_exempts_an_address_from_the_cap() {
    // Reserve access keys on a completed sign-in ON THIS INSTANCE, never on
    // account existence — `create_magic_link` must still perform no account
    // lookup. Proven by a SECOND AppState over the SAME database: the account
    // exists in storage, but this instance has no record of it signing in (the
    // restart case), so it must be treated exactly like an address with no
    // account at all.
    let app = aggregate_capped_app(2).await;
    assert_eq!(
        request_link(&app, "has-account@example.com").await,
        StatusCode::ACCEPTED
    );
    let token = token_from_link(&app.mailer.last_link());
    let (created, _, _) = send_json(
        app.router(),
        "POST",
        "/api/sessions",
        Some(json!({ "token": token })),
        None,
    )
    .await;
    assert_eq!(created, StatusCode::CREATED);

    // Fresh instance state (fresh budget, empty proven-control set), same DB.
    let restarted = AppState::new(
        app.pool.clone(),
        app.mailer.clone(),
        "http://localhost:5173".into(),
    )
    .with_magic_link_aggregate_cap(2)
    .with_magic_link_reserve_cap(1);
    let router = || api_router(restarted.clone());
    let saturate = |n: usize| {
        send_json(
            router(),
            "POST",
            "/api/magic-links",
            Some(json!({ "email": format!("drain{n}@example.com") })),
            None,
        )
    };
    saturate(0).await;
    saturate(1).await;
    let sent_when_saturated = app.mailer.sent_count();

    let (with_account_status, with_account_headers, with_account_body) =
        magic_link_surface(router(), "has-account@example.com").await;
    let (no_account_status, no_account_headers, no_account_body) =
        magic_link_surface(router(), "no-account@example.com").await;

    assert_eq!(
        with_account_status, no_account_status,
        "an address with an account and one without get the same status"
    );
    // Raw bytes and real headers, not `send_json`'s parsed body: comparing two
    // `Value::Null`s would pass however the handler answered. Assert the bodies
    // are empty AND identical, and that the header sets match, so a refactor
    // that added a body or a distinguishing header on either branch fails here.
    assert!(
        with_account_body.is_empty() && no_account_body.is_empty(),
        "both responses carry no body at all"
    );
    assert_eq!(
        with_account_body, no_account_body,
        "…and are byte-identical to each other"
    );
    assert_eq!(
        with_account_headers, no_account_headers,
        "no header distinguishes an address with an account from one without"
    );
    assert_eq!(
        app.mailer.sent_count(),
        sent_when_saturated,
        "neither is mailed: existence in the accounts table buys no reserve access"
    );
    assert_eq!(
        token_rows_for(&app.pool, "no-account@example.com").await,
        0,
        "no token row for the address with no account"
    );
    assert_eq!(
        token_rows_for(&app.pool, "has-account@example.com").await,
        1,
        "…and none added for the one with an account (only its original sign-in row)"
    );
}

#[tokio::test]
async fn total_mail_stays_bounded_even_when_every_requesting_address_is_proven() {
    // The load-bearing property of the two-tier cap. Proven control used to be a
    // BYPASS of the bucket, so once an attacker had spent one capped cell per
    // catch-all address to get itself proven, total outbound mail was unbounded.
    // A reserve BUDGET replaces the bypass: the instance's total is
    // general + reserve no matter who asks.
    const GENERAL: usize = 4;
    const RESERVE: usize = 2;
    let app = two_tier_capped_app(GENERAL as u32, RESERVE as u32).await;

    // Three addresses prove control the only way anyone can — request a link
    // (which itself costs a general cell) and complete the sign-in.
    let proven = ["p0@example.com", "p1@example.com", "p2@example.com"];
    for address in proven {
        assert_eq!(request_link(&app, address).await, StatusCode::ACCEPTED);
        let token = token_from_link(&app.mailer.last_link());
        let (created, _, _) = send_json(
            app.router(),
            "POST",
            "/api/sessions",
            Some(json!({ "token": token })),
            None,
        )
        .await;
        assert_eq!(created, StatusCode::CREATED, "{address} signed in");
    }

    // All three now hammer the endpoint, each staying inside its own 3/15min
    // per-address quota so ONLY the instance-wide budget can be what stops them.
    for _ in 0..2 {
        for address in proven {
            assert_eq!(
                request_link(&app, address).await,
                StatusCode::ACCEPTED,
                "capped or not, the response stays a uniform 202"
            );
        }
    }

    assert_eq!(
        app.mailer.sent_count(),
        GENERAL + RESERVE,
        "total outbound mail is bounded by general + reserve even when every \
         requesting address is proven — proof of control buys a budget, not a bypass"
    );
}

#[tokio::test]
async fn a_proven_address_draws_the_general_pool_before_the_reserve() {
    // The reserve exists for saturation only. If a proven address took the
    // reserve first, it would leave general capacity unspent for a campaign to
    // consume, and the reserve — the thing that keeps returning users signing in
    // — would already be gone by the time it was actually needed.
    let app = two_tier_capped_app(2, 2).await;

    assert_eq!(
        request_link(&app, "proven@example.com").await,
        StatusCode::ACCEPTED
    );
    let token = token_from_link(&app.mailer.last_link());
    let (created, _, _) = send_json(
        app.router(),
        "POST",
        "/api/sessions",
        Some(json!({ "token": token })),
        None,
    )
    .await;
    assert_eq!(created, StatusCode::CREATED);

    // Second request from the now-proven address: it must spend the LAST general
    // cell, not open the reserve.
    assert_eq!(
        request_link(&app, "proven@example.com").await,
        StatusCode::ACCEPTED
    );
    assert_eq!(app.mailer.sent_count(), 2);

    // So an unknown address arriving next finds the general pool empty and is
    // capped. Had the proven request drawn the reserve instead, a general cell
    // would still be sitting here and this would be mailed.
    assert_eq!(
        request_link(&app, "unknown@example.com").await,
        StatusCode::ACCEPTED
    );
    assert_eq!(
        app.mailer.sent_count(),
        2,
        "a proven address spends the general pool first, so nothing is left here"
    );
}

#[tokio::test]
async fn a_disabled_account_gains_no_reserve_access_by_redeeming_a_link() {
    // The proven-control write sits BELOW the AccountDisabled refusal: an
    // operator who disables an abuser must not keep having that abuser's every
    // REFUSED redemption refresh its 30-day entry. Asserted behaviorally — the
    // disabled address must still be subject to the instance budget.
    let app = two_tier_capped_app(4, 2).await;

    // Create the account the only way the API allows, then disable it.
    assert_eq!(
        request_link(&app, "abuser@example.com").await,
        StatusCode::ACCEPTED
    );
    let token = token_from_link(&app.mailer.last_link());
    let (created, _, _) = send_json(
        app.router(),
        "POST",
        "/api/sessions",
        Some(json!({ "token": token })),
        None,
    )
    .await;
    assert_eq!(created, StatusCode::CREATED);
    disable_account(&app.pool, "abuser@example.com").await;

    // A fresh instance (empty proven-control set, fresh budgets) over the SAME
    // database — the sign-in above legitimately proved control before the
    // disable, so only a restart isolates what the refused redemption does.
    let restarted = AppState::new(
        app.pool.clone(),
        app.mailer.clone(),
        "http://localhost:5173".into(),
    )
    .with_magic_link_aggregate_cap(2)
    .with_magic_link_reserve_cap(2);
    let router = || api_router(restarted.clone());

    // The disabled abuser redeems a fresh, valid link and is refused.
    send_json(
        router(),
        "POST",
        "/api/magic-links",
        Some(json!({ "email": "abuser@example.com" })),
        None,
    )
    .await;
    let token = token_from_link(&app.mailer.last_link());
    let (refused, _, problem) = send_json(
        router(),
        "POST",
        "/api/sessions",
        Some(json!({ "token": token })),
        None,
    )
    .await;
    assert_eq!(
        refused,
        StatusCode::FORBIDDEN,
        "a disabled account is refused"
    );
    assert_eq!(problem["type"], "/errors/account-disabled");

    // Drain the rest of the general pool.
    let before_drain = app.mailer.sent_count();
    send_json(
        router(),
        "POST",
        "/api/magic-links",
        Some(json!({ "email": "drain@example.com" })),
        None,
    )
    .await;
    assert_eq!(
        app.mailer.sent_count(),
        before_drain + 1,
        "the drain request took the last general cell"
    );

    // Now the abuser asks again with the general pool empty. If the refused
    // redemption had marked it proven, it would reach the reserve and be mailed.
    let before_probe = app.mailer.sent_count();
    send_json(
        router(),
        "POST",
        "/api/magic-links",
        Some(json!({ "email": "abuser@example.com" })),
        None,
    )
    .await;
    assert_eq!(
        app.mailer.sent_count(),
        before_probe,
        "a REFUSED sign-in must not buy reserve access — the disabled address \
         stays subject to the instance budget like any unknown one"
    );
}

#[tokio::test]
async fn the_per_address_quota_is_evaluated_before_the_aggregate_cap() {
    // A regression: a single-address abuser must get an HONEST 429, not a
    // silent 202 — otherwise they quietly spend instance budget and learn
    // nothing about being throttled. The cap is pinned to exactly the
    // per-address burst, so at the fourth request BOTH limits are exhausted:
    // only the gate order decides which answer comes back.
    let app = aggregate_capped_app(3).await;

    for n in 1..=3 {
        assert_eq!(
            request_link(&app, "abuser@example.com").await,
            StatusCode::ACCEPTED,
            "request {n} within both quotas"
        );
    }

    let request = Request::builder()
        .method("POST")
        .uri("/api/magic-links")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(
            json!({"email": "abuser@example.com"}).to_string(),
        ))
        .expect("build request");
    let response = app.router().oneshot(request).await.expect("route request");

    assert_eq!(
        response.status(),
        StatusCode::TOO_MANY_REQUESTS,
        "the per-address quota answers first — the aggregate cap must not \
         swallow a single-address abuser into a silent 202"
    );
    assert!(
        response.headers().get(header::RETRY_AFTER).is_some(),
        "429 must carry Retry-After"
    );
    let bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024)
        .await
        .expect("read body");
    let problem: Value = serde_json::from_slice(&bytes).expect("problem json");
    assert_eq!(problem["type"], "/errors/rate-limited");
    assert_eq!(
        app.mailer.sent_count(),
        3,
        "no mail for the rejected request"
    );
}

#[tokio::test]
async fn form_token_endpoint_issues_a_token_when_enabled_and_null_when_disabled() {
    let enabled = bot_mitigated_app().await;
    let (status, _, body) =
        send_json(enabled.router(), "GET", "/api/form-tokens", None, None).await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        body["formToken"].is_string(),
        "an enabled instance issues a token"
    );

    let disabled = test_app().await;
    let (status, _, body) =
        send_json(disabled.router(), "GET", "/api/form-tokens", None, None).await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        body["formToken"].is_null(),
        "a disabled instance issues no token"
    );
}
