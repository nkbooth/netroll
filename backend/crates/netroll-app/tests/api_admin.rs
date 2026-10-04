// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
//! Full-loop API tests for the bounded admin surface
//! and disabled-account enforcement: real router, real Postgres (testcontainers),
//! capturing fake mailer. Asserts status codes, the closed capability surface,
//! the recorded audit ROWS, and the disable/verify enforcement DECISIONS — never
//! response prose (house TDD rule).

use std::sync::{Arc, Mutex};

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use netroll_app::http::admin::PER_TYPE_SEARCH_LIMIT;
use netroll_app::http::rate_limit::NET_CREATION_BURST;
use netroll_app::http::{AppState, api_router};
use netroll_domain::admin::{
    AdminCapability, DEFAULT_PAGE_LIMIT, MAX_PAGE_LIMIT, PageCursor, encode_cursor,
    encode_cursor_for, filter_fingerprint,
};
use netroll_domain::deletion::DELETION_GRACE_MILLIS;
use netroll_domain::ports::{BoxFuture, MailError, Mailer};
use serde_json::{Value, json};
use sqlx::PgPool;
use sqlx::postgres::PgPoolOptions;
use testcontainers_modules::postgres::Postgres;
use testcontainers_modules::testcontainers::ContainerAsync;
use testcontainers_modules::testcontainers::runners::AsyncRunner;
use tower::ServiceExt;

const ADMIN_EMAIL: &str = "admin@example.com";

#[derive(Default)]
struct CapturingMailer {
    sent: Mutex<Vec<(String, String)>>,
}
impl CapturingMailer {
    fn last_link(&self) -> String {
        self.sent
            .lock()
            .expect("lock")
            .last()
            .expect("a mail sent")
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
        n: &'a str,
    ) -> BoxFuture<'a, Result<(), MailError>> {
        Box::pin(async move {
            self.sent
                .lock()
                .expect("lock")
                .push((to.to_owned(), n.to_owned()));
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

async fn admin_app() -> TestApp {
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
        .with_admin_allowlist(vec![ADMIN_EMAIL.to_owned()]);
    TestApp {
        _container: container,
        state,
        mailer,
        pool,
    }
}

async fn send(
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
        .expect("build");
    let response = router.oneshot(request).await.expect("route");
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024)
        .await
        .expect("body");
    let json = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).expect("json")
    };
    (status, json)
}

/// GET returning status + `content-type` + the RAW body bytes.
///
/// An extractor rejection's body is not necessarily JSON, so the media type has
/// to be asserted BEFORE any parse is attempted — [`send`] would panic on a
/// `text/plain` rejection body and hide which contract actually answered.
async fn send_untyped(
    router: Router,
    uri: &str,
    cookie: Option<&str>,
) -> (StatusCode, String, String) {
    let mut builder = Request::builder().method("GET").uri(uri);
    if let Some(cookie) = cookie {
        builder = builder.header(header::COOKIE, cookie);
    }
    let request = builder.body(Body::empty()).expect("build");
    let response = router.oneshot(request).await.expect("route");
    let status = response.status();
    let content_type = response
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_owned();
    let bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024)
        .await
        .expect("body");
    (
        status,
        content_type,
        String::from_utf8(bytes.to_vec()).expect("utf8 body"),
    )
}

/// Signs in via the magic-link flow and returns `(session cookie, account id)`.
async fn sign_in(app: &TestApp, email: &str) -> (String, String) {
    let (status, _) = send(
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
        .expect("build");
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
    let bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024)
        .await
        .expect("body");
    let body: Value = serde_json::from_slice(&bytes).expect("json");
    let id = body["id"].as_str().expect("id").to_owned();
    (cookie, id)
}

async fn audit_rows(pool: &PgPool, action: &str) -> Vec<(uuid::Uuid, Option<uuid::Uuid>)> {
    sqlx::query_as::<_, (uuid::Uuid, Option<uuid::Uuid>)>(
        "SELECT actor_account_id, target_id FROM audit_log WHERE action = $1",
    )
    .bind(action)
    .fetch_all(pool)
    .await
    .expect("audit rows")
}

/// The full row content (actor/target/timestamp/metadata) for one audit action
/// — a real HTTP-triggered row's `occurred_at` and `metadata`, not just
/// actor/target. Expects exactly one row for `action`.
async fn audit_row_full(
    pool: &PgPool,
    action: &str,
) -> (
    uuid::Uuid,
    Option<uuid::Uuid>,
    chrono::DateTime<chrono::Utc>,
    Option<Value>,
) {
    sqlx::query_as::<
        _,
        (
            uuid::Uuid,
            Option<uuid::Uuid>,
            chrono::DateTime<chrono::Utc>,
            Option<Value>,
        ),
    >(
        "SELECT actor_account_id, target_id, occurred_at, metadata FROM audit_log WHERE action = $1"
    )
    .bind(action)
    .fetch_one(pool)
    .await
    .expect("exactly one audit row for this action")
}

/// Seeds `count` `audit_log` rows straight into the table.
///
/// The only HTTP paths that write this table are the admin actions themselves,
/// so building a table LARGER than one page through the API would mean
/// hundreds of gated, individually-audited round trips. The clamp is only
/// observable above the clamp, so the seed has to be able to get there.
async fn seed_audit_rows(pool: &PgPool, actor: uuid::Uuid, count: usize) {
    let ids: Vec<uuid::Uuid> = (0..count).map(|_| uuid::Uuid::now_v7()).collect();
    sqlx::query(
        "INSERT INTO audit_log (id, occurred_at, actor_account_id, action)
         SELECT u.id,
                TIMESTAMPTZ '2020-01-01 00:00:00+00' + (u.ord::int * INTERVAL '1 second'),
                $2,
                'view-reports'
           FROM UNNEST($1::uuid[]) WITH ORDINALITY AS u(id, ord)",
    )
    .bind(&ids)
    .bind(actor)
    .execute(pool)
    .await
    .expect("seed audit rows");
}

/// Seeds one unresolved report per entry in `at_millis`, returning their ids in
/// queue order (oldest first, id ascending within a shared millisecond).
///
/// Seeded directly rather than through `POST /api/abuse-reports` because that
/// path is IP-rate-limited and stamps the real clock — this test needs two
/// reports to share a `created_at` exactly, which is where a timestamp-only
/// cursor breaks.
async fn seed_reports(pool: &PgPool, at_millis: &[i64]) -> Vec<uuid::Uuid> {
    let ids: Vec<uuid::Uuid> = (0..at_millis.len()).map(|_| uuid::Uuid::now_v7()).collect();
    let times: Vec<chrono::DateTime<chrono::Utc>> = at_millis
        .iter()
        .map(|m| chrono::DateTime::from_timestamp_millis(*m).expect("epoch millis in range"))
        .collect();
    sqlx::query(
        "INSERT INTO abuse_reports (id, created_at, body)
         SELECT u.id, u.at, 'seeded report ' || u.ord
           FROM UNNEST($1::uuid[], $2::timestamptz[]) WITH ORDINALITY AS u(id, at, ord)",
    )
    .bind(&ids)
    .bind(&times)
    .execute(pool)
    .await
    .expect("seed abuse reports");
    ids
}

