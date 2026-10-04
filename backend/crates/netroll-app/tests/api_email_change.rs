// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
//! Full-loop API tests for identifying-email change:
//! real router, real Postgres (testcontainers), capturing fake mailer that
//! records WHICH mail fired. Asserts status codes, problem+json `type`
//! slugs, body values, captured-mail recipients/kinds, and storage effects
//! — never message prose (house TDD rule).

use std::sync::{Arc, Mutex};

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use netroll_app::http::{AppState, api_router};
use netroll_app::known_addresses::KnownAddresses;
use netroll_domain::auth::hash_token;
use netroll_domain::consent::CURRENT_TERMS_VERSION;
use netroll_domain::ports::{BoxFuture, MailError, Mailer};
use netroll_domain::profile::gravatar_url;
use serde_json::{Value, json};
use sqlx::PgPool;
use sqlx::postgres::PgPoolOptions;
use testcontainers_modules::postgres::Postgres;
use testcontainers_modules::testcontainers::ContainerAsync;
use testcontainers_modules::testcontainers::runners::AsyncRunner;
use tower::ServiceExt;

/// One recorded mail: which method fired, the recipient, and the payload
/// (the link for magic-link/email-change, the new address for the notice).
#[derive(Clone)]
struct SentMail {
    kind: &'static str,
    to: String,
    payload: String,
}

/// Mailer that records sends (with the firing method) instead of speaking SMTP.
#[derive(Default)]
struct CapturingMailer {
    sent: Mutex<Vec<SentMail>>,
}

impl CapturingMailer {
    fn record(&self, kind: &'static str, to: &str, payload: &str) {
        self.sent.lock().expect("mailer lock").push(SentMail {
            kind,
            to: to.to_owned(),
            payload: payload.to_owned(),
        });
    }

    fn of_kind(&self, kind: &str) -> Vec<SentMail> {
        self.sent
            .lock()
            .expect("mailer lock")
            .iter()
            .filter(|m| m.kind == kind)
            .cloned()
            .collect()
    }

    fn count(&self) -> usize {
        self.sent.lock().expect("mailer lock").len()
    }

    /// The most recent magic-link link (drives the sign-in helper).
    fn last_magic_link(&self) -> String {
        self.of_kind("magic-link")
            .last()
            .expect("at least one magic link sent")
            .payload
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
            self.record("magic-link", to, link);
            Ok(())
        })
    }

    fn send_email_change<'a>(
        &'a self,
        to: &'a str,
        link: &'a str,
    ) -> BoxFuture<'a, Result<(), MailError>> {
        Box::pin(async move {
            self.record("email-change", to, link);
            Ok(())
        })
    }

    fn send_email_change_notice<'a>(
        &'a self,
        to: &'a str,
        new_email: &'a str,
    ) -> BoxFuture<'a, Result<(), MailError>> {
        Box::pin(async move {
            self.record("email-change-notice", to, new_email);
            Ok(())
        })
    }
}

struct TestApp {
    _container: ContainerAsync<Postgres>,
    state: AppState,
    mailer: Arc<CapturingMailer>,
    /// The container's pool, so a test can assert storage effects directly
    /// (a capped request must write NO `email_change_tokens` row).
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
/// the seam aggregate/reserve-cap tests use to pin tiny budgets
/// (mirrors `api_auth.rs`'s `test_app_configured`).
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

fn token_from_link(link: &str) -> String {
    link.split_once("token=")
        .expect("link carries a token")
        .1
        .to_owned()
}

/// Signs `email` all the way in (magic-link → session). Returns the session
/// cookie and the account body from the 201.
async fn sign_in(app: &TestApp, email: &str) -> (String, Value) {
    let magic_links_before = app.mailer.of_kind("magic-link").len();
    let (status, _) = send_json(
        app.router(),
        "POST",
        "/api/magic-links",
        Some(json!({ "email": email })),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED);
    // A capped `create_magic_link` is ALSO a 202 with no send, and
    // `last_magic_link()` would then silently hand back the previous caller's
    // already-consumed link — surfacing as an opaque 401, not as the cap. The
    // cap tests below count general-pool cells per sign-in; witness the send.
    assert_eq!(
        app.mailer.of_kind("magic-link").len(),
        magic_links_before + 1,
        "signing in {email} must spend exactly one magic-link send"
    );

    let token = token_from_link(&app.mailer.last_magic_link());
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
    let bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024)
        .await
        .expect("read body");
    let account: Value = serde_json::from_slice(&bytes).expect("account body");
    (cookie, account)
}

