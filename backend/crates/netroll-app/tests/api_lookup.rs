// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
//! Service-level integration tests for the provider-abstracted callbook lookup:
//! a real `LookupService` over a testcontainers Postgres, a known-KEK
//! `EnvelopeCipher`, the credential repo and a scripted `FakeEgress`. Asserts
//! observable behaviour — provider fallthrough, safe degradation, positive
//! caching, rate-limit skip — never storage internals or log strings.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use base64::Engine;
use netroll_adapters::crypto::{EnvelopeCipher, InstanceKek};
use netroll_app::http::AppState;
use netroll_app::lookup::LookupService;
use netroll_domain::egress::{Egress, EgressError, EgressRequest, EgressResponse};
use netroll_domain::lookup::{CallsignRecord, LookupError, LookupSource};
use netroll_domain::ports::{BoxFuture, CredentialCipher, LookupProvider, MailError, Mailer};
use netroll_domain::qrz::{QrzCredentials, parse_qrz_password, parse_qrz_username};
use sqlx::PgPool;
use sqlx::postgres::PgPoolOptions;
use testcontainers_modules::postgres::Postgres;
use testcontainers_modules::testcontainers::ContainerAsync;
use testcontainers_modules::testcontainers::runners::AsyncRunner;
use uuid::Uuid;

// --- Test KEK + mailer -------------------------------------------------------

fn test_kek() -> InstanceKek {
    let b64 = base64::engine::general_purpose::STANDARD.encode([42u8; 32]);
    InstanceKek::from_base64(&b64).expect("32-byte test KEK")
}

#[derive(Default)]
struct NoopMailer;
impl Mailer for NoopMailer {
    fn send_magic_link<'a>(
        &'a self,
        _to: &'a str,
        _link: &'a str,
    ) -> BoxFuture<'a, Result<(), MailError>> {
        Box::pin(async { Ok(()) })
    }
    fn send_email_change<'a>(
        &'a self,
        _to: &'a str,
        _link: &'a str,
    ) -> BoxFuture<'a, Result<(), MailError>> {
        Box::pin(async { Ok(()) })
    }
    fn send_email_change_notice<'a>(
        &'a self,
        _to: &'a str,
        _new: &'a str,
    ) -> BoxFuture<'a, Result<(), MailError>> {
        Box::pin(async { Ok(()) })
    }
}

// --- Scripted, capturing FakeEgress -----------------------------------------

enum Reply {
    Ok(u16, Vec<u8>),
    Err(EgressError),
}

struct FakeEgress {
    queue: Mutex<VecDeque<Reply>>,
    /// Returned when the scripted queue is exhausted (e.g. an "always 404"
    /// hamcall for the rate-limit test). `None` ⇒ an exhausted queue errors.
    default_ok: Option<(u16, Vec<u8>)>,
    requests: Mutex<Vec<EgressRequest>>,
    calls: AtomicUsize,
}

impl FakeEgress {
    fn scripted(replies: Vec<Reply>) -> Arc<Self> {
        Arc::new(Self {
            queue: Mutex::new(replies.into_iter().collect()),
            default_ok: None,
            requests: Mutex::new(Vec::new()),
            calls: AtomicUsize::new(0),
        })
    }

    fn always(status: u16, body: &str) -> Arc<Self> {
        Arc::new(Self {
            queue: Mutex::new(VecDeque::new()),
            default_ok: Some((status, body.as_bytes().to_vec())),
            requests: Mutex::new(Vec::new()),
            calls: AtomicUsize::new(0),
        })
    }

    fn call_count(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }

    fn urls(&self) -> Vec<String> {
        self.requests
            .lock()
            .unwrap()
            .iter()
            .map(|r| r.url.clone())
            .collect()
    }
}

