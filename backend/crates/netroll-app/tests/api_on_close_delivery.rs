// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
//! Full-loop API tests for on-close email + HMAC webhook delivery: real router,
//! real Postgres, capturing fake `Mailer` and `Egress`. Covers both close paths,
//! signature re-verification against the captured bytes, bounded at-least-once
//! retry with per-target isolation, the delivery-off no-op, and
//! read-never-regenerate of the secret. Asserts state, bytes and counts.

use std::collections::HashSet;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use netroll_app::http::{AppState, api_router};
use netroll_app::presence_monitor::run_presence_monitor_tick;
use netroll_domain::consent::CURRENT_TERMS_VERSION;
use netroll_domain::egress::{Egress, EgressError, EgressRequest, EgressResponse};
use netroll_domain::net::delivery::sign_webhook;
use netroll_domain::ports::{BoxFuture, MailError, Mailer, NetSummaryMail};
use serde_json::{Value, json};
use sqlx::PgPool;
use sqlx::postgres::PgPoolOptions;
use testcontainers_modules::postgres::Postgres;
use testcontainers_modules::testcontainers::ContainerAsync;
use testcontainers_modules::testcontainers::runners::AsyncRunner;
use tower::ServiceExt;

// --- Capturing fake Mailer --------------------------------------------------

#[derive(Default)]
struct CapturingMailer {
    /// Sign-in links, so the harness can extract the magic-link token.
    magic: Mutex<Vec<String>>,
    /// Recorded net-summary sends as `(to, subject)`.
    summaries: Mutex<Vec<(String, String)>>,
    /// Addresses whose summary send always errors (the permanent-fail target).
    fail: Mutex<HashSet<String>>,
    /// When set, every summary send blocks on this gate — used to prove
    /// delivery is fire-and-forget (the close response resolves while a send is
    /// still pending). Never released during the assertion.
    gate: Mutex<Option<Arc<tokio::sync::Notify>>>,
}

impl CapturingMailer {
    fn last_link(&self) -> String {
        self.magic
            .lock()
            .expect("lock")
            .last()
            .expect("a magic link was sent")
            .clone()
    }
    fn summary_recipients(&self) -> Vec<String> {
        self.summaries
            .lock()
            .expect("lock")
            .iter()
            .map(|(to, _)| to.clone())
            .collect()
    }
    fn summary_count(&self) -> usize {
        self.summaries.lock().expect("lock").len()
    }
}

impl Mailer for CapturingMailer {
    fn send_magic_link<'a>(
        &'a self,
        _to: &'a str,
        link: &'a str,
    ) -> BoxFuture<'a, Result<(), MailError>> {
        Box::pin(async move {
            self.magic.lock().expect("lock").push(link.to_owned());
            Ok(())
        })
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
        _new_email: &'a str,
    ) -> BoxFuture<'a, Result<(), MailError>> {
        Box::pin(async { Ok(()) })
    }
    fn send_net_summary<'a>(
        &'a self,
        to: &'a str,
        summary: &'a NetSummaryMail,
    ) -> BoxFuture<'a, Result<(), MailError>> {
        Box::pin(async move {
            let gate = self.gate.lock().expect("lock").clone();
            if let Some(gate) = gate {
                // Blocks the send indefinitely (the fire-and-forget probe).
                gate.notified().await;
            }
            if self.fail.lock().expect("lock").contains(to) {
                return Err(MailError("recipient undeliverable".into()));
            }
            self.summaries
                .lock()
                .expect("lock")
                .push((to.to_owned(), summary.subject.clone()));
            Ok(())
        })
    }
}

// --- Capturing fake Egress --------------------------------------------------

struct FakeEgress {
    /// Every captured request with the instant it arrived. The instant is what
    /// makes the retry assertions non-vacuous: the observable is the
    /// GAP BETWEEN two attempts, not the total wall time of the close, which is
    /// dominated by request handling and would pass on `RETRY_BACKOFF` alone.
    requests: Mutex<Vec<(EgressRequest, Instant)>>,
    calls: AtomicUsize,
    /// The first `send` returns a transient error, later calls succeed — the
    /// retry probe.
    fail_first: bool,
    /// Canned `(status, body)` replies for requests whose URL CONTAINS the key,
    /// consumed front-to-back per key; an exhausted (or absent) script falls
    /// back to the default 200. Needed because the Discord probes want a
    /// per-target status the two boolean switches above cannot express.
    scripted: Mutex<Vec<(String, u16, Vec<u8>)>>,
    /// URL substrings whose every send is a PERMANENT egress refusal.
    refuse: Mutex<Vec<String>>,
}

impl FakeEgress {
    fn new(fail_first: bool) -> Arc<Self> {
        Arc::new(Self {
            requests: Mutex::new(Vec::new()),
            calls: AtomicUsize::new(0),
            fail_first,
            scripted: Mutex::new(Vec::new()),
            refuse: Mutex::new(Vec::new()),
        })
    }
    fn ok() -> Arc<Self> {
        Self::new(false)
    }
    fn fail_first_then_ok() -> Arc<Self> {
        Self::new(true)
    }
    /// Queues canned replies matched by URL substring, in order.
    fn scripting(entries: &[(&str, u16, &str)]) -> Arc<Self> {
        let fake = Self::new(false);
        *fake.scripted.lock().expect("lock") = entries
            .iter()
            .map(|(needle, status, body)| ((*needle).to_owned(), *status, body.as_bytes().to_vec()))
            .collect();
        fake
    }
    /// Every send to a URL containing `needle` is a permanent refusal.
    fn refusing(needle: &str) -> Arc<Self> {
        let fake = Self::new(false);
        fake.refuse.lock().expect("lock").push(needle.to_owned());
        fake
    }
    fn requests(&self) -> Vec<EgressRequest> {
        self.requests
            .lock()
            .expect("lock")
            .iter()
            .map(|(req, _)| req.clone())
            .collect()
    }
    /// The captured requests whose URL contains `needle`, with their arrival
    /// instants — the POSITIVE way to locate one target's attempts in a
    /// multi-target delivery: a negative header assertion made against
    /// "whatever request happened to be first" is not an assertion.
    fn attempts_to(&self, needle: &str) -> Vec<(EgressRequest, Instant)> {
        self.requests
            .lock()
            .expect("lock")
            .iter()
            .filter(|(req, _)| req.url.contains(needle))
            .cloned()
            .collect()
    }
}

