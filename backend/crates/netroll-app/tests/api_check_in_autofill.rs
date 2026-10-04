// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
//! Full-loop API tests for the check-in autofill endpoint
//! `GET /api/net-sessions/{id}/check-in-autofill?callsign=`. Real router, real
//! Postgres, a known-KEK `EnvelopeCipher` and a scripted `FakeEgress` standing
//! in for the callbook. Asserts observable behaviour — auth outcomes, the
//! best-effort empty shape, the callbook fill and the profile override.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use base64::Engine;
use netroll_adapters::crypto::{EnvelopeCipher, InstanceKek};
use netroll_app::http::{AppState, api_router};
use netroll_domain::consent::CURRENT_TERMS_VERSION;
use netroll_domain::egress::{Egress, EgressError, EgressRequest, EgressResponse};
use netroll_domain::ports::{BoxFuture, CredentialCipher, MailError, Mailer};
use netroll_domain::qrz::{QrzCredentials, parse_qrz_password, parse_qrz_username};
use serde_json::{Value, json};
use sqlx::PgPool;
use sqlx::postgres::PgPoolOptions;
use testcontainers_modules::postgres::Postgres;
use testcontainers_modules::testcontainers::ContainerAsync;
use testcontainers_modules::testcontainers::runners::AsyncRunner;
use tower::ServiceExt;
use uuid::Uuid;

// --- Test KEK + capturing mailer --------------------------------------------

fn test_kek() -> InstanceKek {
    let b64 = base64::engine::general_purpose::STANDARD.encode([42u8; 32]);
    InstanceKek::from_base64(&b64).expect("32-byte test KEK")
}

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

// --- Scripted, capturing FakeEgress -----------------------------------------

enum Reply {
    Ok(u16, Vec<u8>),
}

struct FakeEgress {
    queue: Mutex<VecDeque<Reply>>,
    default_ok: Option<(u16, Vec<u8>)>,
    calls: AtomicUsize,
}

impl FakeEgress {
    fn scripted(replies: Vec<Reply>) -> Arc<Self> {
        Arc::new(Self {
            queue: Mutex::new(replies.into_iter().collect()),
            default_ok: None,
            calls: AtomicUsize::new(0),
        })
    }

    fn always(status: u16, body: &str) -> Arc<Self> {
        Arc::new(Self {
            queue: Mutex::new(VecDeque::new()),
            default_ok: Some((status, body.as_bytes().to_vec())),
            calls: AtomicUsize::new(0),
        })
    }
}

impl Egress for FakeEgress {
    fn send<'a>(
        &'a self,
        _req: EgressRequest,
    ) -> BoxFuture<'a, Result<EgressResponse, EgressError>> {
        Box::pin(async move {
            self.calls.fetch_add(1, Ordering::SeqCst);
            match self.queue.lock().unwrap().pop_front() {
                Some(Reply::Ok(status, body)) => Ok(EgressResponse { status, body }),
                None => match &self.default_ok {
                    Some((status, body)) => Ok(EgressResponse {
                        status: *status,
                        body: body.clone(),
                    }),
                    None => Err(EgressError::Transport(
                        "fake egress: queue exhausted".into(),
                    )),
                },
            }
        })
    }
}

// --- Canned QRZ callbook bodies ---------------------------------------------

const SESSION_KEY: &str = "SECRETKEY789ABCDEF";
const USERNAME: &str = "SECRETUSER123";
const PASSWORD: &str = "SECRETPASS456";

fn login_ok() -> Reply {
    Reply::Ok(
        200,
        format!(
            r#"<QRZDatabase version="1.34" xmlns="http://xmldata.qrz.com">
               <Session><Key>{SESSION_KEY}</Key></Session></QRZDatabase>"#
        )
        .into_bytes(),
    )
}

/// A QRZ hit for AA7BQ: name "FRED LLOYD", location "SCOTTSDALE, AZ".
fn qrz_hit() -> Reply {
    Reply::Ok(
        200,
        br#"<QRZDatabase xmlns="http://xmldata.qrz.com"><Callsign>
            <call>AA7BQ</call><fname>FRED</fname><name>LLOYD</name>
            <addr2>SCOTTSDALE</addr2><state>AZ</state><grid>DM32af</grid>
            </Callsign><Session><Key>SECRETKEY789ABCDEF</Key></Session></QRZDatabase>"#
            .to_vec(),
    )
}

// --- Harness ----------------------------------------------------------------

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

async fn test_app(egress: Arc<FakeEgress>) -> TestApp {
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
    let state = AppState::new(pool.clone(), mailer.clone(), "http://localhost:5173".into())
        .with_credential_cipher(Arc::new(EnvelopeCipher::new(Some(test_kek()))))
        .with_egress_for_tests(egress);
    TestApp {
        _container: container,
        state,
        pool,
        mailer,
    }
}