impl Egress for FakeEgress {
    fn send<'a>(
        &'a self,
        req: EgressRequest,
    ) -> BoxFuture<'a, Result<EgressResponse, EgressError>> {
        Box::pin(async move {
            self.calls.fetch_add(1, Ordering::SeqCst);
            self.requests.lock().unwrap().push(req);
            match self.queue.lock().unwrap().pop_front() {
                Some(Reply::Ok(status, body)) => Ok(EgressResponse { status, body }),
                Some(Reply::Err(err)) => Err(err),
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

// --- Canned QRZ / hamcall bodies --------------------------------------------

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

fn qrz_not_found() -> Reply {
    Reply::Ok(
        200,
        br#"<QRZDatabase xmlns="http://xmldata.qrz.com"><Session>
            <Error>Not found: AA7BQ</Error><Key>SECRETKEY789ABCDEF</Key>
            </Session></QRZDatabase>"#
            .to_vec(),
    )
}

fn qrz_bad_creds() -> Reply {
    Reply::Ok(
        200,
        br#"<QRZDatabase xmlns="http://xmldata.qrz.com"><Session>
            <Error>Username/password incorrect</Error></Session></QRZDatabase>"#
            .to_vec(),
    )
}

const HAMCALL_HIT: &str =
    r#"{"callsign":"AA7BQ","first_name":"HAM","last_name":"CALL","city":"NEWINGTON","state":"CT"}"#;

// --- Harness -----------------------------------------------------------------

struct TestApp {
    _container: ContainerAsync<Postgres>,
    state: AppState,
    pool: PgPool,
}

/// Assertions in this file read only observable behavior via `egress` call
/// counts and returned records, never storage internals, so they survive a
/// future Valkey swap.
async fn test_app(egress: Arc<FakeEgress>, with_kek: bool) -> TestApp {
    let container = Postgres::default()
        .start()
        .await
        .expect("start postgres container");
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

    let kek = if with_kek { Some(test_kek()) } else { None };
    let state = AppState::new(
        pool.clone(),
        Arc::new(NoopMailer),
        "http://localhost:5173".into(),
    )
    .with_credential_cipher(Arc::new(EnvelopeCipher::new(kek)))
    .with_egress_for_tests(egress.clone());

    TestApp {
        _container: container,
        state,
        pool,
    }
}

fn creds() -> QrzCredentials {
    QrzCredentials {
        username: parse_qrz_username(USERNAME).unwrap(),
        password: parse_qrz_password(PASSWORD).unwrap(),
    }
}

/// Inserts a minimal account and stores its sealed QRZ credentials (sealed with
/// the known test KEK, the same key the app's cipher opens with).
async fn store_credentials(pool: &PgPool) -> Uuid {
    let account_id = Uuid::now_v7();
    sqlx::query("INSERT INTO accounts (id, email) VALUES ($1, $2)")
        .bind(account_id)
        .bind(format!("{account_id}@example.test"))
        .execute(pool)
        .await
        .expect("insert account");

    let cipher = EnvelopeCipher::new(Some(test_kek()));
    let sealed = cipher.seal(account_id, &creds()).expect("seal");
    netroll_adapters::pg::qrz_credentials::QrzCredentialRepo::new(pool.clone())
        .set(account_id, &sealed)
        .await
        .expect("store sealed credentials");
    account_id
}

// --- Provider abstraction ----------------------------------------------

/// A hand-written fake that never touches egress at all — proving `LookupService`
/// depends only on `Arc<dyn LookupProvider>`, not on `QrzLookupProvider`/
/// `HamcallLookupProvider` specifically.
struct AlwaysHit(CallsignRecord);
impl LookupProvider for AlwaysHit {
    fn lookup<'a>(
        &'a self,
        _callsign: &'a str,
        _credentials: Option<&'a QrzCredentials>,
    ) -> BoxFuture<'a, Result<Option<CallsignRecord>, LookupError>> {
        Box::pin(async move { Ok(Some(self.0.clone())) })
    }
}

struct AlwaysMiss;
impl LookupProvider for AlwaysMiss {
    fn lookup<'a>(
        &'a self,
        _callsign: &'a str,
        _credentials: Option<&'a QrzCredentials>,
    ) -> BoxFuture<'a, Result<Option<CallsignRecord>, LookupError>> {
        Box::pin(async move { Ok(None) })
    }
}