impl Egress for FakeEgress {
    fn send<'a>(
        &'a self,
        req: EgressRequest,
    ) -> BoxFuture<'a, Result<EgressResponse, EgressError>> {
        Box::pin(async move {
            let n = self.calls.fetch_add(1, Ordering::SeqCst);
            let url = req.url.clone();
            self.requests
                .lock()
                .expect("lock")
                .push((req, Instant::now()));
            if self
                .refuse
                .lock()
                .expect("lock")
                .iter()
                .any(|needle| url.contains(needle.as_str()))
            {
                // Permanent by `is_transient_egress`'s classification.
                return Err(EgressError::BlockedAddress);
            }
            if self.fail_first && n == 0 {
                // Transient — the deliverer retries with the identical request.
                return Err(EgressError::Timeout);
            }
            let scripted = {
                let mut queue = self.scripted.lock().expect("lock");
                match queue.iter().position(|(needle, _, _)| url.contains(needle)) {
                    Some(at) => {
                        let (_, status, body) = queue.remove(at);
                        Some((status, body))
                    }
                    None => None,
                }
            };
            match scripted {
                Some((status, body)) => Ok(EgressResponse { status, body }),
                None => Ok(EgressResponse {
                    status: 200,
                    body: Vec::new(),
                }),
            }
        })
    }
}

fn header_value<'a>(req: &'a EgressRequest, name: &str) -> Option<&'a str> {
    req.headers
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case(name))
        .map(|(_, v)| v.as_str())
}

// --- Harness ----------------------------------------------------------------

struct TestApp {
    _container: ContainerAsync<Postgres>,
    state: AppState,
    mailer: Arc<CapturingMailer>,
    egress: Arc<FakeEgress>,
    pool: PgPool,
}

impl TestApp {
    fn router(&self) -> Router {
        api_router(self.state.clone())
    }
}

async fn test_app() -> TestApp {
    test_app_with_egress(FakeEgress::ok()).await
}

