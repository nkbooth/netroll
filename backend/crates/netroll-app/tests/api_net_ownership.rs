// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
//! Full-loop API tests for owner management: real
//! router, real Postgres (testcontainers), capturing fake mailer. Asserts
//! status codes, problem+json `type` slugs, owner-set membership, and equal
//! authority (a co-owner can edit/manage) — never message prose (house
//! TDD rule).

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use netroll_app::http::{AppState, api_router};
use netroll_domain::consent::CURRENT_TERMS_VERSION;
use netroll_domain::deletion::DELETION_GRACE_MILLIS;
use netroll_domain::ports::{BoxFuture, Clock, MailError, Mailer};
use serde_json::{Value, json};
use sqlx::PgPool;
use sqlx::postgres::PgPoolOptions;
use testcontainers_modules::postgres::Postgres;
use testcontainers_modules::testcontainers::ContainerAsync;
use testcontainers_modules::testcontainers::runners::AsyncRunner;
use tower::ServiceExt;

/// Wall clock the lifecycle tests wind forward — soft-delete/finalize key off
/// `deletion_verdict(now)`, so time is a test input, never a real sleep.
#[derive(Clone)]
struct FakeClock {
    millis: Arc<AtomicU64>,
}

impl FakeClock {
    fn new(start: u64) -> Self {
        Self {
            millis: Arc::new(AtomicU64::new(start)),
        }
    }
    fn set(&self, millis: u64) {
        self.millis.store(millis, Ordering::SeqCst);
    }
}

impl Clock for FakeClock {
    fn now_epoch_millis(&self) -> u64 {
        self.millis.load(Ordering::SeqCst)
    }
}

