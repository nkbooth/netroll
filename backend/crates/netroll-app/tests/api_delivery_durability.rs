// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
//! Durability tests for on-close delivery: a delivery owed survives the process
//! that owed it. Real Postgres, and gateable fake `Mailer`/`Egress` so a send
//! can be held genuinely in flight and its task aborted.
//!
//! Time is a test INPUT throughout: no test sleeps for a lease or a backoff.

use std::sync::atomic::{AtomicU16, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use netroll_adapters::pg::delivery_jobs::DeliveryJobRepo;
use netroll_adapters::pg::net_sessions::CloseOutcome;
use netroll_app::delivery::MAX_CONCURRENT_DELIVERIES;
use netroll_app::delivery_sweeper::{
    DELIVERY_JOB_RETENTION_MILLIS, DELIVERY_LEASE_MILLIS, MAX_JOB_ATTEMPTS, RecoveredCounts,
    RecoveryScope, SweptCounts, recover_interrupted_deliveries, recover_with_scope,
    retry_delay_millis, run_delivery_sweep_tick,
};
use netroll_app::http::{AppState, api_router};
use netroll_domain::consent::CURRENT_TERMS_VERSION;
use netroll_domain::egress::{Egress, EgressError, EgressRequest, EgressResponse};
use netroll_domain::ports::{BoxFuture, MailError, Mailer, NetSummaryMail};
use serde_json::{Value, json};
use sqlx::PgPool;
use sqlx::postgres::PgPoolOptions;
use testcontainers_modules::postgres::Postgres;
use testcontainers_modules::testcontainers::ContainerAsync;
use testcontainers_modules::testcontainers::runners::AsyncRunner;
use tower::ServiceExt;
use uuid::Uuid;

// --- Capturing, gateable fake Mailer -----------------------------------------

#[derive(Default)]
struct CapturingMailer {
    magic: Mutex<Vec<String>>,
    /// Recipients of every summary the mailer actually accepted.
    summaries: Mutex<Vec<String>>,
    /// When set, every summary send parks on this gate AFTER signalling
    /// `entered` — so a test knows an attempt is genuinely in flight before it
    /// aborts the task holding it.
    gate: Mutex<Option<Arc<tokio::sync::Notify>>>,
    entered: AtomicUsize,
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
    fn recipients(&self) -> Vec<String> {
        self.summaries.lock().expect("lock").clone()
    }
    fn hold(&self) {
        *self.gate.lock().expect("lock") = Some(Arc::new(tokio::sync::Notify::new()));
    }
    fn release(&self) {
        *self.gate.lock().expect("lock") = None;
    }
    fn entered(&self) -> usize {
        self.entered.load(Ordering::SeqCst)
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
        _summary: &'a NetSummaryMail,
    ) -> BoxFuture<'a, Result<(), MailError>> {
        Box::pin(async move {
            let gate = self.gate.lock().expect("lock").clone();
            if let Some(gate) = gate {
                self.entered.fetch_add(1, Ordering::SeqCst);
                gate.notified().await;
            }
            self.summaries.lock().expect("lock").push(to.to_owned());
            Ok(())
        })
    }
}

// --- Capturing, gateable fake Egress ------------------------------------------

struct FakeEgress {
    /// Every request, recorded BEFORE any gate or failure — so a request that
    /// was in flight when its task died is still counted as attempted, which is
    /// exactly the ambiguity recovery has to respect.
    requests: Mutex<Vec<EgressRequest>>,
    /// The status every reply carries once `failures_remaining` is spent.
    status: AtomicU16,
    /// Replies carry a 500 while this is above zero (the transient probe).
    failures_remaining: AtomicUsize,
    /// URL substrings whose every send is a PERMANENT egress refusal.
    refuse: Mutex<Vec<String>>,
    /// When set, every send parks here after recording itself.
    gate: Mutex<Option<Arc<tokio::sync::Notify>>>,
    /// The body every reply carries. Empty unless a test needs one — a Discord
    /// 429 puts its `retry_after` here, which is the only place the wait exists.
    body: Mutex<Vec<u8>>,
}

impl FakeEgress {
    fn ok() -> Arc<Self> {
        Arc::new(Self {
            requests: Mutex::new(Vec::new()),
            status: AtomicU16::new(200),
            failures_remaining: AtomicUsize::new(0),
            refuse: Mutex::new(Vec::new()),
            gate: Mutex::new(None),
            body: Mutex::new(Vec::new()),
        })
    }
    fn always(status: u16) -> Arc<Self> {
        let fake = Self::ok();
        fake.status.store(status, Ordering::SeqCst);
        fake
    }
    fn failing_first(n: usize) -> Arc<Self> {
        let fake = Self::ok();
        fake.failures_remaining.store(n, Ordering::SeqCst);
        fake
    }
    /// Answers every send with a Discord-shaped 429 naming `seconds` of wait.
    fn rate_limited(seconds: f64) -> Arc<Self> {
        let fake = Self::always(429);
        *fake.body.lock().expect("lock") = format!("{{\"retry_after\":{seconds}}}").into_bytes();
        fake
    }
    fn refusing(needle: &str) -> Arc<Self> {
        let fake = Self::ok();
        fake.refuse.lock().expect("lock").push(needle.to_owned());
        fake
    }
    fn hold(&self) {
        *self.gate.lock().expect("lock") = Some(Arc::new(tokio::sync::Notify::new()));
    }
    fn release(&self) {
        *self.gate.lock().expect("lock") = None;
    }
    fn requests(&self) -> Vec<EgressRequest> {
        self.requests.lock().expect("lock").clone()
    }
    fn attempts_to(&self, needle: &str) -> Vec<EgressRequest> {
        self.requests()
            .into_iter()
            .filter(|req| req.url.contains(needle))
            .collect()
    }
}