async fn record_consent(app: &TestApp, cookie: &str) {
    let (status, _) = send_json(
        app.router(),
        "POST",
        "/api/consents",
        Some(json!({ "termsVersion": CURRENT_TERMS_VERSION })),
        Some(cookie),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
}

/// Signs in and records consent — the email-change request is gated on
/// `ConsentedAccount`.
async fn sign_in_and_consent(app: &TestApp, email: &str) -> (String, Value) {
    let (cookie, account) = sign_in(app, email).await;
    record_consent(app, cookie.as_str()).await;
    (cookie, account)
}

const REQUEST_URI: &str = "/api/accounts/me/email-change";
const CONFIRM_URI: &str = "/api/email-changes";

#[tokio::test]
async fn requesting_a_change_mails_the_new_address_and_me_still_shows_the_old_one() {
    let app = test_app().await;
    let (cookie, _) = sign_in_and_consent(&app, "old@example.com").await;

    let (status, _) = send_json(
        app.router(),
        "POST",
        REQUEST_URI,
        Some(json!({ "email": "new@example.com" })),
        Some(&cookie),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED);

    // Exactly one confirmation mail, to the NEW address, with the confirm link.
    let mails = app.mailer.of_kind("email-change");
    assert_eq!(mails.len(), 1, "one confirmation mail");
    assert_eq!(mails[0].to, "new@example.com");
    assert!(
        mails[0]
            .payload
            .starts_with("http://localhost:5173/auth/confirm-email-change?token="),
        "confirm link shape (got {})",
        mails[0].payload
    );

    // The old email remains in effect until the link is confirmed.
    let (status, me) =
        send_json(app.router(), "GET", "/api/accounts/me", None, Some(&cookie)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(me["email"], "old@example.com");
}

#[tokio::test]
async fn confirming_swaps_identity_revokes_sessions_and_reroutes_both_addresses() {
    let app = test_app().await;
    let (cookie_old, account) = sign_in_and_consent(&app, "old@example.com").await;
    let original_id = account["id"].as_str().expect("account id").to_owned();

    // Give the account distinguishing state that must survive the move.
    let (status, _) = send_json(
        app.router(),
        "PUT",
        "/api/accounts/me/callsign",
        Some(json!({ "callsign": "w1aw" })),
        Some(&cookie_old),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (status, _) = send_json(
        app.router(),
        "PUT",
        "/api/accounts/me/profile",
        Some(json!({ "displayName": "Maria" })),
        Some(&cookie_old),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    // Request + confirm.
    let (status, _) = send_json(
        app.router(),
        "POST",
        REQUEST_URI,
        Some(json!({ "email": "new@example.com" })),
        Some(&cookie_old),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED);
    let token = token_from_link(&app.mailer.of_kind("email-change")[0].payload);

    // Confirm with NO cookie — the link may open on a device with no session.
    let (status, body) = send_json(
        app.router(),
        "POST",
        CONFIRM_URI,
        Some(json!({ "token": token })),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        body["email"], "new@example.com",
        "confirm returns the new address"
    );

    // The old session is dead.
    let (status, _) = send_json(
        app.router(),
        "GET",
        "/api/accounts/me",
        None,
        Some(&cookie_old),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::UNAUTHORIZED,
        "the old session is revoked"
    );

    // Signing in with the NEW address reaches the SAME account, state intact,
    // gravatar re-derived from the new email.
    let (cookie_new, new_account) = sign_in(&app, "new@example.com").await;
    assert_eq!(new_account["id"].as_str(), Some(original_id.as_str()));
    assert_eq!(new_account["callsign"], "W1AW");
    assert_eq!(new_account["displayName"], "Maria");
    assert_eq!(new_account["consentRequired"], false);
    assert_eq!(new_account["gravatarUrl"], gravatar_url("new@example.com"));
    // And /me agrees under the fresh session.
    let (_, me) = send_json(
        app.router(),
        "GET",
        "/api/accounts/me",
        None,
        Some(&cookie_new),
    )
    .await;
    assert_eq!(me["email"], "new@example.com");

    // The old address is now free: signing in there mints a DIFFERENT, empty
    // account.
    let (_, old_reborn) = sign_in(&app, "old@example.com").await;
    assert_ne!(
        old_reborn["id"].as_str(),
        Some(original_id.as_str()),
        "the freed old address is a brand-new account"
    );
    assert!(
        old_reborn["callsign"].is_null(),
        "the reborn account is empty"
    );
}

#[tokio::test]
async fn a_completed_change_notifies_the_old_address_naming_the_new_one() {
    let app = test_app().await;
    let (cookie, _) = sign_in_and_consent(&app, "old@example.com").await;

    send_json(
        app.router(),
        "POST",
        REQUEST_URI,
        Some(json!({ "email": "new@example.com" })),
        Some(&cookie),
    )
    .await;
    let token = token_from_link(&app.mailer.of_kind("email-change")[0].payload);
    let (status, _) = send_json(
        app.router(),
        "POST",
        CONFIRM_URI,
        Some(json!({ "token": token })),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let notices = app.mailer.of_kind("email-change-notice");
    assert_eq!(notices.len(), 1, "one notice mail to the old address");
    assert_eq!(notices[0].to, "old@example.com");
    assert_eq!(
        notices[0].payload, "new@example.com",
        "the notice names the new address"
    );
}

#[tokio::test]
async fn requesting_an_email_another_account_holds_is_409_with_no_token_and_no_mail() {
    let app = test_app().await;
    // A bystander already owns the target address.
    sign_in(&app, "taken@example.com").await;
    let (cookie, _) = sign_in_and_consent(&app, "old@example.com").await;

    // Count immediately before the request so intervening sign-in mail does
    // not muddy the "this request sent nothing" assertion.
    let mail_before = app.mailer.count();
    let (status, problem) = send_json(
        app.router(),
        "POST",
        REQUEST_URI,
        Some(json!({ "email": "taken@example.com" })),
        Some(&cookie),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(problem["type"], "/errors/email-taken");
    assert_eq!(
        app.mailer.count(),
        mail_before,
        "a taken target issues no confirmation mail"
    );
    assert!(
        app.mailer.of_kind("email-change").is_empty(),
        "no confirmation mail on a request-time conflict"
    );
}

#[tokio::test]
async fn an_email_claimed_between_request_and_confirm_is_409_and_leaves_the_requester_intact() {
    let app = test_app().await;
    let (cookie, _) = sign_in_and_consent(&app, "old@example.com").await;

    // Request the change while the target is still free.
    let (status, _) = send_json(
        app.router(),
        "POST",
        REQUEST_URI,
        Some(json!({ "email": "contested@example.com" })),
        Some(&cookie),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED);
    let token = token_from_link(&app.mailer.of_kind("email-change")[0].payload);

    // Another account claims the address before the confirm lands.
    sign_in(&app, "contested@example.com").await;

    let (status, problem) = send_json(
        app.router(),
        "POST",
        CONFIRM_URI,
        Some(json!({ "token": token })),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(problem["type"], "/errors/email-taken");

    // The requester's email is unchanged and the session is still live.
    let (status, me) =
        send_json(app.router(), "GET", "/api/accounts/me", None, Some(&cookie)).await;
    assert_eq!(status, StatusCode::OK, "the requester's session survives");
    assert_eq!(me["email"], "old@example.com", "the email is unchanged");
}

#[tokio::test]
async fn requesting_the_current_email_in_any_casing_is_400_validation() {
    let app = test_app().await;
    let (cookie, _) = sign_in_and_consent(&app, "op@example.com").await;

    for variant in ["op@example.com", "  OP@Example.com  "] {
        let (status, problem) = send_json(
            app.router(),
            "POST",
            REQUEST_URI,
            Some(json!({ "email": variant })),
            Some(&cookie),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "variant {variant:?}");
        assert_eq!(problem["type"], "/errors/validation", "variant {variant:?}");
        assert!(
            problem["detail"].as_str().is_some_and(|d| !d.is_empty()),
            "the validation problem names the field"
        );
    }
    assert!(
        app.mailer.of_kind("email-change").is_empty(),
        "same-address requests never mail"
    );
}

#[tokio::test]
async fn an_undeliverable_target_email_is_400_validation() {
    let app = test_app().await;
    let (cookie, _) = sign_in_and_consent(&app, "op@example.com").await;

    for bad in [
        json!("not-an-address"),
        json!(format!("{}@e.com", "x".repeat(260))),
    ] {
        let (status, problem) = send_json(
            app.router(),
            "POST",
            REQUEST_URI,
            Some(json!({ "email": bad })),
            Some(&cookie),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "email {bad:?}");
        assert_eq!(problem["type"], "/errors/validation", "email {bad:?}");
    }
}

#[tokio::test]
async fn the_per_address_rate_limit_throttles_before_the_taken_check_and_mail() {
    let app = test_app().await;
    // The target is held by a bystander, so every ADMITTED request is a
    // truthful 409. Signing the bystander in already spent one limiter cell
    // for this address — the sign-in and change flows share the bucket
    // deliberately (Dev Notes), so the exact number of admitted
    // probes is an artifact of that shared quota; assert the shape, not a
    // magic count.
    sign_in(&app, "taken@example.com").await;
    let (cookie, _) = sign_in_and_consent(&app, "op@example.com").await;

    // Probe well past the burst. Each response is either a truthful 409
    // (admitted, taken) or a 429 (throttled) — never a 200, and once
    // throttling starts it does not revert to answering the oracle.
    let mut statuses = Vec::new();
    let mut retry_after_seen = false;
    for _ in 0..6 {
        let request = Request::builder()
            .method("POST")
            .uri(REQUEST_URI)
            .header(header::CONTENT_TYPE, "application/json")
            .header(header::COOKIE, &cookie)
            .body(Body::from(
                json!({ "email": "taken@example.com" }).to_string(),
            ))
            .expect("build request");
        let response = app.router().oneshot(request).await.expect("route request");
        let status = response.status();
        if status == StatusCode::TOO_MANY_REQUESTS {
            retry_after_seen |= response.headers().get(header::RETRY_AFTER).is_some();
        }
        statuses.push(status);
    }

    assert!(
        statuses.contains(&StatusCode::CONFLICT),
        "at least one probe is admitted as a truthful 409"
    );
    assert!(
        statuses.contains(&StatusCode::TOO_MANY_REQUESTS),
        "probing past the shared quota must be throttled"
    );
    assert!(retry_after_seen, "429 must carry Retry-After");
    // The enumeration oracle is capped: once 429s begin, no later probe gets
    // a 409 answer back (monotonic — throttle never reverts to answering).
    let first_throttle = statuses
        .iter()
        .position(|s| *s == StatusCode::TOO_MANY_REQUESTS)
        .expect("a throttle occurs");
    assert!(
        statuses[first_throttle..]
            .iter()
            .all(|s| *s == StatusCode::TOO_MANY_REQUESTS),
        "no 409 taken-oracle answer leaks past the limit, got {statuses:?}"
    );
    assert!(
        app.mailer.of_kind("email-change").is_empty(),
        "no confirmation mail is ever sent on this path"
    );
}

#[tokio::test]
async fn confirming_an_expired_token_is_401_email_change_expired() {
    let app = test_app().await;
    let (cookie, account) = sign_in_and_consent(&app, "old@example.com").await;
    let account_id: uuid::Uuid = account["id"].as_str().unwrap().parse().unwrap();
    let _ = cookie;

    // Seed an already-expired token directly; a request over HTTP always
    // mints a live one (the expired-link technique).
    let raw = b"expired-change-token-32-bytes!!!";
    let hash = hash_token(raw);
    app.state
        .email_changes
        .issue(account_id, "new@example.com", hash, 1_000)
        .await
        .expect("seed expired token");

    use base64::Engine;
    let token = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(raw);
    let (status, problem) = send_json(
        app.router(),
        "POST",
        CONFIRM_URI,
        Some(json!({ "token": token })),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(problem["type"], "/errors/email-change-expired");
}

#[tokio::test]
async fn confirming_a_consumed_token_a_second_time_is_401_email_change_consumed() {
    let app = test_app().await;
    let (cookie, _) = sign_in_and_consent(&app, "old@example.com").await;

    send_json(
        app.router(),
        "POST",
        REQUEST_URI,
        Some(json!({ "email": "new@example.com" })),
        Some(&cookie),
    )
    .await;
    let token = token_from_link(&app.mailer.of_kind("email-change")[0].payload);

    let (status, _) = send_json(
        app.router(),
        "POST",
        CONFIRM_URI,
        Some(json!({ "token": token.clone() })),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let (status, problem) = send_json(
        app.router(),
        "POST",
        CONFIRM_URI,
        Some(json!({ "token": token })),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(problem["type"], "/errors/email-change-consumed");
}

#[tokio::test]
async fn confirming_an_unknown_or_malformed_token_is_401_email_change_invalid() {
    let app = test_app().await;

    for bad in ["%%%not-base64%%%", "aGVsbG8"] {
        let (status, problem) = send_json(
            app.router(),
            "POST",
            CONFIRM_URI,
            Some(json!({ "token": bad })),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "token {bad:?}");
        assert_eq!(
            problem["type"], "/errors/email-change-invalid",
            "token {bad:?}"
        );
    }
}

#[tokio::test]
async fn an_unauthenticated_request_is_401_and_an_unconsented_one_is_403() {
    let app = test_app().await;

    // No session at all.
    let (status, problem) = send_json(
        app.router(),
        "POST",
        REQUEST_URI,
        Some(json!({ "email": "new@example.com" })),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(problem["type"], "/errors/unauthenticated");

    // Signed in but NOT consented.
    let (cookie, _) = sign_in(&app, "op@example.com").await;
    let (status, problem) = send_json(
        app.router(),
        "POST",
        REQUEST_URI,
        Some(json!({ "email": "new@example.com" })),
        Some(&cookie),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(problem["type"], "/errors/consent-required");
}

#[tokio::test]
async fn the_magic_link_sign_in_path_is_unaffected_by_the_new_slugs() {
    let app = test_app().await;

    // Sign-in still uses its own slugs; the email-change slugs never bleed in.
    for bad in ["%%%not-base64%%%", "aGVsbG8"] {
        let (status, problem) = send_json(
            app.router(),
            "POST",
            "/api/sessions",
            Some(json!({ "token": bad })),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        assert_eq!(problem["type"], "/errors/magic-link-invalid");
    }

    // And a plain magic-link request is still a uniform 202.
    let (status, _) = send_json(
        app.router(),
        "POST",
        "/api/magic-links",
        Some(json!({ "email": "op@example.com" })),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED);
}

// ---- The email-change path joins the instance-wide send cap ------

/// A test app with BOTH instance-wide mail budgets pinned: `general` sends per
/// hour drawn first by every request, `reserve` sends per hour drawn only once
/// the general pool is spent. Each pool starts full (burst = the configured
/// value), so the exact number of admits is knowable.
async fn two_tier_capped_app(general: u32, reserve: u32) -> TestApp {
    test_app_configured(move |state| {
        state
            .with_magic_link_aggregate_cap(general)
            .with_magic_link_reserve_cap(reserve)
    })
    .await
}

/// Number of `email_change_tokens` rows — the storage witness that a capped
/// request issued NO token (a mail assertion alone would pass if the token were
/// written and only the send were skipped).
async fn email_change_token_count(pool: &PgPool) -> i64 {
    sqlx::query_scalar::<_, i64>("SELECT count(*) FROM email_change_tokens")
        .fetch_one(pool)
        .await
        .expect("count email_change_tokens")
}

/// The full observable surface of an email-change request: status, sorted header
/// name/value pairs, and the RAW body bytes. `send_json` normalizes an empty body
/// to `Value::Null`, which would make a "same body" comparison between two empty
/// responses vacuously true; the real bytes and headers are needed because a
/// body or a `Retry-After` appearing on only one branch is exactly the side
/// channel it forbids.
async fn email_change_surface(
    router: Router,
    cookie: &str,
    email: &str,
) -> (StatusCode, Vec<(String, String)>, Vec<u8>) {
    let request = Request::builder()
        .method("POST")
        .uri(REQUEST_URI)
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::COOKIE, cookie)
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

/// The two endpoints spend ONE instance-wide budget. The general pool is
/// drained entirely by MAGIC-LINK traffic; the first email-change request then
/// succeeds only because it falls back to the reserve, and once the reserve is
/// spent too a further email-change request is capped — silent 202, no mail,
/// no token.
#[tokio::test]
async fn the_email_change_path_spends_the_same_instance_wide_budget_as_magic_links() {
    let app = two_tier_capped_app(1, 1).await;
    // Signing the requester in spends the general pool's ONLY cell via
    // `/api/magic-links` — so by the time the email-change request below runs,
    // the general pool has been drained by the OTHER endpoint's traffic.
    let (cookie, _) = sign_in_and_consent(&app, "op@example.com").await;
    let tokens_before = email_change_token_count(&app.pool).await;

    // General is empty; the reserve still has its single cell. An authenticated
    // caller draws it.
    let (status, _) = send_json(
        app.router(),
        "POST",
        REQUEST_URI,
        Some(json!({ "email": "first@example.com" })),
        Some(&cookie),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED);
    assert_eq!(
        app.mailer.of_kind("email-change").len(),
        1,
        "the reserve admits the first email-change request"
    );
    assert_eq!(
        email_change_token_count(&app.pool).await,
        tokens_before + 1,
        "the admitted request issued a token"
    );

    // Both pools are now spent. The next request is capped: same 202, but
    // nothing was mailed and nothing was stored.
    let (status, _) = send_json(
        app.router(),
        "POST",
        REQUEST_URI,
        Some(json!({ "email": "second@example.com" })),
        Some(&cookie),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::ACCEPTED,
        "a capped request is a silent 202"
    );
    assert_eq!(
        app.mailer.of_kind("email-change").len(),
        1,
        "a capped request sends no confirmation mail"
    );
    assert_eq!(
        email_change_token_count(&app.pool).await,
        tokens_before + 1,
        "a capped request writes no email_change_tokens row"
    );
}

/// Authentication alone grants reserve access on this path — NOT
/// `known_addresses` membership. The requester's own sign-in wrote its address
/// into `known_addresses`, so the set is swapped for an EMPTY one before any
/// email-change request: neither the caller's current address nor any target
/// is a member, and a reserve gated on either (`contains(&account.email)` or
/// `contains(&new_email)`) would deny. The reserve is then drained by
/// email-change traffic ALONE (the one magic-link send here is the sign-in,
/// which spends the GENERAL pool): every admit draws the reserve down by one
/// until it is spent, proving the fallback runs on the live session alone.
#[tokio::test]
async fn an_authenticated_caller_draws_the_reserve_without_known_addresses_membership() {
    let mut app = two_tier_capped_app(1, 2).await;
    // The sign-in spends the general pool's only cell; the reserve is untouched.
    let (cookie, _) = sign_in_and_consent(&app, "op@example.com").await;
    app.state.known_addresses = Arc::new(KnownAddresses::new());
    let tokens_before = email_change_token_count(&app.pool).await;

    // Two reserve cells, two admits — each to an address that has NEVER signed
    // in here and so cannot be in `known_addresses`.
    for (i, target) in ["never-seen-1@example.com", "never-seen-2@example.com"]
        .iter()
        .enumerate()
    {
        let (status, _) = send_json(
            app.router(),
            "POST",
            REQUEST_URI,
            Some(json!({ "email": target })),
            Some(&cookie),
        )
        .await;
        assert_eq!(status, StatusCode::ACCEPTED, "request {i}");
        assert_eq!(
            app.mailer.of_kind("email-change").len(),
            i + 1,
            "reserve admit {i} mails the new address"
        );
        assert_eq!(
            email_change_token_count(&app.pool).await,
            tokens_before + i as i64 + 1,
            "reserve admit {i} issues a token"
        );
    }

    // The reserve has been drawn down to zero by email-change traffic alone;
    // the third request is capped.
    let (status, _) = send_json(
        app.router(),
        "POST",
        REQUEST_URI,
        Some(json!({ "email": "never-seen-3@example.com" })),
        Some(&cookie),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::ACCEPTED,
        "a capped request is a silent 202"
    );
    assert_eq!(
        app.mailer.of_kind("email-change").len(),
        2,
        "the third request is capped and sends no mail"
    );
    assert_eq!(
        email_change_token_count(&app.pool).await,
        tokens_before + 2,
        "the third request is capped and writes no token"
    );
}

/// The other direction: email-change traffic spends the GENERAL pool
/// that magic-link requests draw on. The three sibling tests all drain general
/// via sign-ins first, so a reserve-only gate on this path would pass them; here
/// an email-change request takes the general pool's last cell, and a magic-link
/// request for an address that has never signed in (no reserve access) is then
/// capped — silent 202, no magic-link mail.
#[tokio::test]
async fn email_change_traffic_spends_the_general_pool_magic_links_draw_on() {
    let app = two_tier_capped_app(2, 1).await;
    // general 2 -> 1
    let (cookie, _) = sign_in_and_consent(&app, "op@example.com").await;

    // general 1 -> 0 (the reserve is untouched: general still had room)
    let (status, _) = send_json(
        app.router(),
        "POST",
        REQUEST_URI,
        Some(json!({ "email": "first@example.com" })),
        Some(&cookie),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED);
    assert_eq!(app.mailer.of_kind("email-change").len(), 1);

    let magic_links_before = app.mailer.of_kind("magic-link").len();
    let (status, _) = send_json(
        app.router(),
        "POST",
        "/api/magic-links",
        Some(json!({ "email": "stranger@example.com" })),
        None,
    )
    .await;
    assert_eq!(
        status,
        StatusCode::ACCEPTED,
        "a capped magic link is a silent 202"
    );
    assert_eq!(
        app.mailer.of_kind("magic-link").len(),
        magic_links_before,
        "the general pool was spent by email-change traffic, so an unknown address gets no link"
    );
}

/// One consented account may not drain the
/// shared instance budget on its own. The email-change path carries a
/// per-ACCOUNT quota, checked before the shared per-address and instance-wide
/// checks, so a throttled account spends no shared cell. Past the burst the
/// answer is an honest 429 + `Retry-After` — the caller is keyed on their own
/// account id, never on `new_email`, so nothing about the target leaks. A
/// different account keeps its own bucket.
#[tokio::test]
async fn one_account_cannot_drain_the_shared_budget_past_its_own_quota() {
    let app = test_app().await;
    let (cookie, _) = sign_in_and_consent(&app, "op@example.com").await;
    let tokens_before = email_change_token_count(&app.pool).await;

    // Distinct targets every time, so the per-address quota never trips and
    // the only thing that can stop the flood is the account's own bucket.
    let mut admitted = 0usize;
    let mut throttled: Option<(StatusCode, bool)> = None;
    for n in 0..EMAIL_CHANGE_FLOOD {
        let request = Request::builder()
            .method("POST")
            .uri(REQUEST_URI)
            .header(header::CONTENT_TYPE, "application/json")
            .header(header::COOKIE, &cookie)
            .body(Body::from(
                json!({ "email": format!("target{n}@example.com") }).to_string(),
            ))
            .expect("build request");
        let response = app.router().oneshot(request).await.expect("route request");
        if response.status() == StatusCode::TOO_MANY_REQUESTS {
            throttled = Some((
                response.status(),
                response.headers().get(header::RETRY_AFTER).is_some(),
            ));
            break;
        }
        assert_eq!(response.status(), StatusCode::ACCEPTED, "request {n}");
        admitted += 1;
    }

    let (status, retry_after) = throttled.expect("the flood must be throttled before it ends");
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
    assert!(retry_after, "a throttled request carries Retry-After");
    assert!(
        admitted < EMAIL_CHANGE_FLOOD,
        "the account's quota must bind well below the instance budget, admitted {admitted}"
    );
    assert_eq!(
        app.mailer.of_kind("email-change").len(),
        admitted,
        "every admitted request mailed, the throttled one did not"
    );
    assert_eq!(
        email_change_token_count(&app.pool).await,
        tokens_before + admitted as i64,
        "the throttled request wrote no token"
    );

    // Account-keyed, not IP-keyed or instance-wide: a second account is untouched.
    let (other_cookie, _) = sign_in_and_consent(&app, "other@example.com").await;
    let (status, _) = send_json(
        app.router(),
        "POST",
        REQUEST_URI,
        Some(json!({ "email": "elsewhere@example.com" })),
        Some(&other_cookie),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED);
    assert_eq!(
        app.mailer.of_kind("email-change").len(),
        admitted + 1,
        "a different account has its own bucket"
    );
}

/// More requests than any sane per-account burst, and fewer than the default
/// instance budget (60 general + 120 reserve), so the flood can only be
/// stopped by the account's own quota.
const EMAIL_CHANGE_FLOOD: usize = 40;

/// The cap lands BEFORE the taken-probe, so a capped request cannot
/// become an account-existence oracle. Under a fully drained budget, a request
/// for an address a bystander holds and a request for an address nobody holds
/// must be byte-identical: the same silent 202, no `409 EmailTaken`, no mail,
/// no token, no `Retry-After`.
#[tokio::test]
async fn a_capped_request_never_reaches_the_taken_probe() {
    // Two general cells bootstrap two sign-ins; one reserve cell is spent
    // deliberately below so the assertions run against a FULLY drained budget.
    let app = two_tier_capped_app(2, 1).await;
    sign_in(&app, "taken@example.com").await;
    let (cookie, _) = sign_in_and_consent(&app, "op@example.com").await;

    // Spend the reserve's only cell with an ordinary admitted request.
    let (status, _) = send_json(
        app.router(),
        "POST",
        REQUEST_URI,
        Some(json!({ "email": "drain@example.com" })),
        Some(&cookie),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED);
    assert_eq!(app.mailer.of_kind("email-change").len(), 1);
    let tokens_before = email_change_token_count(&app.pool).await;
    let mail_before = app.mailer.count();

    let taken = email_change_surface(app.router(), &cookie, "taken@example.com").await;
    let untaken = email_change_surface(app.router(), &cookie, "nobody@example.com").await;

    assert_eq!(
        taken.0,
        StatusCode::ACCEPTED,
        "a capped request for a TAKEN address must not answer 409"
    );
    assert_eq!(untaken.0, StatusCode::ACCEPTED);
    assert_eq!(
        taken, untaken,
        "capped responses must be byte-identical regardless of the address's account status"
    );
    assert!(
        !taken
            .1
            .iter()
            .any(|(name, _)| name.eq_ignore_ascii_case("retry-after")),
        "a capped response carries no Retry-After"
    );
    assert_eq!(
        app.mailer.count(),
        mail_before,
        "neither capped request sent any mail"
    );
    assert_eq!(
        email_change_token_count(&app.pool).await,
        tokens_before,
        "neither capped request wrote a token"
    );
}