/// Seeds `count` accounts whose callsigns all start with `prefix`, straight into
/// the table.
///
/// The per-type cap is only observable ABOVE the cap, and reaching it through
/// sign-in would be `count` magic-link round trips against a rate-limited
/// path. Callsigns and emails are both unique (`idx_accounts_callsign`, and
/// `email` is `UNIQUE NOT NULL`), so each gets the ordinal as a suffix.
async fn seed_accounts_with_callsign_prefix(pool: &PgPool, prefix: &str, count: usize) {
    let ids: Vec<uuid::Uuid> = (0..count).map(|_| uuid::Uuid::now_v7()).collect();
    sqlx::query(
        "INSERT INTO accounts (id, email, callsign)
         SELECT u.id,
                'seeded-' || $2 || '-' || u.ord || '@example.com',
                $2 || u.ord
           FROM UNNEST($1::uuid[]) WITH ORDINALITY AS u(id, ord)",
    )
    .bind(&ids)
    .bind(prefix)
    .execute(pool)
    .await
    .expect("seed accounts");
}

const ADMIN_ENDPOINTS: [(&str, &str); 6] = [
    ("GET", "/api/admin/abuse-reports"),
    ("GET", "/api/admin/audit-log"),
    ("GET", "/api/admin/search?q=W1ABC"),
    (
        "POST",
        "/api/admin/abuse-reports/00000000-0000-7000-8000-000000000000/resolve",
    ),
    (
        "POST",
        "/api/admin/accounts/00000000-0000-7000-8000-000000000000/disable",
    ),
    (
        "POST",
        "/api/admin/accounts/00000000-0000-7000-8000-000000000000/reenable",
    ),
];

// ---- The admin gate ------------------------------------------------------

#[tokio::test]
async fn an_unauthenticated_caller_gets_401_on_every_admin_endpoint() {
    let app = admin_app().await;
    for (method, uri) in ADMIN_ENDPOINTS {
        let (status, _) = send(app.router(), method, uri, None, None).await;
        assert_eq!(
            status,
            StatusCode::UNAUTHORIZED,
            "{method} {uri} without a session is 401"
        );
    }
}

#[tokio::test]
async fn a_signed_in_non_admin_gets_403_on_every_admin_endpoint() {
    let app = admin_app().await;
    let (cookie, _) = sign_in(&app, "regular@example.com").await;
    for (method, uri) in ADMIN_ENDPOINTS {
        let (status, problem) = send(app.router(), method, uri, None, Some(&cookie)).await;
        assert_eq!(
            status,
            StatusCode::FORBIDDEN,
            "{method} {uri} for a non-admin is 403"
        );
        assert_eq!(problem["type"], "/errors/forbidden");
    }
}

#[tokio::test]
async fn there_is_no_god_mode_route_to_read_a_users_qrz_credentials() {
    // The admin surface has no route that decrypts or
    // exposes a per-user secret — such a path simply does not exist (404), even
    // for an admin. The credential store stays admin-unreadable.
    let app = admin_app().await;
    let (admin_cookie, _) = sign_in(&app, ADMIN_EMAIL).await;
    for uri in [
        "/api/admin/accounts/00000000-0000-7000-8000-000000000000/qrz-credentials",
        "/api/admin/accounts/00000000-0000-7000-8000-000000000000/credentials",
        "/api/admin/accounts/00000000-0000-7000-8000-000000000000/secrets",
    ] {
        let (status, _) = send(app.router(), "GET", uri, None, Some(&admin_cookie)).await;
        assert_eq!(
            status,
            StatusCode::NOT_FOUND,
            "no admin credential-read route exists: {uri}"
        );
    }
}

// ---- Paged review reads -------------------------------------------------------

#[tokio::test]
async fn the_report_queue_pages_through_every_report_without_gap_or_repeat() {
    // Unpaginated, a large backlog would serialize the whole table on every
    // dashboard view. The cursor must still surface every report exactly once.
    let app = admin_app().await;
    let (admin_cookie, _) = sign_in(&app, ADMIN_EMAIL).await;
    // The middle pair shares a created_at, and with limit=2 that pair straddles
    // the first page boundary — exactly where a timestamp-only cursor would
    // skip one of them or serve one twice.
    let seeded = seed_reports(&app.pool, &[1_000, 2_000, 2_000, 3_000, 4_000]).await;

    let mut seen: Vec<String> = Vec::new();
    let mut uri = "/api/admin/abuse-reports?limit=2".to_owned();
    loop {
        let (status, body) = send(app.router(), "GET", &uri, None, Some(&admin_cookie)).await;
        assert_eq!(status, StatusCode::OK);
        let items = body["items"].as_array().expect("items array");
        assert!(items.len() <= 2, "a page never exceeds the requested limit");
        seen.extend(
            items
                .iter()
                .map(|i| i["id"].as_str().expect("id").to_owned()),
        );
        match body["nextCursor"].as_str() {
            Some(cursor) => {
                uri = format!("/api/admin/abuse-reports?limit=2&cursor={cursor}");
            }
            None => break,
        }
    }

    // Compared against the ids the test itself seeded, in the order the queue
    // promises — not against a second read, which could share the same defect.
    let expected: Vec<String> = seeded.iter().map(|id| id.to_string()).collect();
    assert_eq!(
        seen, expected,
        "every seeded report exactly once, oldest first, across every boundary"
    );
    let unique: std::collections::HashSet<_> = seen.iter().collect();
    assert_eq!(unique.len(), seen.len(), "no report is served twice");
}

#[tokio::test]
async fn an_unparseable_page_cursor_is_refused_rather_than_restarting_the_list() {
    // Silently restarting would turn a client bug into an endless first page.
    let app = admin_app().await;
    let (admin_cookie, _) = sign_in(&app, ADMIN_EMAIL).await;

    for uri in [
        "/api/admin/abuse-reports?cursor=nonsense",
        "/api/admin/audit-log?cursor=nonsense",
    ] {
        let (status, _) = send(app.router(), "GET", uri, None, Some(&admin_cookie)).await;
        assert_eq!(
            status,
            StatusCode::BAD_REQUEST,
            "{uri} rejects a bad cursor"
        );
    }
}

#[tokio::test]
async fn a_cursor_naming_an_unrepresentable_instant_is_refused_not_crashed_on() {
    // The cursor's millis are CLIENT-SUPPLIED and become a `DateTime<Utc>` at
    // the storage boundary, where the conversion is only defined inside chrono's
    // range. A value past it must land in the same refusal as any other cursor
    // this server did not issue; letting it through would abort the handler and
    // answer a typed-in query string with a 500.
    let app = admin_app().await;
    let (admin_cookie, _) = sign_in(&app, ADMIN_EMAIL).await;
    let (non_admin, _) = sign_in(&app, "regular@example.com").await;
    let unrepresentable = PageCursor {
        at_millis: 9_000_000_000_000_000,
        id: uuid::Uuid::parse_str("00000000-0000-7000-8000-0000000000ab").expect("uuid"),
    };
    for uri in [
        format!(
            "/api/admin/abuse-reports?cursor={}",
            encode_cursor(unrepresentable)
        ),
        format!(
            "/api/admin/audit-log?cursor={}",
            encode_cursor_for(unrepresentable, filter_fingerprint(None, None, None))
        ),
    ] {
        let (status, _) = send(app.router(), "GET", &uri, None, Some(&admin_cookie)).await;
        assert_eq!(
            status,
            StatusCode::BAD_REQUEST,
            "{uri} refuses an out-of-range cursor"
        );
        // The gate still runs FIRST: an out-of-range cursor must not answer 400
        // to a caller who should never learn the route exists at all.
        let (status, _) = send(app.router(), "GET", &uri, None, None).await;
        assert_eq!(
            status,
            StatusCode::UNAUTHORIZED,
            "{uri} without a session is still 401, not 400"
        );
        let (status, _) = send(app.router(), "GET", &uri, None, Some(&non_admin)).await;
        assert_eq!(
            status,
            StatusCode::FORBIDDEN,
            "{uri} for a signed-in non-admin is still 403, not 400"
        );
    }
}