async fn test_app_with_egress(egress: Arc<FakeEgress>) -> TestApp {
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
    let egress_dyn: Arc<dyn Egress + Send + Sync> = egress.clone();
    let state = AppState::new(pool.clone(), mailer.clone(), "http://localhost:5173".into())
        .with_egress_for_tests(egress_dyn);
    TestApp {
        _container: container,
        state,
        mailer,
        egress,
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

fn minimal_definition_json() -> Value {
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
        Some(minimal_definition_json()),
        Some(cookie),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    body["id"].as_str().expect("id").to_owned()
}

async fn set_delivery_config(app: &TestApp, cookie: &str, definition_id: &str, config: Value) {
    let (status, _) = send_json(
        app.router(),
        "PUT",
        &format!("/api/net-definitions/{definition_id}/delivery-config"),
        Some(config),
        Some(cookie),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
}

/// Starts a live session for `definition_id` and returns its id.
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

/// Checks a station in with a POPULATED Maidenhead grid and returns the grid as
/// the server canonicalized it. Every other test in this file closes a session
/// with an EMPTY roster; the signed body must actually carry a roster
/// field, so this is the first check-in helper here.
async fn check_in_with_grid(
    app: &TestApp,
    cookie: &str,
    session_id: &str,
    callsign: &str,
    grid: &str,
) -> String {
    let (status, body) = send_json(
        app.router(),
        "POST",
        &format!("/api/net-sessions/{session_id}/check-ins"),
        Some(json!({ "callsign": callsign, "location": "Hartford, CT", "grid": grid })),
        Some(cookie),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    // Found by CALLSIGN, not by `roster[0]`: the helper is parameterized by one,
    // so a second call must not silently read the first station's grid back.
    body["roster"]
        .as_array()
        .expect("roster")
        .iter()
        .find(|entry| entry["callsign"] == callsign.to_uppercase())
        .expect("the station just checked in")["grid"]
        .as_str()
        .expect("the owner wire carries the canonicalized grid")
        .to_owned()
}

async fn close_session(app: &TestApp, cookie: &str, session_id: &str) -> StatusCode {
    let (status, _) = send_json(
        app.router(),
        "POST",
        &format!("/api/net-sessions/{session_id}/close"),
        None,
        Some(cookie),
    )
    .await;
    status
}

async fn webhook_secret_in_db(app: &TestApp, definition_id: &str) -> Option<String> {
    sqlx::query_scalar(
        "SELECT webhook_secret FROM net_delivery_configs WHERE definition_id = $1::uuid",
    )
    .bind(definition_id)
    .fetch_optional(&app.pool)
    .await
    .expect("read secret")
    .flatten()
}

/// Drives one presence-monitor tick with the standard thresholds and the app's
/// real delivery service (the fake-backed one) — the no-sleep auto-close seam.
async fn tick_monitor(app: &TestApp, now_millis: u64) {
    run_presence_monitor_tick(
        &app.state.net_sessions,
        &app.state.hub,
        &app.state.session_presence,
        now_millis,
        90_000,
        900_000,
        &app.state.delivery_service(),
    )
    .await
    .expect("monitor tick");
}

/// Polls `check` (yielding to let the spawned delivery task run) until it holds
/// or a bounded timeout elapses — the deliverer is fire-and-forget, so a test
/// observes its effect asynchronously without a fixed sleep.
async fn eventually(check: impl Fn() -> bool) {
    for _ in 0..300 {
        if check() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("delivery effect not observed within the bounded wait");
}

/// Waits until every delivery leg planned for `session_id` has left `pending`
/// — the exact "is the delivery finished?" signal durable job rows
/// provide. Replaces, for the test that needed it, a gate on SIDE EFFECTS
/// (a mailer count, one target's attempt count) that could be satisfied while a
/// concurrent sibling leg had not yet recorded its own attempt.
async fn eventually_settled(app: &TestApp, session_id: &str) {
    let id = uuid::Uuid::parse_str(session_id).expect("session id is a uuid");
    for _ in 0..300 {
        let (total, pending): (i64, i64) = sqlx::query_as(
            "SELECT count(*), count(*) FILTER (WHERE state = 'pending')
               FROM net_delivery_jobs WHERE session_id = $1",
        )
        .bind(id)
        .fetch_one(&app.pool)
        .await
        .expect("count delivery legs");
        if total > 0 && pending == 0 {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("delivery legs did not all settle within the bounded wait");
}

/// A brief settling window (three backoffs' worth) used by NEGATIVE assertions —
/// "nothing was delivered" — to give any (erroneously spawned) delivery a chance
/// to run before we assert it did not.
async fn settle() {
    for _ in 0..10 {
        tokio::task::yield_now().await;
    }
    tokio::time::sleep(Duration::from_millis(120)).await;
}

// --- Emails on both close paths ---------------------------------------

#[tokio::test]
async fn manual_close_emails_each_configured_address_once() {
    let app = test_app().await;
    let owner = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;
    let definition_id = create_net(&app, &owner).await;
    set_delivery_config(
        &app,
        &owner,
        &definition_id,
        json!({ "emails": ["log@example.com", "alerts@example.org"], "webhookUrl": null }),
    )
    .await;
    let session_id = start_session(&app, &owner, &definition_id).await;

    assert_eq!(
        close_session(&app, &owner, &session_id).await,
        StatusCode::OK
    );

    eventually(|| app.mailer.summary_count() == 2).await;
    let mut recipients = app.mailer.summary_recipients();
    recipients.sort();
    assert_eq!(
        recipients,
        vec![
            "alerts@example.org".to_owned(),
            "log@example.com".to_owned()
        ],
        "each configured, normalized address received exactly one summary"
    );
}

#[tokio::test]
async fn the_auto_close_sweep_path_also_delivers_the_summary() {
    // The second path: a net abandoned by its NCS still delivers when the
    // presence monitor auto-closes it. Drive stall then auto-close via ticks.
    let app = test_app().await;
    let owner = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;
    let definition_id = create_net(&app, &owner).await;
    set_delivery_config(
        &app,
        &owner,
        &definition_id,
        json!({ "emails": ["log@example.com"], "webhookUrl": null }),
    )
    .await;
    let _session_id = start_session(&app, &owner, &definition_id).await;

    // No presence heartbeat → the session stalls, then auto-closes past the
    // 15-minute window. The auto-close is the SHIPPED close write → delivery.
    tick_monitor(&app, 1_000_000).await;
    tick_monitor(&app, 1_000_000 + 900_000).await;

    eventually(|| app.mailer.summary_count() == 1).await;
    assert_eq!(app.mailer.summary_recipients(), vec!["log@example.com"]);
}

// --- Webhook POST, verifiable signature, secret read not minted --

#[tokio::test]
async fn close_posts_a_webhook_whose_signature_reverifies_against_the_stored_secret() {
    let app = test_app().await;
    let owner = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;
    let definition_id = create_net(&app, &owner).await;
    set_delivery_config(
        &app,
        &owner,
        &definition_id,
        json!({ "emails": [], "webhookUrl": "https://hooks.example.com/net" }),
    )
    .await;
    let secret_before = webhook_secret_in_db(&app, &definition_id)
        .await
        .expect("a webhook secret was minted at config time");
    let session_id = start_session(&app, &owner, &definition_id).await;
    // The signature assertion below must run against a body
    // that actually CARRIES a roster field, not an empty roster. `fn31PR` comes
    // back canonicalized as `FN31pr`.
    let grid = check_in_with_grid(&app, &owner, &session_id, "N1CCK", "fn31PR").await;
    assert_eq!(grid, "FN31pr", "the fixture genuinely has a grid");

    assert_eq!(
        close_session(&app, &owner, &session_id).await,
        StatusCode::OK
    );
    eventually(|| !app.egress.requests().is_empty()).await;

    let requests = app.egress.requests();
    assert_eq!(requests.len(), 1, "exactly one webhook POST");
    let req = &requests[0];
    assert_eq!(req.url, "https://hooks.example.com/net");
    assert_eq!(
        header_value(req, "content-type"),
        Some("application/json"),
        "the webhook body is JSON"
    );
    let delivery_id =
        header_value(req, "X-NetRoll-Delivery-Id").expect("carries the idempotency key");
    assert!(!delivery_id.is_empty());
    let signature = header_value(req, "X-NetRoll-Signature").expect("carries the signature");
    let body = req.body.as_deref().expect("the POST carries a body");

    // Recompute the HMAC over the STORED secret and the EXACT captured
    // body bytes; it must equal the header the deliverer sent.
    assert_eq!(
        signature,
        sign_webhook(&secret_before, body),
        "the signature verifies against the stored secret and the literal wire bytes"
    );
    assert!(signature.starts_with("sha256="));

    // The payload is the folded projection (camelCase, net identity + summary).
    let payload: Value = serde_json::from_slice(body).expect("body is JSON");
    assert_eq!(payload["net"]["title"], "Sunday Traffic Net");
    // Over the real HTTP loop and the real HMAC: the whole
    // connection set rides the published contract, and the two fields that
    // described one way to reach a net are gone (receiver-visible).
    assert_eq!(payload["connections"][0]["kind"], "hf");
    assert_eq!(payload["connections"][0]["band"], "20m");
    assert!(payload.get("operatingFrequencyHz").is_none());
    assert!(payload["net"].get("band").is_none());
    assert_eq!(payload["sessionId"], session_id);
    // The fixture now checks one station in, so the count is assertable rather
    // than merely present.
    assert_eq!(payload["participantCount"], 1);
    // The bytes that just re-verified are the bytes
    // carrying `grid` — the field the payload gained is inside the digest, not
    // beside it.
    assert_eq!(
        payload["roster"][0]["grid"], "FN31pr",
        "the signed body is the one carrying the per-check-in grid"
    );

    // The close READS the secret and never regenerates it — the stored
    // secret is byte-stable across a delivery.
    let secret_after = webhook_secret_in_db(&app, &definition_id).await;
    assert_eq!(
        secret_after.as_deref(),
        Some(secret_before.as_str()),
        "no new secret is minted by a close"
    );
}

#[tokio::test]
async fn the_webhook_labels_each_station_with_the_frequency_it_was_worked_on_while_connections_reports_where_the_net_ended()
 {
    // The webhook half — both facts in ONE signed body.
    // A station's `viaLabel` names the frequency it was worked on.
    // The top-level `connections` array names where each way in
    // ENDED. The fixture QSYs between two check-ins on the same connection so
    // the two `viaLabel`s differ and the array agrees with only the second.
    let app = test_app().await;
    let owner = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;
    let definition_id = create_net(&app, &owner).await;
    set_delivery_config(
        &app,
        &owner,
        &definition_id,
        json!({ "emails": [], "webhookUrl": "https://hooks.example.com/net" }),
    )
    .await;
    let (status, definition) = send_json(
        app.router(),
        "GET",
        &format!("/api/net-definitions/{definition_id}"),
        None,
        Some(&owner),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let hf = definition["connections"][0]["id"]
        .as_str()
        .expect("connection id")
        .to_owned();
    let via = json!({ "kind": "connection", "connectionId": hf });
    let session_id = start_session(&app, &owner, &definition_id).await;

    let app_ref = &app;
    let check_in_over_hf = |callsign: &'static str| {
        let via = via.clone();
        let session_id = session_id.clone();
        let owner = owner.clone();
        async move {
            let (status, body) = send_json(
                app_ref.router(),
                "POST",
                &format!("/api/net-sessions/{session_id}/check-ins"),
                Some(json!({ "callsign": callsign, "via": via })),
                Some(&owner),
            )
            .await;
            assert_eq!(status, StatusCode::CREATED, "{body}");
        }
    };
    check_in_over_hf("N1CCK").await;
    let (status, body) = send_json(
        app.router(),
        "POST",
        &format!("/api/net-sessions/{session_id}/frequency"),
        Some(json!({ "connectionId": hf, "operatingFrequency": "14.250" })),
        Some(&owner),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    check_in_over_hf("W1ABC").await;

    assert_eq!(
        close_session(&app, &owner, &session_id).await,
        StatusCode::OK
    );
    eventually(|| !app.egress.requests().is_empty()).await;
    let requests = app.egress.requests();
    assert_eq!(requests.len(), 1, "exactly one webhook POST");
    let body = requests[0]
        .body
        .as_deref()
        .expect("the POST carries a body");
    let payload: Value = serde_json::from_slice(body).expect("body is JSON");

    let roster = payload["roster"].as_array().expect("roster");
    assert_eq!(roster.len(), 2);
    assert_eq!(roster[0]["callsign"], "N1CCK");
    assert_eq!(roster[1]["callsign"], "W1ABC");
    assert_ne!(
        roster[0]["viaLabel"], roster[1]["viaLabel"],
        "two stations either side of a QSY must not read the same frequency"
    );
    assert_eq!(roster[0]["viaLabel"], "HF — 14.230 MHz");
    assert_eq!(roster[1]["viaLabel"], "HF — 14.250 MHz");
    // half of the same payload: the array is FINAL.
    assert_eq!(
        payload["connections"][0]["plannedFrequencyHz"], 14_250_000,
        "the top-level connections array reports where the way in ended"
    );
}

// --- At-least-once retry, stable delivery-id, per-target isolation -----

#[tokio::test]
async fn a_transient_webhook_failure_is_retried_with_the_same_delivery_id_and_body() {
    let app = test_app_with_egress(FakeEgress::fail_first_then_ok()).await;
    let owner = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;
    let definition_id = create_net(&app, &owner).await;
    set_delivery_config(
        &app,
        &owner,
        &definition_id,
        json!({ "emails": [], "webhookUrl": "https://hooks.example.com/net" }),
    )
    .await;
    let session_id = start_session(&app, &owner, &definition_id).await;

    assert_eq!(
        close_session(&app, &owner, &session_id).await,
        StatusCode::OK
    );
    // First attempt fails transiently, the retry succeeds → two captured sends.
    eventually(|| app.egress.requests().len() == 2).await;

    let requests = app.egress.requests();
    let first = &requests[0];
    let retry = &requests[1];
    assert_eq!(
        header_value(first, "X-NetRoll-Delivery-Id"),
        header_value(retry, "X-NetRoll-Delivery-Id"),
        "every attempt carries the SAME idempotency key so a receiver dedupes"
    );
    assert_eq!(
        first.body, retry.body,
        "the retry reuses the identical signed bytes (no double-deliver)"
    );
    assert_eq!(
        header_value(first, "X-NetRoll-Signature"),
        header_value(retry, "X-NetRoll-Signature"),
        "and the identical signature"
    );
}

#[tokio::test]
async fn a_permanently_failing_email_does_not_block_the_other_email_or_the_webhook() {
    // The EMAIL row of the destination-kind register
    // (`tests/delivery_destination_kinds.rs`). It predates Discord, and until
    // 2026-08-27 it armed only `emails` + `webhookUrl` and asserted
    // `egress.requests().len() == 1` — so it would have FAILED had Discord been
    // armed, and "a failing email does not suppress the ANNOUNCEMENT" was
    // unasserted while the register reported the row covered. Extended rather
    // than replaced, per the story's own precedent, and every assertion is now
    // per-target (`attempts_to`) so a third destination cannot make a count
    // assertion mean something else again.
    let app = test_app().await;
    let owner = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;
    let definition_id = create_net(&app, &owner).await;
    // One address always fails; the other, the webhook and Discord must all
    // still deliver.
    app.mailer
        .fail
        .lock()
        .expect("lock")
        .insert("broken@example.com".to_owned());
    set_delivery_config(
        &app,
        &owner,
        &definition_id,
        json!({
            "emails": ["broken@example.com", "good@example.com"],
            "webhookUrl": "https://hooks.example.com/net",
            "discordWebhookUrl": DISCORD_URL
        }),
    )
    .await;
    let session_id = start_session(&app, &owner, &definition_id).await;

    assert_eq!(
        close_session(&app, &owner, &session_id).await,
        StatusCode::OK
    );

    // The good address is delivered and both egress destinations are posted
    // despite the permanently-failing sibling address.
    eventually(|| {
        app.mailer.summary_recipients() == vec!["good@example.com"]
            && app.egress.attempts_to("hooks.example.com").len() == 1
            && app.egress.attempts_to("discord.com").len() == 1
    })
    .await;
    settle().await;

    assert_eq!(
        app.egress.attempts_to("hooks.example.com").len(),
        1,
        "the generic webhook delivered despite the failing address"
    );
    assert_eq!(
        app.egress.attempts_to("discord.com").len(),
        1,
        "and so did the Discord announcement"
    );
    // The broken address never appears among the successful sends.
    assert!(
        !app.mailer
            .summary_recipients()
            .contains(&"broken@example.com".to_owned())
    );
}

// --- Delivery off → nothing sent, secret never read -------------------

#[tokio::test]
async fn a_net_with_no_delivery_config_delivers_nothing() {
    let app = test_app().await;
    let owner = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;
    let definition_id = create_net(&app, &owner).await;
    // No delivery-config row at all.
    let session_id = start_session(&app, &owner, &definition_id).await;

    assert_eq!(
        close_session(&app, &owner, &session_id).await,
        StatusCode::OK
    );
    settle().await;

    assert_eq!(app.mailer.summary_count(), 0, "no summary email sent");
    assert!(app.egress.requests().is_empty(), "no webhook posted");
}

#[tokio::test]
async fn a_delivery_off_config_delivers_nothing_and_reads_no_secret() {
    let app = test_app().await;
    let owner = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;
    let definition_id = create_net(&app, &owner).await;
    // A config row that arms NO delivery (no emails, no webhook).
    set_delivery_config(
        &app,
        &owner,
        &definition_id,
        json!({ "emails": [], "webhookUrl": null }),
    )
    .await;
    assert_eq!(
        webhook_secret_in_db(&app, &definition_id).await,
        None,
        "delivery-off stored no secret"
    );
    let session_id = start_session(&app, &owner, &definition_id).await;

    assert_eq!(
        close_session(&app, &owner, &session_id).await,
        StatusCode::OK
    );
    settle().await;

    assert_eq!(app.mailer.summary_count(), 0);
    assert!(app.egress.requests().is_empty());
}

// --- Fire-and-forget — close returns before delivery completes --------

#[tokio::test]
async fn the_close_response_returns_while_a_blocked_delivery_is_still_pending() {
    let app = test_app().await;
    let owner = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;
    let definition_id = create_net(&app, &owner).await;
    set_delivery_config(
        &app,
        &owner,
        &definition_id,
        json!({ "emails": ["log@example.com"], "webhookUrl": null }),
    )
    .await;
    // The mailer will BLOCK on every summary send (never released here).
    let gate = Arc::new(tokio::sync::Notify::new());
    *app.mailer.gate.lock().expect("lock") = Some(gate);
    let session_id = start_session(&app, &owner, &definition_id).await;

    // The close handler returns its 200 even though delivery is spawned against
    // a mailer that never completes — proving delivery is not awaited.
    assert_eq!(
        close_session(&app, &owner, &session_id).await,
        StatusCode::OK
    );

    // Give the spawned task time to start and block; it must still be pending.
    settle().await;
    assert_eq!(
        app.mailer.summary_count(),
        0,
        "the close returned without waiting for the (blocked) delivery to finish"
    );
}

// --- The Discord announcement destination -----------------------
//
// Every assertion here locates the Discord request POSITIVELY, by URL, out of
// the captured vec. A delivery in this file can carry three targets, so
// `requests()[0]` names whichever leg won the race — and an absence assertion
// made against the wrong request, or against an empty capture, passes while
// proving nothing.

/// The configured Discord webhook URL used throughout. The bearer token is in
/// the PATH, which is why it is a credential and why no test here logs a
/// request.
const DISCORD_URL: &str = "https://discord.com/api/webhooks/1234567890/tok-abc";

/// The one embed of the announcement message, parsed out of a captured body.
fn sole_embed(req: &EgressRequest) -> Value {
    let body = req
        .body
        .as_deref()
        .expect("the Discord POST carries a body");
    let message: Value = serde_json::from_slice(body).expect("the body is JSON");
    let embeds = message["embeds"].as_array().expect("an `embeds` array");
    assert_eq!(embeds.len(), 1, "exactly one embed per message");
    assert!(
        message.get("content").is_none(),
        "the announcement is embed-only — no `content` field"
    );
    embeds[0].clone()
}

#[tokio::test]
async fn a_discord_only_net_delivers_a_formatted_embed_to_the_configured_url() {
    // Test A. A net with NO emails and NO generic webhook
    // still delivers: the deliverer's own delivery-off predicate must know
    // about the third target, and the POST must go through the injected
    // `Egress` port at the URL the owner configured.
    let app = test_app().await;
    let owner = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;
    let definition_id = create_net(&app, &owner).await;
    set_delivery_config(
        &app,
        &owner,
        &definition_id,
        json!({ "emails": [], "webhookUrl": null, "discordWebhookUrl": DISCORD_URL }),
    )
    .await;
    let session_id = start_session(&app, &owner, &definition_id).await;
    // NOT `N1CCK`: the embed's fixed footer is "Posted by NetRoll by N1CCK", so a
    // station with that callsign makes the absence assertion below fail for a
    // reason that has nothing to do with roster serialization. Caught 2026-08-27
    // by the first full-suite run that actually reached that assertion — every
    // earlier targeted run had failed on an earlier line, so it had never once
    // executed. A distinctive callsign, name and location make the assertion
    // about the ROSTER rather than about whatever tokens happen to be unique.
    let _ = check_in_with_grid(&app, &owner, &session_id, "W2XYZ", "fn31PR").await;

    assert_eq!(
        close_session(&app, &owner, &session_id).await,
        StatusCode::OK
    );
    eventually(|| !app.egress.attempts_to("discord.com").is_empty()).await;

    let attempts = app.egress.attempts_to("discord.com");
    assert_eq!(attempts.len(), 1, "one announcement, no retry on a 200");
    let req = &attempts[0].0;
    assert_eq!(req.url, DISCORD_URL, "the CONFIGURED URL, not a literal");
    assert_eq!(req.method, netroll_domain::egress::EgressMethod::Post);
    assert_eq!(header_value(req, "content-type"), Some("application/json"));

    let embed = sole_embed(req);
    assert_eq!(
        embed["title"], "Sunday Traffic Net",
        "the embed names the net"
    );
    assert!(
        embed["url"]
            .as_str()
            .expect("the embed links back to the session")
            .ends_with(&format!("/live/{session_id}")),
        "the title links to the public live view of THIS session"
    );
    // A count, never a station. The one roster fact carried is
    // the number of check-ins.
    let field_values: Vec<String> = embed["fields"]
        .as_array()
        .expect("fields")
        .iter()
        .map(|f| f["value"].as_str().unwrap_or_default().to_owned())
        .collect();
    assert!(
        field_values.iter().any(|v| v == "1"),
        "the check-in COUNT is carried: {field_values:?}"
    );
    let rendered = serde_json::to_string(&embed).expect("re-serialize");
    for station_value in ["W2XYZ", "Hartford", "FN31pr"] {
        assert!(
            !rendered.contains(station_value),
            "no per-station data is serialized into the embed, but \
             `{station_value}` appears: {rendered}"
        );
    }
}

#[tokio::test]
async fn a_permanently_failing_discord_destination_still_delivers_the_email_and_the_webhook() {
    // The `tokio::join!` already makes the legs structurally independent,
    // so the email/webhook half of this assertion passes even with no Discord
    // code at all. What makes it a real test is that it also asserts the
    // Discord leg WAS ATTEMPTED and refused — without that, the test is green
    // before the feature exists.
    let app = test_app_with_egress(FakeEgress::refusing("discord.com")).await;
    let owner = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;
    let definition_id = create_net(&app, &owner).await;
    set_delivery_config(
        &app,
        &owner,
        &definition_id,
        json!({
            "emails": ["log@example.com"],
            "webhookUrl": "https://hooks.example.com/net",
            "discordWebhookUrl": DISCORD_URL
        }),
    )
    .await;
    let session_id = start_session(&app, &owner, &definition_id).await;

    assert_eq!(
        close_session(&app, &owner, &session_id).await,
        StatusCode::OK
    );

    eventually(|| {
        app.mailer.summary_recipients() == vec!["log@example.com"]
            && !app.egress.attempts_to("hooks.example.com").is_empty()
            && !app.egress.attempts_to("discord.com").is_empty()
    })
    .await;
    settle().await;

    assert_eq!(
        app.egress.attempts_to("discord.com").len(),
        1,
        "a permanent refusal is never retried"
    );
    assert_eq!(
        app.egress.attempts_to("hooks.example.com").len(),
        1,
        "the generic webhook delivered despite the Discord destination failing"
    );
    assert_eq!(
        app.mailer.summary_count(),
        1,
        "the email delivered despite the Discord destination failing"
    );
}

#[tokio::test]
async fn the_discord_request_carries_no_hmac_signature_while_the_generic_webhook_does() {
    // Both destinations are armed in ONE delivery, so the absence on the
    // Discord request is asserted beside the PRESENCE on the generic webhook
    // request — a negative assertion with a positive control.
    let app = test_app().await;
    let owner = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;
    let definition_id = create_net(&app, &owner).await;
    set_delivery_config(
        &app,
        &owner,
        &definition_id,
        json!({
            "emails": [],
            "webhookUrl": "https://hooks.example.com/net",
            "discordWebhookUrl": DISCORD_URL
        }),
    )
    .await;
    let session_id = start_session(&app, &owner, &definition_id).await;

    assert_eq!(
        close_session(&app, &owner, &session_id).await,
        StatusCode::OK
    );
    eventually(|| {
        !app.egress.attempts_to("discord.com").is_empty()
            && !app.egress.attempts_to("hooks.example.com").is_empty()
    })
    .await;

    let generic = app.egress.attempts_to("hooks.example.com");
    let discord = app.egress.attempts_to("discord.com");
    assert_eq!(generic.len(), 1);
    assert_eq!(discord.len(), 1);
    // The positive control: the signature contract still rides the generic
    // webhook in the SAME delivery.
    assert!(
        header_value(&generic[0].0, "X-NetRoll-Signature").is_some(),
        "the generic webhook still carries its HMAC signature"
    );
    assert!(
        header_value(&generic[0].0, "X-NetRoll-Delivery-Id").is_some(),
        "and its idempotency key"
    );
    // The claim under test.
    assert!(
        header_value(&discord[0].0, "X-NetRoll-Signature").is_none(),
        "Discord verifies nothing; the signature contract does not cover it"
    );
    assert!(
        header_value(&discord[0].0, "X-NetRoll-Delivery-Id").is_none(),
        "and neither does the delivery-id header"
    );
}

#[tokio::test]
async fn a_discord_429_naming_a_small_retry_after_waits_that_long_before_retrying() {
    // The observable is the GAP between the two attempts: `RETRY_BACKOFF`
    // is 25ms, the scripted `retry_after` is 200ms, so ignoring the parsed
    // value collapses the gap by an order of magnitude.
    let app = test_app_with_egress(FakeEgress::scripting(&[(
        "discord.com",
        429,
        r#"{"message":"You are being rate limited.","retry_after":0.2,"global":false}"#,
    )]))
    .await;
    let owner = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;
    let definition_id = create_net(&app, &owner).await;
    set_delivery_config(
        &app,
        &owner,
        &definition_id,
        json!({ "emails": [], "webhookUrl": null, "discordWebhookUrl": DISCORD_URL }),
    )
    .await;
    let session_id = start_session(&app, &owner, &definition_id).await;

    assert_eq!(
        close_session(&app, &owner, &session_id).await,
        StatusCode::OK
    );
    eventually(|| app.egress.attempts_to("discord.com").len() == 2).await;

    let attempts = app.egress.attempts_to("discord.com");
    let gap = attempts[1].1.duration_since(attempts[0].1);
    assert!(
        gap >= Duration::from_millis(150),
        "the honoured wait came from the body's `retry_after` (0.2s), not from \
         RETRY_BACKOFF (25ms); observed gap {gap:?}"
    );
}

#[tokio::test]
async fn a_discord_429_naming_an_over_cap_retry_after_abandons_instead_of_sleeping() {
    // 999 seconds is far past the honoured cap: the delivery must give up
    // now rather than hold its concurrency permit. Bounded by `settle`, so a
    // regression that actually slept would still fail here rather than hang.
    let app = test_app_with_egress(FakeEgress::scripting(&[(
        "discord.com",
        429,
        r#"{"retry_after":999.0,"global":true}"#,
    )]))
    .await;
    let owner = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;
    let definition_id = create_net(&app, &owner).await;
    set_delivery_config(
        &app,
        &owner,
        &definition_id,
        json!({ "emails": [], "webhookUrl": null, "discordWebhookUrl": DISCORD_URL }),
    )
    .await;
    let session_id = start_session(&app, &owner, &definition_id).await;

    assert_eq!(
        close_session(&app, &owner, &session_id).await,
        StatusCode::OK
    );
    eventually(|| app.egress.attempts_to("discord.com").len() == 1).await;
    settle().await;

    assert_eq!(
        app.egress.attempts_to("discord.com").len(),
        1,
        "an over-cap `retry_after` abandons — it never becomes a long sleep \
         followed by a second attempt"
    );
}

#[tokio::test]
async fn a_hostile_discord_429_body_leaves_the_email_and_the_generic_webhook_delivering() {
    // The blast-radius regression. `{"retry_after":1e30}` is
    // finite and positive, so it passes the `is_finite`/`<= 0` guards, and
    // `Duration::from_secs_f64` PANICKED on it (`Duration`'s ceiling is
    // ~1.8e19 s) instead of erroring. The panic was inside a `tokio::join!`
    // branch, so it unwound the whole spawned delivery task: the generic
    // webhook leg never ran at all and even the abort log never fired. That is
    // exactly the suppression this must never have, and it made the promise in
    // `docs/nets/configure-delivery.md` — "a rate-limited Discord channel
    // doesn't stop the webhook" — false. The body is
    // attacker-influenceable: the configured URL is whatever the owner pasted,
    // so nothing guarantees a 429 came from Discord.
    let app = test_app_with_egress(FakeEgress::scripting(&[(
        "discord.com",
        429,
        r#"{"retry_after":1e30,"global":true}"#,
    )]))
    .await;
    let owner = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;
    let definition_id = create_net(&app, &owner).await;
    set_delivery_config(
        &app,
        &owner,
        &definition_id,
        json!({
            "emails": ["log@example.com"],
            "webhookUrl": "https://hooks.example.com/net",
            "discordWebhookUrl": DISCORD_URL
        }),
    )
    .await;
    let session_id = start_session(&app, &owner, &definition_id).await;

    assert_eq!(
        close_session(&app, &owner, &session_id).await,
        StatusCode::OK
    );

    eventually(|| {
        app.mailer.summary_recipients() == vec!["log@example.com"]
            && app.egress.attempts_to("hooks.example.com").len() == 1
            && app.egress.attempts_to("discord.com").len() == 1
    })
    .await;
    settle().await;

    assert_eq!(
        app.egress.attempts_to("hooks.example.com").len(),
        1,
        "a hostile Discord 429 body must not suppress the generic webhook leg"
    );
    assert_eq!(app.mailer.summary_count(), 1, "nor the email fan-out");
    assert_eq!(
        app.egress.attempts_to("discord.com").len(),
        1,
        "and the Discord leg itself abandons rather than panicking or sleeping"
    );
}

#[tokio::test]
async fn a_discord_429_with_an_unparseable_body_falls_back_to_the_default_backoff_and_retries() {
    // A 429 from a proxy or CDN in front of Discord is still transient. A
    // body that is not JSON must NOT be read as "abandon".
    let app = test_app_with_egress(FakeEgress::scripting(&[(
        "discord.com",
        429,
        "<html>429 Too Many Requests</html>",
    )]))
    .await;
    let owner = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;
    let definition_id = create_net(&app, &owner).await;
    set_delivery_config(
        &app,
        &owner,
        &definition_id,
        json!({ "emails": [], "webhookUrl": null, "discordWebhookUrl": DISCORD_URL }),
    )
    .await;
    let session_id = start_session(&app, &owner, &definition_id).await;

    assert_eq!(
        close_session(&app, &owner, &session_id).await,
        StatusCode::OK
    );
    eventually(|| app.egress.attempts_to("discord.com").len() == 2).await;

    let attempts = app.egress.attempts_to("discord.com");
    let gap = attempts[1].1.duration_since(attempts[0].1);
    assert!(
        gap < Duration::from_millis(150),
        "an unparseable body falls back to RETRY_BACKOFF (25ms), not to the cap; \
         observed gap {gap:?}"
    );
    // Two-sided on purpose. `gap < 150ms` alone also passes under
    // `RetryAfter::Wait(Duration::ZERO)` — the ONE outcome
    // `discord_retry_after`'s docstring forbids ("never sleep zero, because a
    // hot retry loop against a rate limiter is worse than not retrying"). The
    // lower bound is 20ms against a 25ms `RETRY_BACKOFF` so it cannot flake on
    // timer granularity while still excluding zero.
    assert!(
        gap >= Duration::from_millis(20),
        "the fallback still SLEEPS — a 429 must never become a hot retry loop; \
         observed gap {gap:?}"
    );
}

#[tokio::test]
async fn clearing_a_discord_destination_returns_the_net_to_delivery_off() {
    // The second half. A config row whose three targets are all empty is
    // still "delivery off" — the predicate must not become "a row exists".
    let app = test_app().await;
    let owner = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;
    let definition_id = create_net(&app, &owner).await;
    set_delivery_config(
        &app,
        &owner,
        &definition_id,
        json!({ "emails": [], "webhookUrl": null, "discordWebhookUrl": DISCORD_URL }),
    )
    .await;
    set_delivery_config(
        &app,
        &owner,
        &definition_id,
        json!({ "emails": [], "webhookUrl": null, "discordWebhookUrl": null }),
    )
    .await;
    let session_id = start_session(&app, &owner, &definition_id).await;

    assert_eq!(
        close_session(&app, &owner, &session_id).await,
        StatusCode::OK
    );
    settle().await;

    assert_eq!(app.mailer.summary_count(), 0);
    assert!(
        app.egress.requests().is_empty(),
        "a cleared Discord destination delivers nothing"
    );
}

#[tokio::test]
async fn a_permanently_failing_webhook_does_not_suppress_the_email_or_the_announcement() {
    // The generic webhook's own row in the destination-kind register. The
    // pre-existing isolation test fails an EMAIL; nothing failed the webhook and
    // asserted the other two still landed.
    let app = test_app_with_egress(FakeEgress::refusing("hooks.example.com")).await;
    let owner = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;
    let definition_id = create_net(&app, &owner).await;
    set_delivery_config(
        &app,
        &owner,
        &definition_id,
        json!({
            "emails": ["log@example.com"],
            "webhookUrl": "https://hooks.example.com/net",
            "discordWebhookUrl": DISCORD_URL
        }),
    )
    .await;
    let session_id = start_session(&app, &owner, &definition_id).await;

    assert_eq!(
        close_session(&app, &owner, &session_id).await,
        StatusCode::OK
    );

    // Gate on the legs' TERMINAL STATES, not on two of the
    // three side effects. The previous gate waited on the mailer and the
    // Discord attempt and then read the webhook's attempt count synchronously
    // — a third, concurrent leg whose `webhook_target` read might not have
    // returned yet, so a zero here meant the read won a race, not that the
    // webhook was never attempted (the recorded harness noise).
    eventually_settled(&app, &session_id).await;
    assert_eq!(app.mailer.summary_recipients(), vec!["log@example.com"]);
    assert_eq!(
        app.egress.attempts_to("hooks.example.com").len(),
        1,
        "the refused webhook was attempted once and never retried"
    );
    assert_eq!(
        app.egress.attempts_to("discord.com").len(),
        1,
        "the announcement delivered despite the generic webhook failing"
    );
}