impl Egress for FakeEgress {
    fn send<'a>(
        &'a self,
        req: EgressRequest,
    ) -> BoxFuture<'a, Result<EgressResponse, EgressError>> {
        Box::pin(async move {
            let url = req.url.clone();
            self.requests.lock().expect("lock").push(req);
            let gate = self.gate.lock().expect("lock").clone();
            if let Some(gate) = gate {
                gate.notified().await;
            }
            if self
                .refuse
                .lock()
                .expect("lock")
                .iter()
                .any(|needle| url.contains(needle.as_str()))
            {
                return Err(EgressError::BlockedAddress);
            }
            let failing = self
                .failures_remaining
                .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |n| n.checked_sub(1))
                .is_ok();
            let status = if failing {
                500
            } else {
                self.status.load(Ordering::SeqCst)
            };
            Ok(EgressResponse {
                status,
                body: self.body.lock().expect("lock").clone(),
            })
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

const DISCORD_URL: &str = "https://discord.com/api/webhooks/1234567890/tok-abc";
const WEBHOOK_URL: &str = "https://hooks.example.com/net";

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
    fn jobs(&self) -> &DeliveryJobRepo {
        &self.state.delivery_jobs
    }
    /// One sweep tick at `now` against the app's real (fake-backed) deliverer.
    async fn tick(&self, now_millis: u64) -> SweptCounts {
        run_delivery_sweep_tick(self.jobs(), &self.state.delivery_service(), now_millis)
            .await
            .expect("sweep tick")
    }
    async fn recover(&self, now_millis: u64) -> RecoveredCounts {
        recover_interrupted_deliveries(self.jobs(), now_millis)
            .await
            .expect("recovery pass")
    }
    /// The boot pass: every claim is a dead process's claim.
    async fn recover_at_boot(&self, now_millis: u64) -> RecoveredCounts {
        recover_with_scope(self.jobs(), now_millis, RecoveryScope::EveryClaim)
            .await
            .expect("boot recovery pass")
    }
    /// Closes through the REPO, so nothing spawns a delivery task: the legs are
    /// planned and nothing has claimed them. Returns the close instant.
    async fn close_without_spawning(&self, session_id: Uuid) -> u64 {
        let now = real_now_millis();
        let outcome = self
            .state
            .net_sessions
            .close(session_id, now, None)
            .await
            .expect("close");
        assert!(matches!(outcome, CloseOutcome::Closed(_)));
        now
    }
}