#[tokio::test]
async fn the_service_composes_with_hand_written_fake_providers_behind_the_trait() {
    // The verification text: "a test composes the service with fake
    // providers behind the trait". The other integration tests below all
    // exercise the REAL adapters (just over a FakeEgress); this one proves the
    // service itself is generic over `LookupProvider`, so a future third
    // adapter (e.g. HamQTH) drops in without `LookupService` changing.
    let egress = FakeEgress::scripted(vec![]); // never touched — fakes bypass egress entirely
    let app = test_app(egress, true).await;
    let account = Uuid::now_v7(); // no stored credentials -> QRZ skipped, hamcall tried

    let fake_record = CallsignRecord {
        callsign: "AA7BQ".into(),
        name: Some("FAKE PROVIDER".into()),
        location: None,
        grid: None,
        source: LookupSource::Hamcall,
    };
    let service = LookupService::new(
        Arc::new(AlwaysMiss),
        Arc::new(AlwaysHit(fake_record.clone())),
        app.state.qrz_credentials.clone(),
        app.state.credential_cipher.clone(),
        app.state.lookup_cache.clone(),
        app.state.lookup_limiter.clone(),
    );

    let result = service.lookup(account, "AA7BQ").await;
    assert_eq!(result, Some(fake_record));
}

// --- QRZ-then-hamcall fallback ----------------------------------------

#[tokio::test]
async fn qrz_hit_returns_the_qrz_record_and_never_calls_hamcall() {
    let egress = FakeEgress::scripted(vec![login_ok(), qrz_hit()]);
    let app = test_app(egress.clone(), true).await;
    let account = store_credentials(&app.pool).await;

    let record = app
        .state
        .lookup_service()
        .lookup(account, "aa7bq")
        .await
        .expect("a QRZ hit");

    assert_eq!(record.callsign, "AA7BQ");
    assert_eq!(record.name.as_deref(), Some("FRED LLOYD"));
    assert_eq!(record.location.as_deref(), Some("SCOTTSDALE, AZ"));
    assert_eq!(egress.call_count(), 2, "login + lookup only");
    assert!(
        egress.urls().iter().all(|u| !u.contains("hamcall.dev")),
        "hamcall not called on a QRZ hit"
    );
}

#[tokio::test]
async fn qrz_miss_falls_back_to_hamcall() {
    let egress = FakeEgress::scripted(vec![
        login_ok(),
        qrz_not_found(),
        Reply::Ok(200, HAMCALL_HIT.as_bytes().to_vec()),
    ]);
    let app = test_app(egress.clone(), true).await;
    let account = store_credentials(&app.pool).await;

    let record = app
        .state
        .lookup_service()
        .lookup(account, "AA7BQ")
        .await
        .expect("hamcall hit");

    assert_eq!(record.name.as_deref(), Some("HAM CALL"));
    assert_eq!(record.location.as_deref(), Some("NEWINGTON, CT"));
    assert_eq!(egress.call_count(), 3, "login, qrz-miss, hamcall");
    assert!(egress.urls()[2].contains("hamcall.dev"));
}

#[tokio::test]
async fn no_stored_credentials_skips_qrz_and_calls_hamcall_directly() {
    let egress = FakeEgress::scripted(vec![Reply::Ok(200, HAMCALL_HIT.as_bytes().to_vec())]);
    let app = test_app(egress.clone(), true).await;
    // A random account with NO stored credentials.
    let account = Uuid::now_v7();

    let record = app
        .state
        .lookup_service()
        .lookup(account, "AA7BQ")
        .await
        .expect("hamcall hit");

    assert_eq!(record.name.as_deref(), Some("HAM CALL"));
    assert_eq!(egress.call_count(), 1, "QRZ skipped: only hamcall called");
    assert!(egress.urls()[0].contains("hamcall.dev"));
}

// --- Safe degradation --------------------------------------------------

#[tokio::test]
async fn every_provider_failing_degrades_to_none_without_erroring() {
    let egress = FakeEgress::scripted(vec![
        login_ok(),
        Reply::Err(EgressError::Timeout), // QRZ lookup fails
        Reply::Err(EgressError::Timeout), // hamcall fails
    ]);
    let app = test_app(egress.clone(), true).await;
    let account = store_credentials(&app.pool).await;

    // The service signature is `Option`, never `Result` — a total failure is
    // simply `None` (the check-in proceeds on manual entry).
    let result = app.state.lookup_service().lookup(account, "AA7BQ").await;

    assert_eq!(result, None);
}