#[tokio::test]
async fn an_oversized_page_limit_is_actually_truncated_to_the_maximum() {
    // A caller asking for everything must not be able to opt out of paging.
    //
    // The seed is deliberately LARGER than the clamp. Below it, "clamped" and
    // "no clamp exists at all" produce byte-identical responses, so a smaller
    // seed cannot fail however broken the clamp is.
    let app = admin_app().await;
    let (admin_cookie, admin_id) = sign_in(&app, ADMIN_EMAIL).await;
    let actor = uuid::Uuid::parse_str(&admin_id).expect("admin account id");
    seed_audit_rows(&app.pool, actor, MAX_PAGE_LIMIT + 1).await;

    let (status, body) = send(
        app.router(),
        "GET",
        "/api/admin/audit-log?limit=99999",
        None,
        Some(&admin_cookie),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        body["items"].as_array().expect("items").len(),
        MAX_PAGE_LIMIT,
        "the requested limit is discarded for the maximum"
    );
    assert_ne!(
        body["nextCursor"],
        Value::Null,
        "the truncation is visible to the caller as a further page"
    );
}

#[tokio::test]
async fn a_table_larger_than_one_page_returns_at_most_the_default_page_size() {
    // With no ?limit= at all — the shape the dashboard actually sends — a table
    // bigger than one page still yields one page, not the table.
    let app = admin_app().await;
    let (admin_cookie, admin_id) = sign_in(&app, ADMIN_EMAIL).await;
    let actor = uuid::Uuid::parse_str(&admin_id).expect("admin account id");
    seed_audit_rows(&app.pool, actor, MAX_PAGE_LIMIT + 1).await;

    let (status, body) = send(
        app.router(),
        "GET",
        "/api/admin/audit-log",
        None,
        Some(&admin_cookie),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        body["items"].as_array().expect("items").len(),
        DEFAULT_PAGE_LIMIT
    );
    assert_ne!(body["nextCursor"], Value::Null);
}

#[tokio::test]
async fn paging_params_do_not_change_the_admin_gate() {
    // Proves pagination did not open a second, ungated read path: the
    // AdminAccount extractor still runs before any cursor work, so a bad cursor
    // cannot answer 400 (and thereby confirm the route exists) to a caller who
    // should be seeing 401/403.
    let app = admin_app().await;
    let (non_admin, _) = sign_in(&app, "regular@example.com").await;
    for uri in [
        "/api/admin/abuse-reports?limit=5&cursor=nonsense",
        "/api/admin/audit-log?limit=5&cursor=nonsense",
    ] {
        let (status, _) = send(app.router(), "GET", uri, None, None).await;
        assert_eq!(
            status,
            StatusCode::UNAUTHORIZED,
            "{uri} without a session is still 401"
        );
        let (status, _) = send(app.router(), "GET", uri, None, Some(&non_admin)).await;
        assert_eq!(
            status,
            StatusCode::FORBIDDEN,
            "{uri} for a signed-in non-admin is still 403"
        );
    }
}

#[tokio::test]
async fn the_admin_route_table_is_still_exactly_six_routes() {
    // The admin surface is a CLOSED capability set. Pagination is a shape
    // of an existing read, never a new privilege.
    //
    // WHAT THIS ACTUALLY GUARDS, precisely — axum's `Router` exposes no route
    // table to a test, so "exactly six routes" cannot be read off
    // `admin_routes()` directly:
    // * `ADMIN_ENDPOINTS` is pinned to the capability set, which lives in
    // another crate. The module contract at `http/admin.rs:1-12` is one
    // route per capability, so a seventh route added under that contract
    // brings a seventh `AdminCapability` and turns this RED. A seventh route
    // that reuses an EXISTING capability would still slip past — that case
    // is covered only by the guessed-path 404 sweep below, which is a
    // sampling, not a proof.
    // * that the six paths named here are each wired and gated
    // (`an_unauthenticated_caller_gets_401_on_every_admin_endpoint` walks
    // the same table and asserts 401, which an unrouted path could not
    // return — it would 404).
    let app = admin_app().await;
    let (admin_cookie, _) = sign_in(&app, ADMIN_EMAIL).await;
    assert_eq!(
        ADMIN_ENDPOINTS.len(),
        AdminCapability::EVERY.len(),
        "one admin route per bounded capability, and no more"
    );
    // No unpaged/bulk/count sibling grew alongside the paged reads.
    for uri in [
        "/api/admin/audit-log/all",
        "/api/admin/audit-log/count",
        "/api/admin/audit-log/export",
        "/api/admin/abuse-reports/all",
        "/api/admin/abuse-reports/count",
    ] {
        let (status, _) = send(app.router(), "GET", uri, None, Some(&admin_cookie)).await;
        assert_eq!(
            status,
            StatusCode::NOT_FOUND,
            "no seventh admin route exists: {uri}"
        );
    }
}

// ---- LookupAccount: turning a free-text report into an actionable id ----------

