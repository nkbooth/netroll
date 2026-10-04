// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
//! Full-loop API tests for envelope-encrypted QRZ credential storage: real
//! router, real Postgres, capturing fake mailer, and a known injected test KEK.
//! Asserts the write-only surface, the at-rest-encryption invariant (no
//! plaintext in any column), the boot posture with no KEK (503), and the
//! set/replace/clear lifecycle — status codes and slugs, never message strings.

use std::sync::{Arc, Mutex};

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use base64::Engine;
use netroll_adapters::crypto::{EnvelopeCipher, InstanceKek};
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

/// A fixed, valid 32-byte test KEK (base64) — deterministic so tests are
/// reproducible; never a production key.
fn test_kek() -> InstanceKek {
    let b64 = base64::engine::general_purpose::STANDARD.encode([42u8; 32]);
    InstanceKek::from_base64(&b64).expect("32-byte test KEK")
}

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
    pool: PgPool,
    mailer: Arc<CapturingMailer>,
}

impl TestApp {
    fn router(&self) -> Router {
        api_router(self.state.clone())
    }
}

/// Builds a test app; `with_kek` selects a working cipher (a known test KEK) or
/// the fail-closed no-KEK cipher (the KEK-absent boot posture).
async fn test_app_with_kek(with_kek: bool) -> TestApp {
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
    let kek = if with_kek { Some(test_kek()) } else { None };
    let state = AppState::new(pool.clone(), mailer.clone(), "http://localhost:5173".into())
        .with_credential_cipher(Arc::new(EnvelopeCipher::new(kek)));
    TestApp {
        _container: container,
        state,
        pool,
        mailer,
    }
}