async fn account_id_for(pool: &PgPool, email: &str) -> Uuid {
    sqlx::query_scalar::<_, Uuid>("SELECT id FROM accounts WHERE email = $1")
        .bind(email)
        .fetch_one(pool)
        .await
        .expect("account exists")
}

/// Seals QRZ credentials for `account_id` with the known test KEK so the acting
/// operator's callbook lookup resolves deterministically.
async fn store_credentials(pool: &PgPool, account_id: Uuid) {
    let creds = QrzCredentials {
        username: parse_qrz_username(USERNAME).unwrap(),
        password: parse_qrz_password(PASSWORD).unwrap(),
    };
    let cipher = EnvelopeCipher::new(Some(test_kek()));
    let sealed = cipher.seal(account_id, &creds).expect("seal");
    netroll_adapters::pg::qrz_credentials::QrzCredentialRepo::new(pool.clone())
        .set(account_id, &sealed)
        .await
        .expect("store sealed credentials");
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

async fn create_net(app: &TestApp, cookie: &str) -> String {
    let (status, body) = send_json(
        app.router(),
        "POST",
        "/api/net-definitions",
        Some(json!({
            "title": "Sunday Traffic Net",
            "connections": [
                { "kind": "hf", "plannedFrequencyHz": 14_230_000, "band": "20m", "mode": "ssb" }
            ],
            "netCategory": "traffic",
            "netType": "open"
        })),
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

async fn add_check_in(app: &TestApp, cookie: &str, session_id: &str, body: Value) {
    let (status, _) = send_json(
        app.router(),
        "POST",
        &format!("/api/net-sessions/{session_id}/check-ins"),
        Some(body),
        Some(cookie),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
}

async fn autofill(
    app: &TestApp,
    cookie: Option<&str>,
    session_id: &str,
    callsign: &str,
) -> (StatusCode, Value) {
    send_json(
        app.router(),
        "GET",
        &format!("/api/net-sessions/{session_id}/check-in-autofill?callsign={callsign}"),
        None,
        cookie,
    )
    .await
}

// --- Auth outcomes ---------------------------------------------

#[tokio::test]
async fn a_participant_cannot_autofill() {
    let app = test_app(FakeEgress::scripted(vec![])).await;
    let owner = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;
    let participant = sign_in_consent(&app, "participant@example.com").await;
    let definition_id = create_net(&app, &owner).await;
    let session_id = start_session(&app, &owner, &definition_id).await;

    let (status, body) = autofill(&app, Some(&participant), &session_id, "AA7BQ").await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(body["type"], "/errors/forbidden");
}

#[tokio::test]
async fn an_unauthenticated_request_is_rejected() {
    let app = test_app(FakeEgress::scripted(vec![])).await;
    let owner = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;
    let definition_id = create_net(&app, &owner).await;
    let session_id = start_session(&app, &owner, &definition_id).await;

    let (status, _) = autofill(&app, None, &session_id, "AA7BQ").await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn autofill_on_a_missing_session_is_404() {
    let app = test_app(FakeEgress::scripted(vec![])).await;
    let stranger = sign_in_consent(&app, "stranger@example.com").await;
    let missing = Uuid::now_v7();

    let (status, body) = autofill(&app, Some(&stranger), &missing.to_string(), "AA7BQ").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body["type"], "/errors/net-session-not-found");
}

// --- Best-effort empty shape ------------------------------------------

#[tokio::test]
async fn a_total_miss_returns_the_empty_shape() {
    // No stored credentials for the acting op ⇒ QRZ skipped; hamcall always 404
    // ⇒ callbook None. No profile owner, no roster ⇒ every source empty.
    let app = test_app(FakeEgress::always(404, "not found")).await;
    let owner = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;
    let definition_id = create_net(&app, &owner).await;
    let session_id = start_session(&app, &owner, &definition_id).await;

    let (status, body) = autofill(&app, Some(&owner), &session_id, "K9NEVER").await;
    assert_eq!(status, StatusCode::OK, "a miss is a 200, never an error");
    assert!(body["name"].is_null());
    assert!(body["location"].is_null());
}

#[tokio::test]
async fn a_blank_callsign_is_a_silent_no_op() {
    let app = test_app(FakeEgress::scripted(vec![])).await;
    let owner = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;
    let definition_id = create_net(&app, &owner).await;
    let session_id = start_session(&app, &owner, &definition_id).await;

    let (status, body) = autofill(&app, Some(&owner), &session_id, "").await;
    assert_eq!(status, StatusCode::OK);
    assert!(body["name"].is_null());
    assert!(body["location"].is_null());
}

// --- Callbook fill and profile override ------------------------

#[tokio::test]
async fn a_callbook_hit_with_no_profile_or_roster_returns_the_callbook_values() {
    // Control: with no profile owner and no roster memory, the
    // callbook (QRZ) values are what fills the autofill.
    let app = test_app(FakeEgress::scripted(vec![login_ok(), qrz_hit()])).await;
    let owner = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;
    let owner_id = account_id_for(&app.pool, "owner@example.com").await;
    store_credentials(&app.pool, owner_id).await;
    let definition_id = create_net(&app, &owner).await;
    let session_id = start_session(&app, &owner, &definition_id).await;

    let (status, body) = autofill(&app, Some(&owner), &session_id, "AA7BQ").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["name"], "FRED LLOYD");
    assert_eq!(body["location"], "SCOTTSDALE, AZ");
}

#[tokio::test]
async fn a_profile_overrides_the_callbook_values() {
    // The looked-up callsign's OWNER account has a populated NetRoll
    // profile whose values differ from the callbook. The profile wins.
    let app = test_app(FakeEgress::scripted(vec![login_ok(), qrz_hit()])).await;
    let owner = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;
    let owner_id = account_id_for(&app.pool, "owner@example.com").await;
    store_credentials(&app.pool, owner_id).await;

    // A DIFFERENT account owns AA7BQ and has a profile.
    let fred = sign_in_consent_callsign(&app, "fred@example.com", "aa7bq").await;
    set_profile(&app, &fred, "Profile Fred", "Profile City, ZZ").await;

    let definition_id = create_net(&app, &owner).await;
    let session_id = start_session(&app, &owner, &definition_id).await;

    let (status, body) = autofill(&app, Some(&owner), &session_id, "AA7BQ").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        body["name"], "Profile Fred",
        "the owner's profile overrides the callbook name"
    );
    assert_eq!(
        body["location"], "Profile City, ZZ",
        "the owner's profile overrides the callbook location"
    );
}

#[tokio::test]
async fn roster_memory_beats_the_callbook_for_this_net() {
    // End-to-end slice: a returning station remembered by THIS net's
    // roster-memory beats the external callbook, with no profile owner present.
    let app = test_app(FakeEgress::scripted(vec![login_ok(), qrz_hit()])).await;
    let owner = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;
    let owner_id = account_id_for(&app.pool, "owner@example.com").await;
    store_credentials(&app.pool, owner_id).await;
    let definition_id = create_net(&app, &owner).await;

    // Session 1: commit a remembered identity for AA7BQ, then close.
    let session1 = start_session(&app, &owner, &definition_id).await;
    add_check_in(
        &app,
        &owner,
        &session1,
        json!({ "callsign": "AA7BQ", "name": "Remembered Fred", "location": "Remembered City" }),
    )
    .await;
    let (status, _) = send_json(
        app.router(),
        "POST",
        &format!("/api/net-sessions/{session1}/close"),
        None,
        Some(&owner),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    // Session 2 of the SAME definition: the roster memory beats the callbook.
    let session2 = start_session(&app, &owner, &definition_id).await;
    let (status, body) = autofill(&app, Some(&owner), &session2, "AA7BQ").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["name"], "Remembered Fred");
    assert_eq!(body["location"], "Remembered City");
}

// --- No name/location leaks into logs ---------------------------------

#[derive(Clone)]
struct BufWriter(Arc<Mutex<Vec<u8>>>);
impl std::io::Write for BufWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for BufWriter {
    type Writer = BufWriter;
    fn make_writer(&'a self) -> Self::Writer {
        self.clone()
    }
}

static LOG_BUF: OnceLock<Arc<Mutex<Vec<u8>>>> = OnceLock::new();

fn install_log_capture() -> Arc<Mutex<Vec<u8>>> {
    LOG_BUF
        .get_or_init(|| {
            let buf = Arc::new(Mutex::new(Vec::new()));
            let subscriber = tracing_subscriber::fmt()
                .with_writer(BufWriter(buf.clone()))
                .with_max_level(tracing::Level::DEBUG)
                .finish();
            let _ = tracing::subscriber::set_global_default(subscriber);
            buf
        })
        .clone()
}

#[tokio::test]
async fn the_autofill_path_never_logs_name_or_location() {
    // Sentinel name/location cross the wire in the JSON body but must
    // never appear in emitted logs. Distinctive sentinels so a parallel test in
    // this binary cannot pollute the assertion.
    let buf = install_log_capture();

    let app = test_app(FakeEgress::always(404, "not found")).await;
    let owner = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;
    let fred = sign_in_consent_callsign(&app, "fred@example.com", "aa7bq").await;
    set_profile(&app, &fred, "ZzSentinelNameZz", "ZzSentinelLocZz").await;
    let definition_id = create_net(&app, &owner).await;
    let session_id = start_session(&app, &owner, &definition_id).await;

    let (status, body) = autofill(&app, Some(&owner), &session_id, "AA7BQ").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["name"], "ZzSentinelNameZz", "returned in the body");

    let logged = String::from_utf8_lossy(&buf.lock().unwrap()).into_owned();
    assert!(
        !logged.contains("ZzSentinelNameZz"),
        "the operator name must never be logged"
    );
    assert!(
        !logged.contains("ZzSentinelLocZz"),
        "the operator location must never be logged"
    );
}