/// Signs in and reserves a callsign — the lookup's callsign arm needs one.
async fn sign_in_with_callsign(app: &TestApp, email: &str, callsign: &str) -> (String, String) {
    let (cookie, id) = sign_in(app, email).await;
    let (status, _) = send(
        app.router(),
        "POST",
        "/api/consents",
        Some(json!({ "termsVersion": netroll_domain::consent::CURRENT_TERMS_VERSION })),
        Some(&cookie),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    let (status, _) = send(
        app.router(),
        "PUT",
        "/api/accounts/me/callsign",
        Some(json!({ "callsign": callsign })),
        Some(&cookie),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    (cookie, id)
}

// ---- The account view's admin flag (the dashboard's only nav signal) ----------

#[tokio::test]
async fn the_account_view_reports_admin_status_from_the_allowlist() {
    // The frontend has no other way to know whether to offer the admin surface;
    // the flag must come from the SAME boot allowlist the gate enforces, so a
    // non-admin can never be shown an entry the server would then 403.
    let app = admin_app().await;

    let (admin_cookie, _) = sign_in(&app, ADMIN_EMAIL).await;
    let (status, body) = send(
        app.router(),
        "GET",
        "/api/accounts/me",
        None,
        Some(&admin_cookie),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["isAdmin"], json!(true));

    let (plain_cookie, _) = sign_in(&app, "regular@example.com").await;
    let (status, body) = send(
        app.router(),
        "GET",
        "/api/accounts/me",
        None,
        Some(&plain_cookie),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["isAdmin"], json!(false));
}

// ---- Bounded actions each land an audit row ------------------------------

#[tokio::test]
async fn an_admin_can_view_and_resolve_reports_and_each_lands_an_audit_row() {
    let app = admin_app().await;
    let (admin_cookie, admin_id) = sign_in(&app, ADMIN_EMAIL).await;
    let admin_uuid = uuid::Uuid::parse_str(&admin_id).expect("admin uuid");

    // A public report exists to action.
    let (status, _) = send(
        app.router(),
        "POST",
        "/api/abuse-reports",
        Some(json!({ "body": "spam" })),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED);
    let report_id: uuid::Uuid = sqlx::query_scalar("SELECT id FROM abuse_reports LIMIT 1")
        .fetch_one(&app.pool)
        .await
        .expect("report id");

    // View: 200 with the queue, and a ViewReports audit row for the admin.
    let (status, body) = send(
        app.router(),
        "GET",
        "/api/admin/abuse-reports",
        None,
        Some(&admin_cookie),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        body["items"].as_array().expect("items array").len(),
        1,
        "the unresolved report is listed"
    );
    let views = audit_rows(&app.pool, "view-reports").await;
    assert_eq!(views.len(), 1);
    assert_eq!(
        views[0].0, admin_uuid,
        "the audit actor is the acting admin"
    );

    // Resolve: 204, and a ResolveReport audit row targeting the report.
    let (status, _) = send(
        app.router(),
        "POST",
        &format!("/api/admin/abuse-reports/{report_id}/resolve"),
        None,
        Some(&admin_cookie),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let resolves = audit_rows(&app.pool, "resolve-report").await;
    assert_eq!(resolves.len(), 1);
    assert_eq!(resolves[0].0, admin_uuid);
    assert_eq!(
        resolves[0].1,
        Some(report_id),
        "the audit target is the report"
    );

    // The report leaves the unresolved queue.
    let (_, body) = send(
        app.router(),
        "GET",
        "/api/admin/abuse-reports",
        None,
        Some(&admin_cookie),
    )
    .await;
    assert_eq!(
        body["items"].as_array().expect("items array").len(),
        0,
        "a resolved report is gone from the queue"
    );
}

#[tokio::test]
async fn resolving_a_missing_report_is_404() {
    let app = admin_app().await;
    let (admin_cookie, _) = sign_in(&app, ADMIN_EMAIL).await;
    let (status, problem) = send(
        app.router(),
        "POST",
        "/api/admin/abuse-reports/00000000-0000-7000-8000-0000000000ff/resolve",
        None,
        Some(&admin_cookie),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(problem["type"], "/errors/abuse-report-not-found");
}

// ---- Disable enforcement + audit ----------------------------------------

#[tokio::test]
async fn disabling_an_account_blocks_it_lands_an_audit_row_and_reenable_restores_it() {
    let app = admin_app().await;
    let (admin_cookie, admin_id) = sign_in(&app, ADMIN_EMAIL).await;
    let admin_uuid = uuid::Uuid::parse_str(&admin_id).expect("admin uuid");
    let (victim_cookie, victim_id) = sign_in(&app, "abuser@example.com").await;
    let victim_uuid = uuid::Uuid::parse_str(&victim_id).expect("victim uuid");

    // The victim can act before being disabled.
    let (status, _) = send(
        app.router(),
        "GET",
        "/api/accounts/me",
        None,
        Some(&victim_cookie),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    // Admin disables the victim → 204, and a DisableAccount audit row targeting
    // the victim, carrying actor+action+target+timestamp+metadata
    // (PII/secret-free by construction).
    let before_disable = chrono::Utc::now();
    let (status, _) = send(
        app.router(),
        "POST",
        &format!("/api/admin/accounts/{victim_uuid}/disable"),
        None,
        Some(&admin_cookie),
    )
    .await;
    let after_disable = chrono::Utc::now();
    assert_eq!(status, StatusCode::NO_CONTENT);
    let disables = audit_rows(&app.pool, "disable-account").await;
    assert_eq!(disables.len(), 1);
    assert_eq!(disables[0].0, admin_uuid, "audit actor is the admin");
    assert_eq!(
        disables[0].1,
        Some(victim_uuid),
        "audit target is the victim"
    );

    // The full row, not just actor/target —
    // occurred_at falls inside the request's own wall-clock window, and
    // metadata is exactly the bounded, non-secret shape the handler writes.
    let (_, _, occurred_at, metadata) = audit_row_full(&app.pool, "disable-account").await;
    assert!(
        occurred_at >= before_disable && occurred_at <= after_disable,
        "occurred_at falls within the request's own wall-clock window"
    );
    assert_eq!(
        metadata,
        Some(json!({ "newlyDisabled": true })),
        "metadata is exactly the bounded, PII/secret-free shape the handler writes"
    );

    // The victim's session was revoked by the disable → its cookie no longer works.
    let (status, _) = send(
        app.router(),
        "GET",
        "/api/accounts/me",
        None,
        Some(&victim_cookie),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::UNAUTHORIZED,
        "the disabled account's session is gone"
    );

    // A FRESH sign-in attempt is distinctly refused at the VERIFY step (403
    // account-disabled) — signing back in does NOT clear the disable.
    let (status, _) = send(
        app.router(),
        "POST",
        "/api/magic-links",
        Some(json!({ "email": "abuser@example.com" })),
        None,
    )
    .await;
    assert_eq!(
        status,
        StatusCode::ACCEPTED,
        "the request step stays a uniform 202 (no oracle)"
    );
    let token = app
        .mailer
        .last_link()
        .split_once("token=")
        .expect("token")
        .1
        .to_owned();
    let (status, problem) = send(
        app.router(),
        "POST",
        "/api/sessions",
        Some(json!({ "token": token })),
        None,
    )
    .await;
    assert_eq!(
        status,
        StatusCode::FORBIDDEN,
        "the verify step refuses a disabled account"
    );
    assert_eq!(problem["type"], "/errors/account-disabled");

    // Admin re-enables → the account can sign in fresh again.
    let (status, _) = send(
        app.router(),
        "POST",
        &format!("/api/admin/accounts/{victim_uuid}/reenable"),
        None,
        Some(&admin_cookie),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert_eq!(audit_rows(&app.pool, "reenable-account").await.len(), 1);

    let (new_cookie, _) = sign_in(&app, "abuser@example.com").await;
    let (status, _) = send(
        app.router(),
        "GET",
        "/api/accounts/me",
        None,
        Some(&new_cookie),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "a re-enabled account signs in and acts again"
    );
}

#[tokio::test]
async fn disabling_a_missing_account_is_404() {
    let app = admin_app().await;
    let (admin_cookie, _) = sign_in(&app, ADMIN_EMAIL).await;
    let (status, problem) = send(
        app.router(),
        "POST",
        "/api/admin/accounts/00000000-0000-7000-8000-0000000000ee/disable",
        None,
        Some(&admin_cookie),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(problem["type"], "/errors/account-not-found");
}

#[tokio::test]
async fn an_admin_cannot_disable_their_own_account() {
    // Admin status is a boot-configured email
    // allowlist, not a DB-editable role, so a self-disable would revoke the
    // acting admin's own session and permanently refuse them from
    // signing back in to `reenable` themselves — an unrecoverable lockout on
    // a single-admin instance. Refused up front, no row touched, no audit
    // row written.
    let app = admin_app().await;
    let (admin_cookie, admin_id) = sign_in(&app, ADMIN_EMAIL).await;

    let (status, problem) = send(
        app.router(),
        "POST",
        &format!("/api/admin/accounts/{admin_id}/disable"),
        None,
        Some(&admin_cookie),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(problem["type"], "/errors/cannot-disable-self");

    // The admin's own session survives — self-disable never took effect.
    let (status, _) = send(
        app.router(),
        "GET",
        "/api/accounts/me",
        None,
        Some(&admin_cookie),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "the admin's session is untouched");
    assert!(
        audit_rows(&app.pool, "disable-account").await.is_empty(),
        "a refused self-disable writes no audit row"
    );
}

#[tokio::test]
async fn a_disabled_account_past_its_independent_deletion_grace_window_is_neither_resurrected_nor_hard_deleted()
 {
    // Disabling does not hard-delete data: an account that is BOTH
    // admin-disabled AND separately
    // self-deleted must not have its disable silently undone
    // once the deletion grace window elapses — neither by the finalize-on-
    // access path a fresh sign-in attempt would otherwise trigger, nor by
    // the background sweep. The disabled account must keep refusing sign-in
    // as `/errors/account-disabled`, never resurrect as a brand-new account
    // via the same email.
    let app = admin_app().await;
    let (admin_cookie, _) = sign_in(&app, ADMIN_EMAIL).await;
    let (_victim_cookie, victim_id) = sign_in(&app, "grace-and-disabled@example.com").await;
    let victim_uuid = uuid::Uuid::parse_str(&victim_id).expect("victim uuid");

    let (status, _) = send(
        app.router(),
        "POST",
        &format!("/api/admin/accounts/{victim_uuid}/disable"),
        None,
        Some(&admin_cookie),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT, "disable succeeds");

    // Independently self-delete the same account, backdated past its grace
    // window (bypassing HTTP — `soft_delete` takes an explicit `now_millis`,
    // exactly like the adapter-level regression test for this fix).
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_millis() as u64;
    app.state
        .accounts
        .soft_delete(victim_uuid, now - DELETION_GRACE_MILLIS - 1)
        .await
        .expect("backdated soft delete");

    // A fresh sign-in attempt hits the finalize-on-access branch (the account
    // is `Finalizable`) but must still be refused as disabled — not silently
    // resurrected as a fresh account under the same email.
    let (status, _) = send(
        app.router(),
        "POST",
        "/api/magic-links",
        Some(json!({ "email": "grace-and-disabled@example.com" })),
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
    let (status, problem) = send(
        app.router(),
        "POST",
        "/api/sessions",
        Some(json!({ "token": token })),
        None,
    )
    .await;
    assert_eq!(
        status,
        StatusCode::FORBIDDEN,
        "still refused as disabled, not resurrected as a fresh account"
    );
    assert_eq!(problem["type"], "/errors/account-disabled");

    // The original account row survives, still disabled — finalize-on-access
    // did not hard-delete it out from under the admin's disable.
    let survivor = app
        .state
        .accounts
        .find_by_id(victim_uuid)
        .await
        .expect("query")
        .expect("the disabled account survives finalize-on-access");
    assert!(survivor.disabled_at_millis.is_some());
}

#[tokio::test]
async fn a_refused_sign_in_on_a_disabled_account_does_not_clear_its_pending_self_deletion() {
    // `VerifyAndAttach`'s UPDATE unconditionally clears
    // `deleted_at` (the undelete) as a side effect. A disabled account
    // that is ALSO mid-way through its OWN self-deletion grace window (not
    // yet past it — `Finalizable` doesn't apply here) must not have that
    // grace window silently cancelled by someone merely ATTEMPTING to sign
    // in, even though the attempt is correctly refused overall.
    let app = admin_app().await;
    let (admin_cookie, _) = sign_in(&app, ADMIN_EMAIL).await;
    let (_victim_cookie, victim_id) = sign_in(&app, "mid-grace-disabled@example.com").await;
    let victim_uuid = uuid::Uuid::parse_str(&victim_id).expect("victim uuid");

    let (status, _) = send(
        app.router(),
        "POST",
        &format!("/api/admin/accounts/{victim_uuid}/disable"),
        None,
        Some(&admin_cookie),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT, "disable succeeds");

    // Self-delete, well within the grace window (NOT past it — no finalize
    // path is involved here at all).
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_millis() as u64;
    app.state
        .accounts
        .soft_delete(victim_uuid, now)
        .await
        .expect("soft delete mid-grace");

    // A sign-in attempt is refused as disabled.
    let (status, _) = send(
        app.router(),
        "POST",
        "/api/magic-links",
        Some(json!({ "email": "mid-grace-disabled@example.com" })),
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
    let (status, problem) = send(
        app.router(),
        "POST",
        "/api/sessions",
        Some(json!({ "token": token })),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(problem["type"], "/errors/account-disabled");

    // The refused attempt must NOT have cleared `deleted_at` — the account's
    // own pending self-deletion is untouched by someone else's refused login.
    let after = app
        .state
        .accounts
        .find_by_id(victim_uuid)
        .await
        .expect("query")
        .expect("account still exists");
    assert!(
        after.deleted_at_millis.is_some(),
        "a refused sign-in on a disabled account must not clear its own deleted_at"
    );
    assert!(after.disabled_at_millis.is_some());
}

// ---- Audit-log investigation filters ------------------------------------------

#[tokio::test]
async fn a_malformed_filter_is_refused_rather_than_silently_widening_the_read() {
    // Ignoring an unusable filter would return the WHOLE log while the caller
    // believes a restriction applied — the most dangerous default on this
    // surface.
    let app = admin_app().await;
    let (admin_cookie, _) = sign_in(&app, ADMIN_EMAIL).await;

    for uri in [
        "/api/admin/audit-log?actor=not-a-uuid",
        "/api/admin/audit-log?object=not-a-uuid",
        "/api/admin/audit-log?action=not-a-real-verb",
    ] {
        let (status, _) = send(app.router(), "GET", uri, None, Some(&admin_cookie)).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{uri} is refused");
    }
}

#[tokio::test]
async fn the_hand_written_query_validations_keep_their_exact_slug_and_detail() {
    // The COUNTER-DIRECTION guard. `PageQuery::cursor`,
    // `AuditQuery::uuid` and `AuditQuery::action` already map to
    // `ApiError::Validation` by hand, so the extractor must not swallow
    // them into one generic rejection detail.
    //
    // This is the one place the house rule against asserting prose is
    // deliberately inverted: the `detail` string IS the shipped contract for
    // these three paths, and losing it is exactly the over-correction this test
    // exists to catch.
    let app = admin_app().await;
    let (admin_cookie, _) = sign_in(&app, ADMIN_EMAIL).await;

    for (uri, detail) in [
        (
            "/api/admin/abuse-reports?cursor=nonsense",
            "page cursor is not one this server issued",
        ),
        (
            "/api/admin/audit-log?actor=not-a-uuid",
            "actor filter is not a valid id",
        ),
        (
            "/api/admin/audit-log?object=not-a-uuid",
            "object filter is not a valid id",
        ),
        (
            "/api/admin/audit-log?action=not-a-real-verb",
            "unknown audit action",
        ),
    ] {
        let (status, content_type, raw) =
            send_untyped(app.router(), uri, Some(&admin_cookie)).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{uri} is a 400");
        assert_eq!(content_type, "application/problem+json", "{uri} media type");
        let problem: Value = serde_json::from_str(&raw).expect("problem body is JSON");
        assert_eq!(problem["type"], "/errors/validation", "{uri} slug");
        assert_eq!(problem["detail"], detail, "{uri} detail is unchanged");
    }
}

#[tokio::test]
async fn a_malformed_query_string_answers_in_problem_json_on_both_paged_admin_reads() {
    // A value that cannot be deserialized into the query type
    // must reach the SAME contract as every other failure. Before this it was
    // axum's own `text/plain` rejection, so a client paging the admin surface
    // had to parse two error formats.
    let app = admin_app().await;
    let (admin_cookie, _) = sign_in(&app, ADMIN_EMAIL).await;

    for uri in [
        "/api/admin/abuse-reports?limit=abc",
        "/api/admin/audit-log?limit=abc",
    ] {
        let (status, content_type, raw) =
            send_untyped(app.router(), uri, Some(&admin_cookie)).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{uri} is a 400");
        assert_eq!(
            content_type, "application/problem+json",
            "{uri} answers a query rejection in the problem+json contract"
        );
        let problem: Value = serde_json::from_str(&raw).expect("problem body is JSON");
        assert_eq!(
            problem["type"], "/errors/validation",
            "{uri} carries the stable validation slug"
        );
        assert_eq!(problem["status"], 400);
    }
}

#[tokio::test]
async fn a_retired_action_verb_is_still_an_accepted_filter() {
    // `lookup-account` rows exist in the historical log. If the filter rejected
    // the verb those rows would be unreachable through the only surface that
    // can read them.
    let app = admin_app().await;
    let (admin_cookie, _) = sign_in(&app, ADMIN_EMAIL).await;

    let (status, _) = send(
        app.router(),
        "GET",
        "/api/admin/audit-log?action=lookup-account",
        None,
        Some(&admin_cookie),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn filtering_by_actor_narrows_the_log_to_that_account() {
    let app = admin_app().await;
    let (admin_cookie, admin_id) = sign_in(&app, ADMIN_EMAIL).await;
    // A second account signs in, landing its own `signed-in` row.
    sign_in(&app, "bystander@example.com").await;

    let (status, body) = send(
        app.router(),
        "GET",
        &format!("/api/admin/audit-log?actor={admin_id}"),
        None,
        Some(&admin_cookie),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let items = body["items"].as_array().expect("items");
    assert!(!items.is_empty(), "the admin has audit rows");
    assert!(
        items.iter().all(|r| r["actorAccountId"] == json!(admin_id)),
        "every row belongs to the filtered actor"
    );
}

#[tokio::test]
async fn a_cursor_is_refused_when_replayed_under_different_filters() {
    // A page-2 cursor replayed against another filter set would silently
    // re-anchor and return a partial slice with no signal — indistinguishable
    // from "this is everything".
    let app = admin_app().await;
    let (admin_cookie, admin_id) = sign_in(&app, ADMIN_EMAIL).await;
    // Generate several rows so a first page yields a cursor.
    for _ in 0..4 {
        send(
            app.router(),
            "GET",
            "/api/admin/abuse-reports",
            None,
            Some(&admin_cookie),
        )
        .await;
    }

    let (status, body) = send(
        app.router(),
        "GET",
        &format!("/api/admin/audit-log?limit=2&actor={admin_id}"),
        None,
        Some(&admin_cookie),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let cursor = body["nextCursor"].as_str().expect("a further page exists");

    // Same cursor, filters dropped → refused.
    let (status, _) = send(
        app.router(),
        "GET",
        &format!("/api/admin/audit-log?limit=2&cursor={cursor}"),
        None,
        Some(&admin_cookie),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    // Same cursor, same filters → still works.
    let (status, _) = send(
        app.router(),
        "GET",
        &format!("/api/admin/audit-log?limit=2&actor={admin_id}&cursor={cursor}"),
        None,
        Some(&admin_cookie),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
}

// ---- SearchObjects: turning a name into an id ----------------------------------

#[tokio::test]
async fn search_finds_an_account_by_a_callsign_prefix() {
    // The point of the whole feature: an admin who half-remembers a callsign
    // must still reach the id every action endpoint requires.
    let app = admin_app().await;
    let (admin_cookie, _) = sign_in(&app, ADMIN_EMAIL).await;
    let (_, target_id) = sign_in_with_callsign(&app, "target@example.com", "W1ABC").await;

    for q in ["W1A", "w1a", "W1ABC"] {
        let (status, body) = send(
            app.router(),
            "GET",
            &format!("/api/admin/search?q={q}"),
            None,
            Some(&admin_cookie),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "prefix {q}");
        let items = body["items"].as_array().expect("items");
        assert!(
            items.iter().any(|i| i["id"] == json!(target_id)),
            "prefix {q} reaches the account"
        );
        assert_eq!(items[0]["objectType"], json!("account"));
    }
}

#[tokio::test]
async fn search_finds_an_account_by_its_exact_email() {
    // The other half of the fence: an address must WORK when given in full,
    // otherwise "exact-match only" just means "email search is broken".
    let app = admin_app().await;
    let (admin_cookie, _) = sign_in(&app, ADMIN_EMAIL).await;
    let (_, target_id) = sign_in_with_callsign(&app, "target@example.com", "W1ABC").await;

    for q in [
        "target%40example.com",
        "Target%40Example.COM",
        "%20target%40example.com%20",
    ] {
        let (status, body) = send(
            app.router(),
            "GET",
            &format!("/api/admin/search?q={q}"),
            None,
            Some(&admin_cookie),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "exact email {q}");
        let items = body["items"].as_array().expect("items");
        assert_eq!(items.len(), 1, "exact email {q} finds the account");
        assert_eq!(items[0]["id"], json!(target_id));
    }
}

#[tokio::test]
async fn search_does_not_match_an_email_by_prefix() {
    // THE FENCE. Callsigns and display names are public radio data and may be
    // prefix-matched; an address must not be, or this becomes a harvesting
    // surface rather than a targeting one.
    let app = admin_app().await;
    let (admin_cookie, _) = sign_in(&app, ADMIN_EMAIL).await;
    sign_in_with_callsign(&app, "target@example.com", "W1ABC").await;

    for q in ["target", "target@", "tar"] {
        let (status, body) = send(
            app.router(),
            "GET",
            &format!("/api/admin/search?q={q}"),
            None,
            Some(&admin_cookie),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(
            body["items"].as_array().expect("items").len(),
            0,
            "a partial email finds nothing: {q}"
        );
    }
}

#[tokio::test]
async fn search_resolves_a_bare_id_back_to_its_object() {
    // The reverse direction: a uuid copied out of the audit log must resolve to
    // a named object, or the log stays a wall of opaque ids.
    let app = admin_app().await;
    let (admin_cookie, _) = sign_in(&app, ADMIN_EMAIL).await;
    let (_, target_id) = sign_in_with_callsign(&app, "target@example.com", "W1ABC").await;

    let (status, body) = send(
        app.router(),
        "GET",
        &format!("/api/admin/search?q={target_id}"),
        None,
        Some(&admin_cookie),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let items = body["items"].as_array().expect("items");
    assert_eq!(items.len(), 1);
    assert_eq!(items[0]["objectType"], json!("account"));
    assert_eq!(items[0]["label"], json!("W1ABC"));
}

#[tokio::test]
async fn a_blank_or_oversized_search_term_is_refused() {
    // A blank term must never degenerate into "list every object".
    let app = admin_app().await;
    let (admin_cookie, _) = sign_in(&app, ADMIN_EMAIL).await;
    let too_long = "a".repeat(200);

    for q in ["", "%20%20", too_long.as_str()] {
        let (status, _) = send(
            app.router(),
            "GET",
            &format!("/api/admin/search?q={q}"),
            None,
            Some(&admin_cookie),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "refused: {q:?}");
    }
}

#[tokio::test]
async fn a_search_lands_an_audit_row_carrying_no_term_and_no_match() {
    // The term may itself BE an email, and so may a match — only the shape of
    // the outcome is recorded.
    let app = admin_app().await;
    let (admin_cookie, admin_id) = sign_in(&app, ADMIN_EMAIL).await;
    sign_in_with_callsign(&app, "target@example.com", "W1ABC").await;

    let (status, _) = send(
        app.router(),
        "GET",
        "/api/admin/search?q=W1ABC",
        None,
        Some(&admin_cookie),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let (actor, target, _, metadata) = audit_row_full(&app.pool, "search-objects").await;
    assert_eq!(actor, uuid::Uuid::parse_str(&admin_id).expect("uuid"));
    assert_eq!(target, None);
    assert_eq!(metadata, Some(json!({ "shownCount": 1 })));

    let blob = metadata.expect("metadata").to_string();
    assert!(!blob.contains('@'), "no email or term in the row: {blob}");
    assert!(!blob.contains("W1ABC"), "no search term in the row: {blob}");
}

#[tokio::test]
async fn search_reports_an_accounts_disabled_state() {
    // The dashboard picks Disable vs Re-enable from this field.
    let app = admin_app().await;
    let (admin_cookie, _) = sign_in(&app, ADMIN_EMAIL).await;
    let (_, target_id) = sign_in_with_callsign(&app, "target@example.com", "W1ABC").await;

    send(
        app.router(),
        "POST",
        &format!("/api/admin/accounts/{target_id}/disable"),
        None,
        Some(&admin_cookie),
    )
    .await;

    let (status, body) = send(
        app.router(),
        "GET",
        "/api/admin/search?q=W1ABC",
        None,
        Some(&admin_cookie),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(body["items"][0]["disabledAt"].is_string());
}

// ---------------------------------------------------------------------------
// A search result says whether it is complete.
//
// The per-type cap is a bound, and stays one; what changes is that a caller
// can now tell twenty-of-twenty from twenty-of-two-hundred. Every seed is
// anchored on `PER_TYPE_SEARCH_LIMIT` (imported, never a literal) and every
// assertion is on the SIGNAL, because the constant will move.
// ---------------------------------------------------------------------------

/// The object types the response names as truncated, in response order.
fn truncated_types(body: &Value) -> Vec<String> {
    body["truncatedTypes"]
        .as_array()
        .expect("truncatedTypes is an array")
        .iter()
        .map(|t| t.as_str().expect("a type name").to_owned())
        .collect()
}

#[tokio::test]
async fn a_type_matching_more_than_the_cap_is_named_truncated_and_still_serves_the_cap() {
    // Without the signal, an admin reading twenty rows cannot know whether
    // the twenty-first exists — "no such account" and "not in the first twenty"
    // look identical.
    let app = admin_app().await;
    let (admin_cookie, _) = sign_in(&app, ADMIN_EMAIL).await;
    seed_accounts_with_callsign_prefix(&app.pool, "W1ZZ", PER_TYPE_SEARCH_LIMIT + 1).await;

    let (status, body) = send(
        app.router(),
        "GET",
        "/api/admin/search?q=W1ZZ",
        None,
        Some(&admin_cookie),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        truncated_types(&body),
        vec!["account".to_owned()],
        "the cut type is named: {body}"
    );
    assert_eq!(
        hit_ids_of_type(&body, "account").len(),
        PER_TYPE_SEARCH_LIMIT,
        "the cap itself does not move"
    );
}

#[tokio::test]
async fn a_type_matching_exactly_the_cap_is_reported_complete() {
    // The trap. A result of exactly the cap is COMPLETE; an implementation
    // that tests `rows.len() == limit` would call it truncated and send the
    // admin looking for a twenty-first row that does not exist.
    let app = admin_app().await;
    let (admin_cookie, _) = sign_in(&app, ADMIN_EMAIL).await;
    seed_accounts_with_callsign_prefix(&app.pool, "W1ZZ", PER_TYPE_SEARCH_LIMIT).await;

    let (status, body) = send(
        app.router(),
        "GET",
        "/api/admin/search?q=W1ZZ",
        None,
        Some(&admin_cookie),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        truncated_types(&body),
        Vec::<String>::new(),
        "exactly the cap is complete, not cut: {body}"
    );
    assert_eq!(
        hit_ids_of_type(&body, "account").len(),
        PER_TYPE_SEARCH_LIMIT,
        "every seeded row is served"
    );
}

#[tokio::test]
async fn two_types_truncate_independently_and_each_is_named() {
    // The per-type budget is what stops a noisy type reading as "no such
    // account"; a future shared budget would red here rather than in
    // production. Every net gets a live session, so the same title over-fills
    // BOTH title searches at once.
    let mut app = admin_app().await;
    let nets = PER_TYPE_SEARCH_LIMIT + 1;
    // Two resource bounds sit between this fixture and the search cap, and
    // neither is a search fact: the default per-user net cap answers 409 well
    // below it (raised here), and the create route admits `NET_CREATION_BURST`
    // writes per account per window (so the nets are spread over as many
    // owners as the count needs).
    let owners_cap = app.state.max_owners_per_net;
    app.state = app.state.with_resource_caps(nets, owners_cap);
    let (admin_cookie, _) = sign_in(&app, ADMIN_EMAIL).await;
    let owners_needed = nets.div_ceil(NET_CREATION_BURST as usize);
    let mut owner_cookies: Vec<String> = Vec::with_capacity(owners_needed);
    for o in 0..owners_needed {
        let (cookie, _) =
            sign_in_with_callsign(&app, &format!("owner{o}@example.com"), &format!("W{o}ZEB"))
                .await;
        owner_cookies.push(cookie);
    }
    for n in 0..nets {
        let owner = &owner_cookies[n % owners_needed];
        net_with_live_session(&app, owner, &format!("Zebra Net {n}")).await;
    }

    let (status, body) = send(
        app.router(),
        "GET",
        "/api/admin/search?q=Zebra",
        None,
        Some(&admin_cookie),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        truncated_types(&body),
        vec!["net-definition".to_owned(), "net-session".to_owned()],
        "both cut types are named, and the untouched account type is not: {body}"
    );
    assert_eq!(
        hit_ids_of_type(&body, "net-definition").len(),
        PER_TYPE_SEARCH_LIMIT,
        "nets keep their own budget"
    );
    assert_eq!(
        hit_ids_of_type(&body, "net-session").len(),
        PER_TYPE_SEARCH_LIMIT,
        "sessions keep their own budget"
    );

    // Review fence: a type that was NOT searched never appears — complete or
    // truncated. Sessions are still over-full, but narrowing to nets must not
    // report them.
    let (status, body) = send(
        app.router(),
        "GET",
        "/api/admin/search?q=Zebra&type=net-definition",
        None,
        Some(&admin_cookie),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        truncated_types(&body),
        vec!["net-definition".to_owned()],
        "only the searched type is named: {body}"
    );
    assert!(
        hit_ids_of_type(&body, "net-session").is_empty(),
        "narrowing excludes the unsearched type from items too"
    );
}

#[tokio::test]
async fn an_id_or_exact_email_search_never_claims_truncation() {
    // Both paths answer at most one row and are structurally incapable of
    // truncation. Pinned so neither arm can ever grow a push onto the type
    // list — the signal comes only from the three split reads.
    let app = admin_app().await;
    let (admin_cookie, _) = sign_in(&app, ADMIN_EMAIL).await;
    let (_, target_id) = sign_in_with_callsign(&app, "target@example.com", "W1ABC").await;

    for q in [target_id.as_str(), "target%40example.com"] {
        let (status, body) = send(
            app.router(),
            "GET",
            &format!("/api/admin/search?q={q}"),
            None,
            Some(&admin_cookie),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{q}");
        assert_eq!(body["items"].as_array().expect("items").len(), 1, "{q}");
        assert_eq!(
            truncated_types(&body),
            Vec::<String>::new(),
            "a single exact hit is complete: {q}"
        );
    }
}

// ---------------------------------------------------------------------------
// An unknown query key is refused on a FILTERED read.
//
// A read is "filtered" when a silently-dropped key changes
// the MEANING of the answer. All three admin reads here are on that list; the
// eight lenient call sites are asserted in `api_discovery.rs` and
// `api_net_session_ws.rs`.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn an_unrecognised_query_parameter_is_refused_on_every_filtered_admin_read() {
    // Three reads, four shapes of unrecognised key: a typo'd
    // filter, a cache-buster, a typo'd paging param, and a typo'd narrowing
    // param. Asserts status, media type and slug — never the prose.
    let app = admin_app().await;
    let (admin_cookie, _) = sign_in(&app, ADMIN_EMAIL).await;

    for uri in [
        "/api/admin/audit-log?actorr=00000000-0000-0000-0000-000000000001",
        "/api/admin/audit-log?_t=1724716800",
        "/api/admin/abuse-reports?limitt=5",
        "/api/admin/search?q=nick&typ=account",
    ] {
        let (status, content_type, raw) =
            send_untyped(app.router(), uri, Some(&admin_cookie)).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{uri} is a 400");
        assert_eq!(content_type, "application/problem+json", "{uri} media type");
        let problem: Value = serde_json::from_str(&raw).expect("problem body is JSON");
        assert_eq!(problem["type"], "/errors/validation", "{uri} slug");
        assert_eq!(problem["status"], 400, "{uri} status field");
    }
}

#[tokio::test]
async fn a_misspelled_actor_filter_no_longer_answers_as_though_unfiltered() {
    // The MEANING assertion, and the one that must fail for
    // the right reason. A test asserting only "400" would also pass if the
    // endpoint started refusing everything, so the correctly-spelled filter's
    // narrowing is asserted in the same test as the misspelled one's refusal.
    let app = admin_app().await;
    let (admin_cookie, admin_id) = sign_in(&app, ADMIN_EMAIL).await;
    // A second account signs in, landing its own `signed-in` row, so an
    // unfiltered log demonstrably spans more than one actor.
    sign_in(&app, "bystander-12-3@example.com").await;

    let (status, body) = send(
        app.router(),
        "GET",
        &format!("/api/admin/audit-log?actor={admin_id}"),
        None,
        Some(&admin_cookie),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "the spelled filter is still accepted"
    );
    let items = body["items"].as_array().expect("items");
    assert!(!items.is_empty(), "the admin has audit rows");
    assert!(
        items.iter().all(|r| r["actorAccountId"] == json!(admin_id)),
        "the spelled filter still narrows to that actor"
    );

    let (status, body) = send(
        app.router(),
        "GET",
        &format!("/api/admin/audit-log?actorr={admin_id}"),
        None,
        Some(&admin_cookie),
    )
    .await;
    // The `items` assertion comes FIRST on purpose: this read once answered 200
    // with the UNFILTERED log, and printing the body is how a RED run records
    // that wrong answer.
    assert!(
        body["items"].is_null(),
        "a refused read returns no page at all, not the unfiltered log: {body}"
    );
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "a dropped filter key is refused, not answered as though unfiltered"
    );
}

#[tokio::test]
async fn the_search_type_narrowing_param_still_binds_under_strictness() {
    // Trap 3. `SearchQuery`'s field is the raw identifier `r#type`,
    // so its serde name is `type`. A container-level `deny_unknown_fields` plus
    // a raw-identifier field is exactly the pairing that breaks silently: the
    // attribute would start refusing the very key the field declares.
    let app = admin_app().await;
    let (admin_cookie, _) = sign_in(&app, ADMIN_EMAIL).await;

    let (status, body) = send(
        app.router(),
        "GET",
        "/api/admin/search?q=nick&type=account",
        None,
        Some(&admin_cookie),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "`?type=` is a declared parameter");
    assert!(
        body["items"].is_array(),
        "the narrowed search still answers a page: {body}"
    );
}

// ---------------------------------------------------------------------------
// Both admin title searches match with `ILIKE`, so a `%`, `_` or
// `\` in the term must stay a LITERAL. Each fence seeds the row an UNESCAPED
// pattern would ALSO match (the near-miss), so it fails if the escaping is
// removed rather than staying green because nothing else could have matched.
// The session search reads the FROZEN snapshot title, so the fixture starts a
// session on every net — the snapshot copies the title at that moment.
// ---------------------------------------------------------------------------

/// A net titled `title` owned by `cookie`'s account, plus one live session on it.
/// Returns `(definition_id, session_id)`.
async fn net_with_live_session(app: &TestApp, cookie: &str, title: &str) -> (String, String) {
    let (status, created) = send(
        app.router(),
        "POST",
        "/api/net-definitions",
        Some(json!({
            "title": title,
            "connections": [
                { "kind": "hf", "plannedFrequencyHz": 14_230_000, "band": "20m", "mode": "ssb" }
            ],
            "netCategory": "traffic",
            "netType": "open"
        })),
        Some(cookie),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "create net {title:?}");
    let definition_id = created["id"].as_str().expect("id").to_owned();

    let (status, session) = send(
        app.router(),
        "POST",
        "/api/net-sessions",
        Some(json!({ "definitionId": definition_id })),
        Some(cookie),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "start session on {title:?}");
    let session_id = session["id"].as_str().expect("session id").to_owned();
    (definition_id, session_id)
}

/// The ids of every search hit of one `objectType`, in response order.
fn hit_ids_of_type(body: &Value, object_type: &str) -> Vec<String> {
    body["items"]
        .as_array()
        .expect("items")
        .iter()
        .filter(|item| item["objectType"] == json!(object_type))
        .map(|item| item["id"].as_str().expect("id").to_owned())
        .collect()
}

/// Runs one escaping fence over BOTH title searches: `encoded_term` is the
/// `?q=` value as it travels in the URL, `target` the title it must reach,
/// `near_miss` the title an unescaped pattern would also reach.
async fn assert_term_matches_only_the_literal(encoded_term: &str, target: &str, near_miss: &str) {
    let app = admin_app().await;
    let (admin_cookie, _) = sign_in(&app, ADMIN_EMAIL).await;
    let (owner_cookie, _) = sign_in_with_callsign(&app, "owner@example.com", "W1AW").await;
    let (target_definition, target_session) =
        net_with_live_session(&app, &owner_cookie, target).await;
    net_with_live_session(&app, &owner_cookie, near_miss).await;

    let (status, body) = send(
        app.router(),
        "GET",
        &format!("/api/admin/search?q={encoded_term}"),
        None,
        Some(&admin_cookie),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        hit_ids_of_type(&body, "net-definition"),
        vec![target_definition],
        "the net search for {encoded_term} reaches {target:?} and NOT {near_miss:?}"
    );
    assert_eq!(
        hit_ids_of_type(&body, "net-session"),
        vec![target_session],
        "the session search for {encoded_term} reaches {target:?} and NOT {near_miss:?}"
    );
}

#[tokio::test]
async fn a_percent_in_an_admin_search_matches_only_a_literal_percent_in_both_title_searches() {
    assert_term_matches_only_the_literal("100%25", "100% Rag", "1009 Rag").await;
}

#[tokio::test]
async fn an_underscore_in_an_admin_search_matches_only_a_literal_underscore_in_both_title_searches()
{
    assert_term_matches_only_the_literal("A_B", "A_B Net", "AxB Net").await;
}

#[tokio::test]
async fn a_backslash_in_an_admin_search_matches_only_a_literal_backslash_in_both_title_searches() {
    assert_term_matches_only_the_literal("A%5CB", "A\\B Net", "AB Net").await;
}