#[tokio::test]
async fn a_fail_closed_cipher_degrades_to_hamcall_only() {
    // Credentials ARE stored, but the app's cipher has no KEK, so `open` fails —
    // QRZ is skipped (never an error) and hamcall answers.
    let egress = FakeEgress::scripted(vec![Reply::Ok(200, HAMCALL_HIT.as_bytes().to_vec())]);
    let app = test_app(egress.clone(), false).await;
    let account = store_credentials(&app.pool).await;

    let record = app
        .state
        .lookup_service()
        .lookup(account, "AA7BQ")
        .await
        .expect("hamcall hit");

    assert_eq!(record.name.as_deref(), Some("HAM CALL"));
    assert_eq!(
        egress.call_count(),
        1,
        "KEK-unavailable ⇒ QRZ skipped, hamcall only"
    );
    assert!(egress.urls()[0].contains("hamcall.dev"));
}

// --- Caching + rate limiting ------------------------------------------

#[tokio::test]
async fn a_repeat_lookup_is_served_from_cache_with_no_second_upstream_call() {
    let egress = FakeEgress::scripted(vec![login_ok(), qrz_hit()]);
    let app = test_app(egress.clone(), true).await;
    let account = store_credentials(&app.pool).await;
    let service = app.state.lookup_service();

    let first = service.lookup(account, "AA7BQ").await.expect("hit");
    assert_eq!(egress.call_count(), 2);

    let second = service.lookup(account, "AA7BQ").await.expect("cached hit");
    assert_eq!(first, second);
    assert_eq!(
        egress.call_count(),
        2,
        "the repeat lookup issued NO new upstream call"
    );
}

#[tokio::test]
async fn an_over_threshold_burst_degrades_to_no_lookup() {
    // hamcall always 404 ⇒ every lookup is a miss (nothing cached), so each
    // distinct callsign spends one rate-limit cell and one egress call until the
    // limiter trips.
    let egress = FakeEgress::always(404, "not found");
    let app = test_app(egress.clone(), true).await;
    let account = Uuid::now_v7(); // no creds ⇒ hamcall-only, one call per lookup
    let service = app.state.lookup_service();

    // Burst is 30; the first 30 distinct lookups each reach hamcall.
    for n in 0..30 {
        assert_eq!(service.lookup(account, &format!("W{n}AAA")).await, None);
    }
    assert_eq!(
        egress.call_count(),
        30,
        "all 30 within-burst lookups reached hamcall"
    );

    // Further lookups are skipped — they return None WITHOUT any upstream call.
    for n in 30..40 {
        assert_eq!(service.lookup(account, &format!("W{n}AAA")).await, None);
    }
    assert_eq!(
        egress.call_count(),
        30,
        "over-threshold lookups issued no upstream call"
    );
}

// --- Never-logged secrets ---------------------------------------------

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

/// Installs a process-global tracing subscriber capturing to a shared buffer
/// (once). This test binary sets no other global default, so a single install
/// is safe.
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
async fn credentials_and_session_key_never_appear_in_logs() {
    let buf = install_log_capture();

    // Path 1: a full login + hit (a session key flows through the adapter).
    let egress = FakeEgress::scripted(vec![login_ok(), qrz_hit()]);
    let app = test_app(egress, true).await;
    let account = store_credentials(&app.pool).await;
    app.state
        .lookup_service()
        .lookup(account, "AA7BQ")
        .await
        .expect("hit");

    // Path 2: rejected credentials (exercises the service's debug signal, which
    // must carry the account id only — never the secret values).
    let egress2 = FakeEgress::scripted(vec![
        qrz_bad_creds(),
        Reply::Ok(200, HAMCALL_HIT.as_bytes().to_vec()),
    ]);
    let app2 = test_app(egress2, true).await;
    let account2 = store_credentials(&app2.pool).await;
    app2.state.lookup_service().lookup(account2, "AA7BQ").await;

    let logged = String::from_utf8_lossy(&buf.lock().unwrap()).into_owned();
    assert!(
        !logged.contains(USERNAME),
        "the QRZ username must never be logged"
    );
    assert!(
        !logged.contains(PASSWORD),
        "the QRZ password must never be logged"
    );
    assert!(
        !logged.contains(SESSION_KEY),
        "the session key must never be logged"
    );
}