async fn test_app_with(egress: Arc<FakeEgress>) -> TestApp {
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

async fn test_app() -> TestApp {
    test_app_with(FakeEgress::ok()).await
}

fn real_now_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock after epoch")
        .as_millis() as u64
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

async fn start_session(app: &TestApp, cookie: &str, definition_id: &str) -> Uuid {
    let (status, body) = send_json(
        app.router(),
        "POST",
        "/api/net-sessions",
        Some(json!({ "definitionId": definition_id })),
        Some(cookie),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    Uuid::parse_str(body["id"].as_str().expect("session id")).expect("uuid")
}

/// An owner with one net whose delivery config is `config`, and one live
/// session on it. The common arrange step.
async fn owner_net_and_session(app: &TestApp, config: Value) -> (String, String, Uuid) {
    let owner = sign_in_consent_callsign(app, "owner@example.com", "w1aw").await;
    let definition_id = create_net(app, &owner).await;
    set_delivery_config(app, &owner, &definition_id, config).await;
    let session_id = start_session(app, &owner, &definition_id).await;
    (owner, definition_id, session_id)
}

/// One leg row, read straight off the table.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Leg {
    destination: String,
    target: String,
    state: String,
    attempts: i32,
    claimed: bool,
    next_attempt_at_millis: i64,
}

async fn legs_of(pool: &PgPool, session_id: Uuid) -> Vec<Leg> {
    sqlx::query_as::<_, (String, String, String, i32, bool, i64)>(
        "SELECT destination, target, state, attempts, claimed_until IS NOT NULL,
                (extract(epoch FROM next_attempt_at) * 1000)::bigint
           FROM net_delivery_jobs WHERE session_id = $1
          ORDER BY destination, target",
    )
    .bind(session_id)
    .fetch_all(pool)
    .await
    .expect("read legs")
    .into_iter()
    .map(
        |(destination, target, state, attempts, claimed, next)| Leg {
            destination,
            target,
            state,
            attempts,
            claimed,
            next_attempt_at_millis: next,
        },
    )
    .collect()
}

fn states(legs: &[Leg]) -> Vec<&str> {
    legs.iter().map(|l| l.state.as_str()).collect()
}

/// Polls `check` until it holds or a bounded wait elapses — for the two
/// in-flight signals only (a gated send has been ENTERED). Nothing else in this
/// file waits on wall time.
async fn eventually(check: impl Fn() -> bool) {
    for _ in 0..300 {
        if check() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("in-flight signal not observed within the bounded wait");
}

// --- A delivery owed survives the process that owed it -----------------

#[tokio::test]
async fn a_close_whose_delivery_task_never_runs_still_leaves_pending_legs_a_tick_completes() {
    // The debt is written by the close itself. No task ever ran for this
    // session; one sweep tick pays it.
    let app = test_app().await;
    let (_owner, _definition_id, session_id) = owner_net_and_session(
        &app,
        json!({ "emails": ["log@example.com"], "webhookUrl": null }),
    )
    .await;

    let closed_at = app.close_without_spawning(session_id).await;
    let before = legs_of(&app.pool, session_id).await;
    assert_eq!(before.len(), 1);
    assert_eq!(before[0].state, "pending");
    assert_eq!(before[0].attempts, 0);
    assert!(!before[0].claimed);
    assert!(app.mailer.recipients().is_empty(), "nothing has run yet");

    let counts = app.tick(closed_at).await;
    assert_eq!(counts.claimed, 1);
    assert_eq!(counts.sessions, 1);
    assert_eq!(counts.settled.succeeded, 1);
    assert_eq!(app.mailer.recipients(), vec!["log@example.com"]);
    let after = legs_of(&app.pool, session_id).await;
    assert_eq!(states(&after), vec!["succeeded"]);
    assert_eq!(after[0].attempts, 1, "the claim burned one attempt");
}

#[tokio::test]
async fn an_email_delivery_aborted_in_flight_is_recovered_and_completed_by_a_later_sweep() {
    // The RED shape: a REAL task, genuinely mid-send, dropped.
    // The mailer parks every send on a gate and signals that it did; the leg is
    // then `pending` with a live claim. Abort the task. Advance past the lease,
    // recover, tick once: the email arrives and the row is `succeeded`.
    let app = test_app().await;
    let (_owner, _definition_id, session_id) = owner_net_and_session(
        &app,
        json!({ "emails": ["log@example.com"], "webhookUrl": null }),
    )
    .await;
    app.close_without_spawning(session_id).await;
    app.mailer.hold();

    let handle = app
        .state
        .delivery_service()
        .spawn_for_closed_session(session_id);
    eventually(|| app.mailer.entered() >= 1).await;
    let in_flight = legs_of(&app.pool, session_id).await;
    assert_eq!(in_flight[0].state, "pending");
    assert!(in_flight[0].claimed, "an attempt is in flight: leased");
    assert_eq!(in_flight[0].attempts, 1);

    handle.abort();
    let joined = handle.await;
    assert!(
        joined.is_err_and(|e| e.is_cancelled()),
        "the task was dropped mid-send"
    );
    assert!(
        app.mailer.recipients().is_empty(),
        "the aborted send never completed"
    );
    app.mailer.release();

    // Before the lease passes, recovery sees nothing and a tick claims nothing:
    // the row still looks like a live attempt, which is the point of a lease.
    let too_soon = real_now_millis();
    assert_eq!(app.recover(too_soon).await, RecoveredCounts::default());
    assert_eq!(app.tick(too_soon).await.claimed, 0);

    let later = real_now_millis() + DELIVERY_LEASE_MILLIS + 60_000;
    let recovered = app.recover(later).await;
    assert_eq!(recovered.released, 1, "an interrupted email leg is retried");
    assert_eq!(recovered.abandoned, 0);
    let released = legs_of(&app.pool, session_id).await;
    assert_eq!(released[0].state, "pending");
    assert!(!released[0].claimed);

    let counts = app.tick(later).await;
    assert_eq!(counts.claimed, 1);
    assert_eq!(counts.settled.succeeded, 1);
    assert_eq!(app.mailer.recipients(), vec!["log@example.com"]);
    let done = legs_of(&app.pool, session_id).await;
    assert_eq!(states(&done), vec!["succeeded"]);
    assert_eq!(done[0].attempts, 2, "the killed attempt still counted");
}

#[tokio::test]
async fn every_email_leg_of_a_session_is_in_flight_at_once_so_the_fan_out_fits_the_lease() {
    // The claim leases EVERY leg of the session for `DELIVERY_LEASE_MILLIS`,
    // so the lease has to cover the whole fan-out, not one address. Sequential
    // sends cost `MAX_DELIVERY_EMAILS` x the mailer's own bound (10 x ~30 s),
    // which overruns the 180 s lease at roughly six addresses and lets recovery
    // re-claim a LIVE attempt. Concurrency is what keeps the derivation honest:
    // the fan-out costs one attempt, whatever the address count.
    //
    // Asserted by parking every send on the gate and counting how many entered.
    // Sequential code parks the first and never reaches the rest.
    let app = test_app().await;
    let (_owner, _definition_id, session_id) = owner_net_and_session(
        &app,
        json!({
            "emails": ["a@example.com", "b@example.com", "c@example.com"],
            "webhookUrl": null
        }),
    )
    .await;
    app.close_without_spawning(session_id).await;
    app.mailer.hold();

    let handle = app
        .state
        .delivery_service()
        .spawn_for_closed_session(session_id);
    eventually(|| app.mailer.entered() >= 1).await;
    // Bounded settle: a concurrent fan-out enters the rest immediately, a
    // sequential one never does because the first send owns the gate.
    for _ in 0..100 {
        if app.mailer.entered() >= 3 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(
        app.mailer.entered(),
        3,
        "every email leg is in flight at once, so the fan-out costs one attempt and not one per \
         address"
    );

    handle.abort();
    let _ = handle.await;
}

#[tokio::test]
async fn a_leg_whose_in_run_ladder_is_spent_is_rescheduled_not_abandoned_and_a_later_tick_delivers()
{
    // The COMMON case, not the restart one: a receiver down for longer than
    // the ~50 ms the inner ladder covers. Three 500s spend the inner ladder;
    // the row goes back to `pending` due 30 s later; the tick at that instant
    // delivers.
    let app = test_app_with(FakeEgress::failing_first(3)).await;
    let (_owner, _definition_id, session_id) =
        owner_net_and_session(&app, json!({ "emails": [], "webhookUrl": WEBHOOK_URL })).await;
    let closed_at = app.close_without_spawning(session_id).await;

    let first = app.tick(closed_at).await;
    assert_eq!(first.claimed, 1);
    assert_eq!(first.settled.rescheduled, 1);
    assert_eq!(first.settled.failed, 0);
    assert_eq!(
        app.egress.requests().len(),
        3,
        "the inner ladder ran to its bound"
    );
    let parked = legs_of(&app.pool, session_id).await;
    assert_eq!(parked[0].state, "pending");
    assert_eq!(parked[0].attempts, 1);
    assert!(!parked[0].claimed, "the lease was released");
    assert_eq!(
        parked[0].next_attempt_at_millis,
        (closed_at + retry_delay_millis(1)) as i64,
        "due one outer-ladder step later"
    );

    // Not yet due: nothing claimed, nothing sent.
    let early = app.tick(closed_at + retry_delay_millis(1) - 1).await;
    assert_eq!(early.claimed, 0);
    assert_eq!(app.egress.requests().len(), 3);

    let due = app.tick(closed_at + retry_delay_millis(1)).await;
    assert_eq!(due.claimed, 1);
    assert_eq!(due.settled.succeeded, 1);
    assert_eq!(app.egress.requests().len(), 4);
    let done = legs_of(&app.pool, session_id).await;
    assert_eq!(states(&done), vec!["succeeded"]);
    assert_eq!(done[0].attempts, 2);
    // For the webhook: the retry hours later is the same bytes and the
    // same idempotency key as the first attempt.
    let requests = app.egress.requests();
    assert_eq!(requests[0].body, requests[3].body);
    assert_eq!(
        header_value(&requests[0], "X-NetRoll-Delivery-Id"),
        header_value(&requests[3], "X-NetRoll-Delivery-Id")
    );
}

// --- Recovery is per destination, derived from the dedupe basis -------

#[tokio::test]
async fn recovery_retries_an_interrupted_webhook_and_never_re_posts_an_interrupted_discord() {
    // ONE interrupted close, BOTH halves asserted. The egress parks every send
    // after recording it, so both POSTs are genuinely in flight when the task
    // is aborted. After recovery the webhook is POSTed again with the identical
    // delivery id and body; Discord's captured count does not increase.
    let app = test_app().await;
    let (_owner, _definition_id, session_id) = owner_net_and_session(
        &app,
        json!({ "emails": [], "webhookUrl": WEBHOOK_URL, "discordWebhookUrl": DISCORD_URL }),
    )
    .await;
    app.close_without_spawning(session_id).await;
    app.egress.hold();

    let handle = app
        .state
        .delivery_service()
        .spawn_for_closed_session(session_id);
    eventually(|| app.egress.requests().len() == 2).await;
    let in_flight = legs_of(&app.pool, session_id).await;
    assert!(
        in_flight
            .iter()
            .all(|l| l.state == "pending" && l.claimed && l.attempts == 1)
    );
    handle.abort();
    assert!(handle.await.is_err_and(|e| e.is_cancelled()));
    app.egress.release();

    let later = real_now_millis() + DELIVERY_LEASE_MILLIS + 60_000;
    let recovered = app.recover(later).await;
    assert_eq!(
        recovered,
        RecoveredCounts {
            released: 1,
            abandoned: 1
        },
        "the webhook leg is released; the Discord leg is abandoned"
    );
    let after_recovery = legs_of(&app.pool, session_id).await;
    let discord = after_recovery
        .iter()
        .find(|l| l.destination == "discord")
        .expect("discord leg");
    let webhook = after_recovery
        .iter()
        .find(|l| l.destination == "webhook")
        .expect("webhook leg");
    assert_eq!(discord.state, "failed", "terminal, never re-posted");
    assert_eq!(webhook.state, "pending");
    assert!(!webhook.claimed);

    let counts = app.tick(later).await;
    assert_eq!(counts.claimed, 1, "only the webhook leg is claimable");
    assert_eq!(counts.settled.succeeded, 1);

    let webhook_attempts = app.egress.attempts_to("hooks.example.com");
    assert_eq!(
        webhook_attempts.len(),
        2,
        "the webhook was POSTed a second time"
    );
    assert_eq!(webhook_attempts[0].body, webhook_attempts[1].body);
    assert_eq!(
        header_value(&webhook_attempts[0], "X-NetRoll-Delivery-Id"),
        header_value(&webhook_attempts[1], "X-NetRoll-Delivery-Id"),
        "the SAME idempotency key across the restart"
    );
    assert_eq!(
        app.egress.attempts_to("discord.com").len(),
        1,
        "Discord's captured request count did not increase"
    );
    let done = legs_of(&app.pool, session_id).await;
    assert_eq!(states(&done), vec!["failed", "succeeded"]);
}

#[tokio::test]
async fn a_discord_429_above_the_honoured_cap_is_rescheduled_for_the_wait_discord_asked_for() {
    // The cap exists to stop the attempt SLEEPING on a permit, and that is
    // unchanged. But the wait itself has to survive into the row: a
    // `retry_after` above `MAX_HONOURED_RETRY_AFTER` is Discord signalling a
    // GLOBAL rate limit, and rescheduling at the ladder's 30 s re-POSTs several
    // times inside the window Discord just closed — against the same
    // 10,000-invalid-requests ban budget that makes a 401 permanent here.
    let app = test_app_with(FakeEgress::rate_limited(600.0)).await;
    let (_owner, _definition_id, session_id) = owner_net_and_session(
        &app,
        json!({ "emails": [], "webhookUrl": null, "discordWebhookUrl": DISCORD_URL }),
    )
    .await;
    let closed_at = app.close_without_spawning(session_id).await;

    let counts = app.tick(closed_at).await;
    assert_eq!(counts.settled.rescheduled, 1, "still owed, not abandoned");
    assert_eq!(
        app.egress.attempts_to("discord.com").len(),
        1,
        "the attempt is still abandoned without sleeping on the permit"
    );

    let legs = legs_of(&app.pool, session_id).await;
    assert_eq!(
        legs[0].next_attempt_at_millis,
        (closed_at + 600_000) as i64,
        "the row honours Discord's wait, not the ladder's 30 s"
    );
}

#[tokio::test]
async fn boot_recovery_releases_a_claim_whose_lease_has_not_expired_yet() {
    // The boot pass exists for a process killed MID-SEND, which is exactly the
    // case where the lease is still in the future: it was stamped seconds
    // before the crash and runs for `DELIVERY_LEASE_MILLIS`. Selecting only
    // lapsed leases makes the awaited boot pass return nothing for that crash
    // and leaves the delivery waiting out a lease no process is honouring.
    let app = test_app().await;
    let (_owner, _definition_id, session_id) = owner_net_and_session(
        &app,
        json!({ "emails": ["log@example.com"], "webhookUrl": null }),
    )
    .await;
    let closed_at = app.close_without_spawning(session_id).await;

    let claimed = app
        .jobs()
        .claim_session(session_id, closed_at, DELIVERY_LEASE_MILLIS)
        .await
        .expect("claim");
    assert_eq!(claimed.len(), 1);

    // One second later — the restart. The lease has almost all of its life
    // left, and a per-tick pass correctly refuses to touch it.
    let boot = closed_at + 1_000;
    assert_eq!(
        app.recover(boot).await,
        RecoveredCounts::default(),
        "a running tick must not touch a lease that has not lapsed"
    );
    assert_eq!(
        app.recover_at_boot(boot).await,
        RecoveredCounts {
            released: 1,
            abandoned: 0
        },
        "at boot the claim belongs to the process that died, lease or no lease"
    );

    let counts = app.tick(boot).await;
    assert_eq!(counts.settled.succeeded, 1);
    assert_eq!(app.mailer.recipients(), vec!["log@example.com"]);
}

#[tokio::test]
async fn a_leg_interrupted_past_its_attempt_budget_becomes_terminal_instead_of_looping_forever() {
    // The budget check lives in `settle_leg`, which only runs when an attempt
    // COMPLETES. Recovery released an interrupted leg unconditionally and
    // the claim has no `attempts` predicate, so a leg that is interrupted
    // every time — a crash loop, or an attempt that outlives its lease — climbs
    // `attempts` forever and re-sends on every cycle. An exhausted
    // `MAX_JOB_ATTEMPTS` as terminal; this proves it is terminal on the
    // interrupted path too.
    let app = test_app().await;
    let (_owner, _definition_id, session_id) = owner_net_and_session(
        &app,
        json!({ "emails": ["log@example.com"], "webhookUrl": null }),
    )
    .await;
    let closed_at = app.close_without_spawning(session_id).await;

    // Claim, never settle, let the lease lapse, recover. Repeat past the budget.
    let mut now = closed_at;
    for _ in 0..(MAX_JOB_ATTEMPTS + 2) {
        let claimed = app
            .jobs()
            .claim_session(session_id, now, DELIVERY_LEASE_MILLIS)
            .await
            .expect("claim");
        if claimed.is_empty() {
            break;
        }
        now += DELIVERY_LEASE_MILLIS + 1;
        app.recover(now).await;
    }

    let legs = legs_of(&app.pool, session_id).await;
    assert_eq!(
        states(&legs),
        vec!["failed"],
        "an endlessly interrupted leg is recorded, not retried forever"
    );
    assert!(
        legs[0].attempts <= MAX_JOB_ATTEMPTS,
        "the budget bounds the claims: attempts was {}",
        legs[0].attempts
    );
    assert_eq!(
        app.jobs()
            .claim_session(session_id, now + 1, DELIVERY_LEASE_MILLIS)
            .await
            .expect("claim")
            .len(),
        0,
        "nothing claims a terminal leg"
    );
}

#[tokio::test]
async fn a_discord_leg_claimed_but_never_posted_is_retried_not_abandoned() {
    // The AMBIGUITY forbids the retry, not the claim. `attempts` is burned at
    // claim, so a claimed row proves only that a tick picked the leg up — not
    // that a POST was ever dispatched. Everything between the claim and
    // `egress.send` can still fail without Discord seeing anything: a storage
    // error in `load_for_delivery`, a `delivery_configs` read, or the process
    // dying while queued on the concurrency semaphore. Abandoning those is a
    // guaranteed loss bought for no duplicate risk at all.
    let app = test_app().await;
    let (_owner, _definition_id, session_id) = owner_net_and_session(
        &app,
        json!({ "emails": [], "webhookUrl": null, "discordWebhookUrl": DISCORD_URL }),
    )
    .await;
    let closed_at = app.close_without_spawning(session_id).await;

    // Claim it and then do nothing: precisely the process dying after the claim
    // and before the send.
    let claimed = app
        .jobs()
        .claim_session(session_id, closed_at, DELIVERY_LEASE_MILLIS)
        .await
        .expect("claim the leg");
    assert_eq!(claimed.len(), 1);
    assert!(
        app.egress.attempts_to("discord.com").is_empty(),
        "no POST was dispatched, so there is nothing ambiguous about this leg"
    );

    let later = closed_at + DELIVERY_LEASE_MILLIS + 60_000;
    let recovered = app.recover(later).await;
    assert_eq!(
        recovered,
        RecoveredCounts {
            released: 1,
            abandoned: 0
        },
        "a claim that dispatched no request is retried, not recorded as a loss"
    );

    let counts = app.tick(later).await;
    assert_eq!(counts.settled.succeeded, 1);
    assert_eq!(app.egress.attempts_to("discord.com").len(), 1);
    assert_eq!(
        states(&legs_of(&app.pool, session_id).await),
        vec!["succeeded"]
    );
}

#[tokio::test]
async fn a_discord_leg_interrupted_before_its_attempt_began_is_retried() {
    // The ambiguity forbids the retry, not the destination: a Discord leg with
    // no claim (the process died between the commit and the spawn) is simply
    // owed, and the sweep posts it.
    let app = test_app().await;
    let (_owner, _definition_id, session_id) = owner_net_and_session(
        &app,
        json!({ "emails": [], "webhookUrl": null, "discordWebhookUrl": DISCORD_URL }),
    )
    .await;
    let closed_at = app.close_without_spawning(session_id).await;
    let before = legs_of(&app.pool, session_id).await;
    assert_eq!(before[0].attempts, 0);
    assert!(!before[0].claimed);

    let recovered = app
        .recover(closed_at + DELIVERY_LEASE_MILLIS + 60_000)
        .await;
    assert_eq!(
        recovered,
        RecoveredCounts::default(),
        "nothing was interrupted"
    );
    let counts = app.tick(closed_at).await;
    assert_eq!(counts.settled.succeeded, 1);
    assert_eq!(app.egress.attempts_to("discord.com").len(), 1);
    assert_eq!(
        states(&legs_of(&app.pool, session_id).await),
        vec!["succeeded"]
    );
}

// --- A delivery that can never succeed becomes terminal, not eternal --

#[tokio::test]
async fn an_unreplayable_log_terminates_every_leg_of_the_session_and_no_later_tick_reclaims_it() {
    // Two addresses and a webhook; the snapshot loses its `connections` key
    // (the older shape, `is_unreplayable_log`). One
    // tick must move ALL THREE legs to `skipped`; the next must claim nothing.
    let app = test_app().await;
    let (_owner, _definition_id, session_id) = owner_net_and_session(
        &app,
        json!({ "emails": ["a@example.com", "b@example.com"], "webhookUrl": WEBHOOK_URL }),
    )
    .await;
    let closed_at = app.close_without_spawning(session_id).await;
    sqlx::query(
        "UPDATE net_sessions SET definition_snapshot = definition_snapshot - 'connections'
          WHERE id = $1",
    )
    .bind(session_id)
    .execute(&app.pool)
    .await
    .expect("strip the connection set");

    let first = app.tick(closed_at).await;
    assert_eq!(first.claimed, 3);
    assert_eq!(first.sessions, 1);
    assert_eq!(
        first.settled.skipped, 3,
        "every leg of the session, not the claimed one"
    );
    assert_eq!(
        states(&legs_of(&app.pool, session_id).await),
        vec!["skipped", "skipped", "skipped"]
    );
    assert!(app.mailer.recipients().is_empty());
    assert!(app.egress.requests().is_empty());

    let second = app.tick(closed_at + 1).await;
    assert_eq!(
        second.claimed, 0,
        "nothing left to claim: no infinite retry"
    );
}

#[tokio::test]
async fn a_permanently_rejected_webhook_is_failed_and_never_reclaimed() {
    let app = test_app_with(FakeEgress::always(400)).await;
    let (_owner, _definition_id, session_id) =
        owner_net_and_session(&app, json!({ "emails": [], "webhookUrl": WEBHOOK_URL })).await;
    let closed_at = app.close_without_spawning(session_id).await;

    let counts = app.tick(closed_at).await;
    assert_eq!(counts.settled.failed, 1);
    assert_eq!(
        app.egress.requests().len(),
        1,
        "a permanent status is not retried in-run either"
    );
    assert_eq!(
        states(&legs_of(&app.pool, session_id).await),
        vec!["failed"]
    );
    assert_eq!(app.tick(closed_at + 1).await.claimed, 0);
}

#[tokio::test]
async fn a_permanently_refused_egress_is_failed_and_never_reclaimed() {
    let app = test_app_with(FakeEgress::refusing("hooks.example.com")).await;
    let (_owner, _definition_id, session_id) =
        owner_net_and_session(&app, json!({ "emails": [], "webhookUrl": WEBHOOK_URL })).await;
    let closed_at = app.close_without_spawning(session_id).await;

    let counts = app.tick(closed_at).await;
    assert_eq!(counts.settled.failed, 1);
    assert_eq!(app.egress.requests().len(), 1);
    assert_eq!(
        states(&legs_of(&app.pool, session_id).await),
        vec!["failed"]
    );
    assert_eq!(app.tick(closed_at + 1).await.claimed, 0);
}

#[tokio::test]
async fn a_webhook_with_no_stored_secret_is_failed_without_a_post() {
    let app = test_app().await;
    let (_owner, definition_id, session_id) =
        owner_net_and_session(&app, json!({ "emails": [], "webhookUrl": WEBHOOK_URL })).await;
    let closed_at = app.close_without_spawning(session_id).await;
    sqlx::query("UPDATE net_delivery_configs SET webhook_secret = NULL WHERE definition_id = $1")
        .bind(Uuid::parse_str(&definition_id).expect("uuid"))
        .execute(&app.pool)
        .await
        .expect("null the secret");

    let counts = app.tick(closed_at).await;
    assert_eq!(counts.settled.failed, 1);
    assert!(
        app.egress.requests().is_empty(),
        "never signed with a fabricated key"
    );
    assert_eq!(
        states(&legs_of(&app.pool, session_id).await),
        vec!["failed"]
    );
}

#[tokio::test]
async fn a_target_cleared_between_the_plan_and_the_send_is_skipped_while_its_siblings_deliver() {
    // The owner drops the webhook and one address after the close but before
    // the sweep reaches the legs: those two legs are `skipped` (not
    // `succeeded`, not retried); the surviving address still delivers.
    let app = test_app().await;
    let (owner, definition_id, session_id) = owner_net_and_session(
        &app,
        json!({ "emails": ["keep@example.com", "drop@example.com"], "webhookUrl": WEBHOOK_URL }),
    )
    .await;
    let closed_at = app.close_without_spawning(session_id).await;
    assert_eq!(legs_of(&app.pool, session_id).await.len(), 3);
    set_delivery_config(
        &app,
        &owner,
        &definition_id,
        json!({ "emails": ["keep@example.com"], "webhookUrl": null }),
    )
    .await;

    let counts = app.tick(closed_at).await;
    assert_eq!(counts.claimed, 3);
    assert_eq!(counts.settled.succeeded, 1);
    assert_eq!(counts.settled.skipped, 2);
    assert_eq!(app.mailer.recipients(), vec!["keep@example.com"]);
    assert!(app.egress.requests().is_empty());
    let legs = legs_of(&app.pool, session_id).await;
    let by_target = |target: &str| {
        legs.iter()
            .find(|l| l.target == target)
            .expect("leg")
            .state
            .clone()
    };
    assert_eq!(by_target("keep@example.com"), "succeeded");
    assert_eq!(by_target("drop@example.com"), "skipped");
    assert_eq!(
        legs.iter()
            .find(|l| l.destination == "webhook")
            .expect("webhook")
            .state,
        "skipped"
    );
}

#[tokio::test]
async fn a_dead_receiver_exhausts_the_attempt_budget_and_is_failed_rather_than_retried_forever() {
    // Every tick spends the inner ladder (three 500s) and the outer ladder
    // reschedules — until the tenth claim, which is recorded as `failed`. The
    // eleventh tick claims nothing.
    let app = test_app_with(FakeEgress::always(500)).await;
    let (_owner, _definition_id, session_id) =
        owner_net_and_session(&app, json!({ "emails": [], "webhookUrl": WEBHOOK_URL })).await;
    let closed_at = app.close_without_spawning(session_id).await;

    let mut at = closed_at;
    for claim in 1..=MAX_JOB_ATTEMPTS {
        let counts = app.tick(at).await;
        assert_eq!(counts.claimed, 1, "claim {claim} happened at {at}");
        let leg = &legs_of(&app.pool, session_id).await[0];
        assert_eq!(leg.attempts, claim);
        if claim < MAX_JOB_ATTEMPTS {
            assert_eq!(counts.settled.rescheduled, 1);
            assert_eq!(leg.state, "pending");
            assert_eq!(
                leg.next_attempt_at_millis,
                (at + retry_delay_millis(claim)) as i64
            );
            at += retry_delay_millis(claim);
        } else {
            assert_eq!(counts.settled.failed, 1);
            assert_eq!(leg.state, "failed");
        }
    }
    assert_eq!(
        app.egress.requests().len(),
        3 * MAX_JOB_ATTEMPTS as usize,
        "three in-run attempts per claim, ten claims"
    );
    assert_eq!(
        app.tick(at + 60 * 60_000).await.claimed,
        0,
        "terminal: never claimed again"
    );
}

// --- The fifth of a shape -------------------------------------------

#[tokio::test]
async fn a_session_is_claimed_whole_or_not_at_all_so_its_log_folds_once() {
    // ONE fold per session. The tick groups by session, but the claim
    // bounded LEGS while a permit buys a SESSION, so a session straddling the
    // limit had part of its legs claimed now and the rest a tick later — and
    // its event log folded once per part. Two sessions at the domain's address
    // cap put twenty legs against sixteen permits, which is exactly that split.
    let app = test_app().await;
    let owner = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;
    let definition_id = create_net(&app, &owner).await;
    let emails: Vec<String> = (0..10).map(|i| format!("r{i}@example.com")).collect();
    set_delivery_config(
        &app,
        &owner,
        &definition_id,
        json!({ "emails": emails, "webhookUrl": null }),
    )
    .await;
    let mut closed_at = 0;
    for _ in 0..2 {
        let session_id = start_session(&app, &owner, &definition_id).await;
        closed_at = app.close_without_spawning(session_id).await;
    }

    let first = app.tick(closed_at).await;
    assert_eq!(first.sessions, 2);
    assert_eq!(
        first.claimed, 20,
        "both sessions claimed whole: a session is never split across ticks"
    );
    assert_eq!(
        app.tick(closed_at + 1).await.sessions,
        0,
        "nothing is left over to fold a second time"
    );
}

#[tokio::test]
async fn one_tick_claims_at_most_the_concurrency_ceiling_and_the_rest_wait_for_the_next() {
    // Seventeen closed sessions, one webhook leg each. A tick claims sixteen
    // (the process-wide in-flight ceiling), the seventeenth waits a tick.
    let app = test_app().await;
    let owner = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;
    let definition_id = create_net(&app, &owner).await;
    set_delivery_config(
        &app,
        &owner,
        &definition_id,
        json!({ "emails": [], "webhookUrl": WEBHOOK_URL }),
    )
    .await;
    let mut closed_at = 0;
    for _ in 0..=MAX_CONCURRENT_DELIVERIES {
        let session_id = start_session(&app, &owner, &definition_id).await;
        closed_at = app.close_without_spawning(session_id).await;
    }
    let pending = |pool: &PgPool| {
        let pool = pool.clone();
        async move {
            sqlx::query_scalar::<_, i64>(
                "SELECT count(*) FROM net_delivery_jobs WHERE state = 'pending'",
            )
            .fetch_one(&pool)
            .await
            .expect("count")
        }
    };
    assert_eq!(
        pending(&app.pool).await,
        (MAX_CONCURRENT_DELIVERIES + 1) as i64
    );

    let first = app.tick(closed_at).await;
    assert_eq!(first.claimed, MAX_CONCURRENT_DELIVERIES as u64);
    assert_eq!(first.sessions, MAX_CONCURRENT_DELIVERIES as u64);
    assert_eq!(first.settled.succeeded, MAX_CONCURRENT_DELIVERIES as u64);
    assert_eq!(pending(&app.pool).await, 1);

    let second = app.tick(closed_at).await;
    assert_eq!(second.claimed, 1);
    assert_eq!(pending(&app.pool).await, 0);
    assert_eq!(app.egress.requests().len(), MAX_CONCURRENT_DELIVERIES + 1);
}

#[tokio::test]
async fn a_session_with_three_legs_is_claimed_as_one_group_so_it_folds_once() {
    // Structurally: three armed destinations, one tick, one session
    // group — the executor folds per group, so this is one fold.
    let app = test_app().await;
    let (_owner, _definition_id, session_id) = owner_net_and_session(
        &app,
        json!({ "emails": ["log@example.com"], "webhookUrl": WEBHOOK_URL, "discordWebhookUrl": DISCORD_URL }),
    )
    .await;
    let closed_at = app.close_without_spawning(session_id).await;

    let counts = app.tick(closed_at).await;
    assert_eq!(counts.claimed, 3);
    assert_eq!(
        counts.sessions, 1,
        "three legs, ONE session group, ONE fold"
    );
    assert_eq!(counts.settled.succeeded, 3);
    assert_eq!(app.mailer.recipients(), vec!["log@example.com"]);
    assert_eq!(app.egress.attempts_to("hooks.example.com").len(), 1);
    assert_eq!(app.egress.attempts_to("discord.com").len(), 1);
}

#[tokio::test]
async fn a_failing_leg_moves_its_row_while_a_failing_tick_returns_its_own_error() {
    // The two error paths, side by side. A refused webhook is a LEG failure:
    // the tick is `Ok`, the row moves. A tick whose own queries cannot run
    // (the pool is closed) returns `Err` to the one boundary that logs it.
    let app = test_app_with(FakeEgress::refusing("hooks.example.com")).await;
    let (_owner, _definition_id, session_id) =
        owner_net_and_session(&app, json!({ "emails": [], "webhookUrl": WEBHOOK_URL })).await;
    let closed_at = app.close_without_spawning(session_id).await;

    let counts = app.tick(closed_at).await;
    assert_eq!(counts.errored, 0);
    assert_eq!(counts.settled.failed, 1);
    assert_eq!(
        states(&legs_of(&app.pool, session_id).await),
        vec!["failed"]
    );

    let doomed =
        PgPoolOptions::new().connect_lazy_with(app.pool.connect_options().as_ref().clone());
    doomed.close().await;
    let tick = run_delivery_sweep_tick(
        &DeliveryJobRepo::new(doomed),
        &app.state.delivery_service(),
        closed_at,
    )
    .await;
    assert!(
        tick.is_err(),
        "the tick's own storage failure propagates rather than panicking"
    );
}

// --- The table does not grow without a ceiling -------------------------

#[tokio::test]
async fn terminal_rows_past_retention_are_pruned_by_the_tick_and_the_prune_is_idempotent() {
    let app = test_app().await;
    let (_owner, _definition_id, session_id) = owner_net_and_session(
        &app,
        json!({ "emails": ["log@example.com"], "webhookUrl": null }),
    )
    .await;
    let closed_at = app.close_without_spawning(session_id).await;
    let delivered = app.tick(closed_at).await;
    assert_eq!(delivered.settled.succeeded, 1);
    assert_eq!(delivered.pruned, 0);

    let inside = app
        .tick(closed_at + DELIVERY_JOB_RETENTION_MILLIS - 1)
        .await;
    assert_eq!(inside.pruned, 0, "inside the window: kept");
    assert_eq!(legs_of(&app.pool, session_id).await.len(), 1);

    let past = app
        .tick(closed_at + DELIVERY_JOB_RETENTION_MILLIS + 1)
        .await;
    assert_eq!(past.pruned, 1);
    assert!(legs_of(&app.pool, session_id).await.is_empty());

    let again = app
        .tick(closed_at + DELIVERY_JOB_RETENTION_MILLIS + 2)
        .await;
    assert_eq!(again.pruned, 0, "idempotent");
}