const START_MILLIS: u64 = 1_800_000_000_000;

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
    clock: FakeClock,
    _pool: PgPool,
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
    let clock = FakeClock::new(START_MILLIS);
    let mut state = AppState::new(pool.clone(), mailer.clone(), "http://localhost:5173".into());
    state.clock = Arc::new(clock.clone());
    TestApp {
        _container: container,
        state,
        mailer,
        clock,
        _pool: pool,
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

/// Signs in a fresh email and records consent. Returns the session cookie.
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

/// Signs in, consents, AND reserves a callsign. Returns (cookie, accountId).
async fn sign_in_consent_callsign(app: &TestApp, email: &str, callsign: &str) -> (String, String) {
    let cookie = sign_in_and_consent(app, email).await;
    let (status, body) = send_json(
        app.router(),
        "PUT",
        "/api/accounts/me/callsign",
        Some(json!({ "callsign": callsign })),
        Some(&cookie),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    (cookie, body["id"].as_str().expect("account id").to_owned())
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

/// The scalar `PUT` body: `full_definition_json` without its `connections`,
/// which the scalar route refuses rather than ignores.
fn scalar_definition_json() -> Value {
    let mut body = full_definition_json();
    body.as_object_mut().expect("object").remove("connections");
    body
}

/// Creates a net owned by `cookie`'s account; returns its id.
async fn create_net(app: &TestApp, cookie: &str) -> String {
    let (status, created) = send_json(
        app.router(),
        "POST",
        "/api/net-definitions",
        Some(full_definition_json()),
        Some(cookie),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    created["id"].as_str().expect("id").to_owned()
}

fn owner_ids(body: &Value) -> Vec<String> {
    body["ownerAccountIds"]
        .as_array()
        .expect("ownerAccountIds array")
        .iter()
        .map(|v| v.as_str().expect("id string").to_owned())
        .collect()
}

const OWNERS_PATH: &str = "/owners";

#[tokio::test]
async fn add_co_owner_by_callsign_grants_equal_authority() {
    let app = test_app().await;
    let (a_cookie, a_id) = sign_in_consent_callsign(&app, "a@example.com", "w1aw").await;
    let (b_cookie, b_id) = sign_in_consent_callsign(&app, "b@example.com", "k2xyz").await;
    let net = create_net(&app, &a_cookie).await;

    // A adds B by callsign → 200, owner set contains BOTH.
    let (status, body) = send_json(
        app.router(),
        "POST",
        &format!("/api/net-definitions/{net}{OWNERS_PATH}"),
        Some(json!({ "callsign": "k2xyz" })),
        Some(&a_cookie),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let owners = owner_ids(&body);
    assert!(owners.contains(&a_id) && owners.contains(&b_id));
    assert_eq!(owners.len(), 2);
    // The owner-facing body carries owners with callsigns for display.
    let callsigns: Vec<_> = body["owners"]
        .as_array()
        .expect("owners array")
        .iter()
        .map(|o| o["callsign"].as_str().expect("callsign").to_owned())
        .collect();
    assert!(callsigns.contains(&"W1AW".to_owned()));
    assert!(callsigns.contains(&"K2XYZ".to_owned()));

    // Equal authority: B (a distinct session) can now edit.
    let mut edit = scalar_definition_json();
    edit["title"] = json!("Co-managed Net");
    let (status, edited) = send_json(
        app.router(),
        "PUT",
        &format!("/api/net-definitions/{net}"),
        Some(edit),
        Some(&b_cookie),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "a co-owner has edit authority");
    assert_eq!(edited["title"], "Co-managed Net");

    // B can also manage owners: add a third owner.
    let (_c_cookie, c_id) = sign_in_consent_callsign(&app, "c@example.com", "n3abc").await;
    let (status, body) = send_json(
        app.router(),
        "POST",
        &format!("/api/net-definitions/{net}{OWNERS_PATH}"),
        Some(json!({ "callsign": "n3abc" })),
        Some(&b_cookie),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "a co-owner can add owners too");
    assert!(owner_ids(&body).contains(&c_id));
}

#[tokio::test]
async fn adding_an_existing_owner_is_idempotent() {
    let app = test_app().await;
    let (a_cookie, _a_id) = sign_in_consent_callsign(&app, "a@example.com", "w1aw").await;
    let (_b_cookie, b_id) = sign_in_consent_callsign(&app, "b@example.com", "k2xyz").await;
    let net = create_net(&app, &a_cookie).await;

    for _ in 0..2 {
        let (status, body) = send_json(
            app.router(),
            "POST",
            &format!("/api/net-definitions/{net}{OWNERS_PATH}"),
            Some(json!({ "callsign": "k2xyz" })),
            Some(&a_cookie),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let owners = owner_ids(&body);
        assert_eq!(owners.len(), 2, "no duplicate owner");
        assert!(owners.contains(&b_id));
    }
}

#[tokio::test]
async fn adding_an_unknown_callsign_is_404_owner_not_found() {
    let app = test_app().await;
    let (a_cookie, _a_id) = sign_in_consent_callsign(&app, "a@example.com", "w1aw").await;
    let net = create_net(&app, &a_cookie).await;

    let (status, problem) = send_json(
        app.router(),
        "POST",
        &format!("/api/net-definitions/{net}{OWNERS_PATH}"),
        Some(json!({ "callsign": "n0body" })),
        Some(&a_cookie),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(problem["type"], "/errors/owner-not-found");
}

/// A callsign still resolves to its account through the deletion grace window
/// (soft-delete never clears the callsign) — but adding a departing account
/// as a co-owner is nonsensical (its session is already revoked, and it may
/// finalize at any moment). This must be refused the same way an unknown
/// callsign is: `404 /errors/owner-not-found`, never a silent 200 that hands
/// ownership to an account on its way out.
#[tokio::test]
async fn adding_a_soft_deleted_accounts_callsign_is_404_owner_not_found() {
    let app = test_app().await;
    let (a_cookie, _a_id) = sign_in_consent_callsign(&app, "a@example.com", "w1aw").await;
    let (b_cookie, _b_id) = sign_in_consent_callsign(&app, "b@example.com", "k2xyz").await;
    let net = create_net(&app, &a_cookie).await;

    // B soft-deletes (still within the grace window — the callsign is
    // untouched, only `deleted_at` is set and B's sessions are revoked).
    let (status, _) = send_json(
        app.router(),
        "DELETE",
        DELETE_ACCOUNT,
        None,
        Some(&b_cookie),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    let (status, problem) = send_json(
        app.router(),
        "POST",
        &format!("/api/net-definitions/{net}{OWNERS_PATH}"),
        Some(json!({ "callsign": "k2xyz" })),
        Some(&a_cookie),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(problem["type"], "/errors/owner-not-found");
}

#[tokio::test]
async fn a_non_owner_cannot_add_and_a_missing_net_is_404() {
    let app = test_app().await;
    let (a_cookie, _a_id) = sign_in_consent_callsign(&app, "a@example.com", "w1aw").await;
    let (c_cookie, _c_id) = sign_in_consent_callsign(&app, "c@example.com", "k2xyz").await;
    let net = create_net(&app, &a_cookie).await;

    // A consented non-owner is forbidden from managing the owner set.
    let (status, problem) = send_json(
        app.router(),
        "POST",
        &format!("/api/net-definitions/{net}{OWNERS_PATH}"),
        Some(json!({ "callsign": "w1aw" })),
        Some(&c_cookie),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(problem["type"], "/errors/forbidden");

    // A non-existent net id is 404 net-definition-not-found (not owner-not-found).
    let missing = uuid::Uuid::now_v7();
    let (status, problem) = send_json(
        app.router(),
        "POST",
        &format!("/api/net-definitions/{missing}{OWNERS_PATH}"),
        Some(json!({ "callsign": "w1aw" })),
        Some(&a_cookie),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(problem["type"], "/errors/net-definition-not-found");
}

#[tokio::test]
async fn remove_co_owner_revokes_their_authority() {
    let app = test_app().await;
    let (a_cookie, a_id) = sign_in_consent_callsign(&app, "a@example.com", "w1aw").await;
    let (b_cookie, b_id) = sign_in_consent_callsign(&app, "b@example.com", "k2xyz").await;
    let net = create_net(&app, &a_cookie).await;
    send_json(
        app.router(),
        "POST",
        &format!("/api/net-definitions/{net}{OWNERS_PATH}"),
        Some(json!({ "callsign": "k2xyz" })),
        Some(&a_cookie),
    )
    .await;

    // A removes B → 204.
    let (status, _) = send_json(
        app.router(),
        "DELETE",
        &format!("/api/net-definitions/{net}{OWNERS_PATH}/{b_id}"),
        None,
        Some(&a_cookie),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    // Owner set == [A]; B can no longer edit.
    let (_, body) = send_json(
        app.router(),
        "GET",
        &format!("/api/net-definitions/{net}"),
        None,
        Some(&a_cookie),
    )
    .await;
    assert_eq!(owner_ids(&body), vec![a_id]);
    let (status, _) = send_json(
        app.router(),
        "PUT",
        &format!("/api/net-definitions/{net}"),
        Some(scalar_definition_json()),
        Some(&b_cookie),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::FORBIDDEN,
        "a removed owner loses authority"
    );
}

#[tokio::test]
async fn an_owner_may_remove_themselves_while_another_remains() {
    let app = test_app().await;
    let (a_cookie, a_id) = sign_in_consent_callsign(&app, "a@example.com", "w1aw").await;
    let (b_cookie, b_id) = sign_in_consent_callsign(&app, "b@example.com", "k2xyz").await;
    let net = create_net(&app, &a_cookie).await;
    send_json(
        app.router(),
        "POST",
        &format!("/api/net-definitions/{net}{OWNERS_PATH}"),
        Some(json!({ "callsign": "k2xyz" })),
        Some(&a_cookie),
    )
    .await;

    // B removes B (self) → 204; owner set == [A].
    let (status, _) = send_json(
        app.router(),
        "DELETE",
        &format!("/api/net-definitions/{net}{OWNERS_PATH}/{b_id}"),
        None,
        Some(&b_cookie),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    let (_, body) = send_json(
        app.router(),
        "GET",
        &format!("/api/net-definitions/{net}"),
        None,
        Some(&a_cookie),
    )
    .await;
    assert_eq!(owner_ids(&body), vec![a_id]);
}

/// `definition_version` tracks FIELD edits — adding or removing an
/// owner is not a field edit and must never bump it (Dev Notes: "Confirm in a
/// test").
#[tokio::test]
async fn adding_and_removing_an_owner_does_not_bump_definition_version() {
    let app = test_app().await;
    let (a_cookie, _a_id) = sign_in_consent_callsign(&app, "a@example.com", "w1aw").await;
    let (_b_cookie, b_id) = sign_in_consent_callsign(&app, "b@example.com", "k2xyz").await;
    let net = create_net(&app, &a_cookie).await;
    let (_, created) = send_json(
        app.router(),
        "GET",
        &format!("/api/net-definitions/{net}"),
        None,
        Some(&a_cookie),
    )
    .await;
    let starting_version = created["definitionVersion"].as_i64().expect("version");

    let (status, added) = send_json(
        app.router(),
        "POST",
        &format!("/api/net-definitions/{net}{OWNERS_PATH}"),
        Some(json!({ "callsign": "k2xyz" })),
        Some(&a_cookie),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        added["definitionVersion"].as_i64().expect("version"),
        starting_version,
        "adding an owner is not a field edit — definitionVersion must not move"
    );

    let (status, _) = send_json(
        app.router(),
        "DELETE",
        &format!("/api/net-definitions/{net}{OWNERS_PATH}/{b_id}"),
        None,
        Some(&a_cookie),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    let (_, after_remove) = send_json(
        app.router(),
        "GET",
        &format!("/api/net-definitions/{net}"),
        None,
        Some(&a_cookie),
    )
    .await;
    assert_eq!(
        after_remove["definitionVersion"].as_i64().expect("version"),
        starting_version,
        "removing an owner is not a field edit — definitionVersion must not move"
    );
}

#[tokio::test]
async fn removing_the_last_owner_is_refused_with_409_last_owner() {
    let app = test_app().await;
    let (a_cookie, a_id) = sign_in_consent_callsign(&app, "a@example.com", "w1aw").await;
    let net = create_net(&app, &a_cookie).await;

    // A is the sole owner; removing A would orphan the net → 409.
    let (status, problem) = send_json(
        app.router(),
        "DELETE",
        &format!("/api/net-definitions/{net}{OWNERS_PATH}/{a_id}"),
        None,
        Some(&a_cookie),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(problem["type"], "/errors/last-owner");

    // A is still an owner and can still edit.
    let (_, body) = send_json(
        app.router(),
        "GET",
        &format!("/api/net-definitions/{net}"),
        None,
        Some(&a_cookie),
    )
    .await;
    assert_eq!(owner_ids(&body), vec![a_id]);
    let (status, _) = send_json(
        app.router(),
        "PUT",
        &format!("/api/net-definitions/{net}"),
        Some(scalar_definition_json()),
        Some(&a_cookie),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn removing_a_non_owner_account_is_404_owner_not_found() {
    let app = test_app().await;
    let (a_cookie, _a_id) = sign_in_consent_callsign(&app, "a@example.com", "w1aw").await;
    // A second owner so the net is not sole-owned (isolates non-member from
    // last-owner).
    let (_b_cookie, _b_id) = sign_in_consent_callsign(&app, "b@example.com", "k2xyz").await;
    let net = create_net(&app, &a_cookie).await;
    send_json(
        app.router(),
        "POST",
        &format!("/api/net-definitions/{net}{OWNERS_PATH}"),
        Some(json!({ "callsign": "k2xyz" })),
        Some(&a_cookie),
    )
    .await;

    // An accountId that is not a member of the owner set.
    let stranger = uuid::Uuid::now_v7();
    let (status, problem) = send_json(
        app.router(),
        "DELETE",
        &format!("/api/net-definitions/{net}{OWNERS_PATH}/{stranger}"),
        None,
        Some(&a_cookie),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(problem["type"], "/errors/owner-not-found");
}

// --- Ownership lifecycle: archival on account-finalize --------

const DELETE_ACCOUNT: &str = "/api/accounts/me";

#[tokio::test]
async fn a_solely_owned_net_is_archived_when_its_owner_finalizes() {
    let app = test_app().await;
    let (a_cookie, _a_id) = sign_in_consent_callsign(&app, "a@example.com", "w1aw").await;
    let net = create_net(&app, &a_cookie).await;
    let net_id = uuid::Uuid::parse_str(&net).expect("net uuid");

    // A soft-deletes their account.
    let (status, _) = send_json(
        app.router(),
        "DELETE",
        DELETE_ACCOUNT,
        None,
        Some(&a_cookie),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    // Finalize past the grace window, then run the ownerless sweep — do NOT
    // sleep; the injected clock is the mechanism (mirrors the finalize path).
    let now = START_MILLIS + DELETION_GRACE_MILLIS + 1;
    let finalized = app
        .state
        .accounts
        .finalize_deletions(now, DELETION_GRACE_MILLIS)
        .await
        .expect("finalize");
    assert_eq!(finalized, 1, "the sole owner's account is finalized");
    let archived = app
        .state
        .net_definitions
        .archive_ownerless(now)
        .await
        .expect("archive sweep");
    assert_eq!(archived, 1, "the now-ownerless net is archived");

    // The net row survives, is archived, and is absent from discovery.
    let def = app
        .state
        .net_definitions
        .find_by_id(net_id)
        .await
        .expect("query")
        .expect("net row survives (archived, not deleted)");
    assert!(def.archived_at_millis.is_some(), "the net is archived");
    let discoverable = app
        .state
        .net_definitions
        .list_discoverable()
        .await
        .expect("list discoverable");
    assert!(
        !discoverable.iter().any(|d| d.id == net_id),
        "an archived net leaves discovery"
    );
}

#[tokio::test]
async fn a_co_owned_net_survives_when_one_owner_finalizes() {
    let app = test_app().await;
    let (a_cookie, _a_id) = sign_in_consent_callsign(&app, "a@example.com", "w1aw").await;
    let (b_cookie, b_id) = sign_in_consent_callsign(&app, "b@example.com", "k2xyz").await;
    let net = create_net(&app, &a_cookie).await;
    let net_id = uuid::Uuid::parse_str(&net).expect("net uuid");
    send_json(
        app.router(),
        "POST",
        &format!("/api/net-definitions/{net}{OWNERS_PATH}"),
        Some(json!({ "callsign": "k2xyz" })),
        Some(&a_cookie),
    )
    .await;

    // A soft-deletes and finalizes.
    let (status, _) = send_json(
        app.router(),
        "DELETE",
        DELETE_ACCOUNT,
        None,
        Some(&a_cookie),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let now = START_MILLIS + DELETION_GRACE_MILLIS + 1;
    app.state
        .accounts
        .finalize_deletions(now, DELETION_GRACE_MILLIS)
        .await
        .expect("finalize");
    let archived = app
        .state
        .net_definitions
        .archive_ownerless(now)
        .await
        .expect("archive sweep");
    assert_eq!(archived, 0, "a co-owned net is NOT archived");

    // Net not archived; owner set == [B]; B can still edit.
    let def = app
        .state
        .net_definitions
        .find_by_id(net_id)
        .await
        .expect("query")
        .expect("net present");
    assert!(def.archived_at_millis.is_none());
    assert_eq!(
        def.owner_account_ids,
        vec![uuid::Uuid::parse_str(&b_id).expect("b uuid")]
    );
    let (status, _) = send_json(
        app.router(),
        "PUT",
        &format!("/api/net-definitions/{net}"),
        Some(scalar_definition_json()),
        Some(&b_cookie),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "the surviving owner can still edit");
}

#[tokio::test]
async fn archival_is_tied_to_finalize_not_soft_delete_and_undelete_restores() {
    let app = test_app().await;
    let (a_cookie, a_id) = sign_in_consent_callsign(&app, "a@example.com", "w1aw").await;
    let net = create_net(&app, &a_cookie).await;
    let net_id = uuid::Uuid::parse_str(&net).expect("net uuid");

    // A soft-deletes — but the grace window has NOT elapsed. The owner row is
    // intact, so the sweep archives nothing.
    let (status, _) = send_json(
        app.router(),
        "DELETE",
        DELETE_ACCOUNT,
        None,
        Some(&a_cookie),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let in_grace = START_MILLIS + DELETION_GRACE_MILLIS / 2;
    app.clock.set(in_grace);
    let archived = app
        .state
        .net_definitions
        .archive_ownerless(in_grace)
        .await
        .expect("archive sweep");
    assert_eq!(
        archived, 0,
        "an in-grace net is not archived (owner row intact)"
    );
    let def = app
        .state
        .net_definitions
        .find_by_id(net_id)
        .await
        .expect("query")
        .expect("net present");
    assert!(def.archived_at_millis.is_none());
    assert_eq!(
        def.owner_account_ids,
        vec![uuid::Uuid::parse_str(&a_id).expect("a uuid")]
    );

    // A signs back in within the window (undelete), then can still manage the net.
    let a_cookie = sign_in_and_consent(&app, "a@example.com").await;
    let (status, body) = send_json(
        app.router(),
        "GET",
        &format!("/api/net-definitions/{net}"),
        None,
        Some(&a_cookie),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "undelete restores full management");
    assert_eq!(owner_ids(&body), vec![a_id]);
}