async fn test_app() -> TestApp {
    test_app_with_kek(true).await
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

/// Signs a fresh email in and records consent (the QRZ surface is gated on
/// `ConsentedAccount`, the callsign/profile posture). Returns the session cookie.
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
        .expect("build session request");
    let response = app.router().oneshot(request).await.expect("route");
    assert_eq!(response.status(), StatusCode::CREATED);
    let cookie = response
        .headers()
        .get(header::SET_COOKIE)
        .expect("201 sets the session cookie")
        .to_str()
        .expect("cookie ascii")
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

/// Whether `needle` appears anywhere in `haystack` as a contiguous subslice.
fn bytes_contain(haystack: &[u8], needle: &[u8]) -> bool {
    !needle.is_empty() && haystack.windows(needle.len()).any(|w| w == needle)
}

const CALLSIGN: &str = "W1AW";
const PASSWORD: &str = "topSecretPassphrase99";

#[tokio::test]
async fn put_seals_stores_no_plaintext_and_marks_credentials_set() {
    let app = test_app().await;
    let cookie = sign_in_and_consent(&app, "op@example.com").await;

    let (status, _) = send_json(
        app.router(),
        "PUT",
        "/api/accounts/me/qrz-credentials",
        Some(json!({ "callsign": CALLSIGN, "password": PASSWORD })),
        Some(&cookie),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    // /me now reports the boolean and NO credential value.
    let (status, me) =
        send_json(app.router(), "GET", "/api/accounts/me", None, Some(&cookie)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(me["qrzCredentialsSet"], true);
    let serialized = me.to_string();
    assert!(
        !serialized.contains(PASSWORD),
        "the /me body must never carry the password"
    );

    // No stored bytea column contains the plaintext password or callsign.
    let account = app
        .state
        .accounts
        .find_by_email("op@example.com")
        .await
        .expect("lookup")
        .expect("account");
    let row: (Vec<u8>, Vec<u8>, Vec<u8>, Vec<u8>) = sqlx::query_as(
        "SELECT wrapped_dek, dek_nonce, credential_ciphertext, credential_nonce
         FROM qrz_credentials WHERE account_id = $1",
    )
    .bind(account.id)
    .fetch_one(&app.pool)
    .await
    .expect("read raw row");
    for column in [&row.0, &row.1, &row.2, &row.3] {
        assert!(
            !bytes_contain(column, PASSWORD.as_bytes()),
            "no stored column may contain the plaintext password"
        );
        assert!(
            !bytes_contain(column, CALLSIGN.as_bytes()),
            "no stored column may contain the plaintext callsign"
        );
    }

    // The stored blob opens back to exactly the submitted credentials.
    let sealed = app
        .state
        .qrz_credentials
        .get(account.id)
        .await
        .expect("get")
        .expect("sealed row");
    let opened = app
        .state
        .credential_cipher
        .open(account.id, &sealed)
        .expect("open round-trips");
    assert_eq!(opened.username.as_str(), CALLSIGN);
    assert_eq!(opened.password.as_str(), PASSWORD);
}

#[tokio::test]
async fn clear_flips_the_flag_and_no_endpoint_ever_returns_the_password() {
    let app = test_app().await;
    let cookie = sign_in_and_consent(&app, "op@example.com").await;

    let (status, _) = send_json(
        app.router(),
        "PUT",
        "/api/accounts/me/qrz-credentials",
        Some(json!({ "callsign": CALLSIGN, "password": PASSWORD })),
        Some(&cookie),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    let (_, me) = send_json(app.router(), "GET", "/api/accounts/me", None, Some(&cookie)).await;
    assert_eq!(me["qrzCredentialsSet"], true);
    assert!(!me.to_string().contains(PASSWORD));

    let (status, _) = send_json(
        app.router(),
        "DELETE",
        "/api/accounts/me/qrz-credentials",
        None,
        Some(&cookie),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    let (_, me) = send_json(app.router(), "GET", "/api/accounts/me", None, Some(&cookie)).await;
    assert_eq!(me["qrzCredentialsSet"], false, "clear resets the boolean");
    assert!(!me.to_string().contains(PASSWORD));
}

#[tokio::test]
async fn replace_generates_fresh_material_and_keeps_one_row() {
    let app = test_app().await;
    let cookie = sign_in_and_consent(&app, "op@example.com").await;

    for pw in [PASSWORD, "aCompletelyDifferentPw"] {
        let (status, _) = send_json(
            app.router(),
            "PUT",
            "/api/accounts/me/qrz-credentials",
            Some(json!({ "callsign": CALLSIGN, "password": pw })),
            Some(&cookie),
        )
        .await;
        assert_eq!(status, StatusCode::NO_CONTENT);
    }

    let account = app
        .state
        .accounts
        .find_by_email("op@example.com")
        .await
        .expect("lookup")
        .expect("account");
    let count: i64 =
        sqlx::query_scalar("SELECT count(*) FROM qrz_credentials WHERE account_id = $1")
            .bind(account.id)
            .fetch_one(&app.pool)
            .await
            .expect("count");
    assert_eq!(count, 1, "replace keeps the 1:1 row");

    // The current row opens to the SECOND password (fresh DEK/nonce on replace).
    let sealed = app
        .state
        .qrz_credentials
        .get(account.id)
        .await
        .expect("get")
        .expect("row");
    let opened = app
        .state
        .credential_cipher
        .open(account.id, &sealed)
        .expect("open");
    assert_eq!(opened.password.as_str(), "aCompletelyDifferentPw");
}

#[tokio::test]
async fn without_a_kek_put_is_503_crypto_unavailable_but_other_routes_serve() {
    let app = test_app_with_kek(false).await;
    let cookie = sign_in_and_consent(&app, "op@example.com").await;

    let (status, problem) = send_json(
        app.router(),
        "PUT",
        "/api/accounts/me/qrz-credentials",
        Some(json!({ "callsign": CALLSIGN, "password": PASSWORD })),
        Some(&cookie),
    )
    .await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(problem["type"], "/errors/crypto-unavailable");

    // The app still boots and serves every non-QRZ route; nothing was stored.
    let (status, me) =
        send_json(app.router(), "GET", "/api/accounts/me", None, Some(&cookie)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(me["qrzCredentialsSet"], false);
    assert_eq!(me["email"], "op@example.com");
}

/// A `tracing` writer that appends every emitted line to a shared buffer, so a
/// test can assert what did (and did not) reach the logs.
#[derive(Clone)]
struct BufferWriter(Arc<Mutex<Vec<u8>>>);

impl std::io::Write for BufferWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0
            .lock()
            .expect("log buffer lock")
            .extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl tracing_subscriber::fmt::MakeWriter<'_> for BufferWriter {
    type Writer = BufferWriter;
    fn make_writer(&self) -> Self::Writer {
        self.clone()
    }
}

#[tokio::test]
async fn the_password_never_appears_in_tracing_output_across_a_full_put() {
    let app = test_app().await;

    // Capture all tracing for this (current-thread) test via a thread-local
    // default subscriber writing into a buffer.
    let buffer = Arc::new(Mutex::new(Vec::new()));
    let subscriber = tracing_subscriber::fmt()
        .with_writer(BufferWriter(buffer.clone()))
        .with_max_level(tracing::Level::TRACE)
        .with_ansi(false)
        .finish();
    let guard = tracing::subscriber::set_default(subscriber);

    let cookie = sign_in_and_consent(&app, "op@example.com").await;
    let (status, _) = send_json(
        app.router(),
        "PUT",
        "/api/accounts/me/qrz-credentials",
        Some(json!({ "callsign": CALLSIGN, "password": PASSWORD })),
        Some(&cookie),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    drop(guard);

    let logs =
        String::from_utf8(buffer.lock().expect("log buffer lock").clone()).expect("logs are utf-8");
    // The capture is real (the set path emitted its audit line) yet the secret
    // is absent from every line — the redacting Debug + no-body-logging hold.
    assert!(
        logs.contains("qrz credentials set"),
        "the set path must have emitted its audit line (proves the capture is live)"
    );
    assert!(
        !logs.contains(PASSWORD),
        "the submitted password must appear in NO emitted log line"
    );
}

#[tokio::test]
async fn invalid_input_is_422_and_writes_no_row() {
    let app = test_app().await;
    let cookie = sign_in_and_consent(&app, "op@example.com").await;

    for bad in [
        json!({ "callsign": CALLSIGN, "password": "" }),
        json!({ "callsign": "   ", "password": PASSWORD }),
        json!({ "callsign": CALLSIGN, "password": "x".repeat(129) }),
    ] {
        let (status, problem) = send_json(
            app.router(),
            "PUT",
            "/api/accounts/me/qrz-credentials",
            Some(bad),
            Some(&cookie),
        )
        .await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(problem["type"], "/errors/qrz-credentials-invalid");
    }

    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM qrz_credentials")
        .fetch_one(&app.pool)
        .await
        .expect("count");
    assert_eq!(count, 0, "a rejected PUT must not write a row");
}
