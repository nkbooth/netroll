// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
//! Full-loop API tests for favorites / "My Nets": real router,
//! real Postgres (testcontainers), capturing fake mailer. Asserts status
//! codes, problem+json `type` slugs, body values, and DB side-effects — never
//! message prose (house TDD rule). The redaction and account-isolation
//! tests are the highest-value tests here.

use std::sync::{Arc, Mutex};

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use netroll_app::http::{AppState, api_router};
use netroll_domain::admin::MAX_PAGE_LIMIT;
use netroll_domain::consent::CURRENT_TERMS_VERSION;
use netroll_domain::net::connection::{NetConnection, NetConnectionKind, NetConnectionSet};
use netroll_domain::net::enums::{Band, Mode, Visibility};
use netroll_domain::net::validation::{
    NetDefinitionFields, RawNetDefinition, parse_net_definition_fields,
};
use netroll_domain::ports::{BoxFuture, MailError, Mailer};
use serde_json::{Value, json};
use sqlx::PgPool;
use sqlx::postgres::PgPoolOptions;
use testcontainers_modules::postgres::Postgres;
use testcontainers_modules::testcontainers::ContainerAsync;
use testcontainers_modules::testcontainers::runners::AsyncRunner;
use tower::ServiceExt;
use uuid::Uuid;

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
    pool: PgPool,
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
    let state = AppState::new(pool.clone(), mailer.clone(), "http://localhost:5173".into());
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
    let (status, _headers, value) = send_capture(router, method, uri, body, cookie).await;
    (status, value)
}

/// Like [`send_json`] but also returns the response headers (for `Retry-After`).
async fn send_capture(
    router: Router,
    method: &str,
    uri: &str,
    body: Option<Value>,
    cookie: Option<&str>,
) -> (StatusCode, axum::http::HeaderMap, Value) {
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
    let headers = response.headers().clone();
    let bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024)
        .await
        .expect("body");
    let json = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).expect("body is JSON")
    };
    (status, headers, json)
}

/// Signs in a fresh email and records consent (favoriting is gated on
/// `ConsentedAccount` — consent but NOT a callsign). Returns the cookie.
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

/// The one connection every fixture net is born with: a
/// definition carries no connection fact of its own, so `create` takes the set
/// as an argument.
fn sample_connections() -> NetConnectionSet {
    NetConnectionSet::new(vec![NetConnection {
        id: Uuid::now_v7(),
        position: 0,
        kind: NetConnectionKind::Hf {
            planned_frequency_hz: 14_230_000,
            band: Band::TwentyMeters,
            mode: Mode::Ssb,
        },
    }])
    .expect("one connection is a valid set")
}

fn sample_fields() -> NetDefinitionFields {
    parse_net_definition_fields(RawNetDefinition {
        title: Some("Sunday Traffic Net".to_owned()),
        description: Some("Weekly NTS".to_owned()),
        country: Some("USA".to_owned()),
        state: Some("CT".to_owned()),
        grid: Some("fn31pr".to_owned()),
        net_category: Some("traffic".to_owned()),
        net_type: Some("open".to_owned()),
        expected_duration: Some("90".to_owned()),
        visibility: None,
    })
    .expect("valid sample fields")
}

/// Seeds an account directly (bypassing the sign-in flow), returning its id.
async fn seed_account(app: &TestApp, email: &str) -> Uuid {
    app.state
        .accounts
        .create_verified_and_attach(email, now_millis())
        .await
        .expect("seed account")
        .id
}

/// Seeds a net owned by `owner`, returning its id.
async fn seed_net(app: &TestApp, owner: Uuid, token: &str, visibility: Visibility) -> Uuid {
    let mut fields = sample_fields();
    fields.visibility = visibility;
    app.state
        .net_definitions
        .create(&fields, &sample_connections(), owner, token, now_millis())
        .await
        .expect("seed net")
        .id
}

fn now_millis() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock")
        .as_millis() as u64
}

async fn favorite_count(app: &TestApp, account_id: Uuid) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM net_favorites WHERE account_id = $1")
        .bind(account_id)
        .fetch_one(&app.pool)
        .await
        .expect("count favorites")
}

#[tokio::test]
async fn favorite_happy_path_returns_204_and_writes_the_row() {
    let app = test_app().await;
    let cookie = sign_in_and_consent(&app, "fav@example.com").await;
    let owner = seed_account(&app, "owner@example.com").await;
    let net = seed_net(&app, owner, "tok-happy", Visibility::Listed).await;
    let me = current_account_id(&app, &cookie).await;

    let (status, _) = send_json(
        app.router(),
        "PUT",
        &format!("/api/favorites/{net}"),
        None,
        Some(&cookie),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert_eq!(
        favorite_count(&app, me).await,
        1,
        "the favorite row is written"
    );
}

#[tokio::test]
async fn re_favorite_is_idempotent_still_204_still_one_row() {
    let app = test_app().await;
    let cookie = sign_in_and_consent(&app, "fav@example.com").await;
    let owner = seed_account(&app, "owner@example.com").await;
    let net = seed_net(&app, owner, "tok-idem", Visibility::Listed).await;
    let me = current_account_id(&app, &cookie).await;

    for _ in 0..2 {
        let (status, _) = send_json(
            app.router(),
            "PUT",
            &format!("/api/favorites/{net}"),
            None,
            Some(&cookie),
        )
        .await;
        assert_eq!(status, StatusCode::NO_CONTENT);
    }
    assert_eq!(
        favorite_count(&app, me).await,
        1,
        "re-favoriting never duplicates"
    );
}

#[tokio::test]
async fn unfavorite_removes_the_row_and_absent_unfavorite_is_still_204() {
    let app = test_app().await;
    let cookie = sign_in_and_consent(&app, "fav@example.com").await;
    let owner = seed_account(&app, "owner@example.com").await;
    let net = seed_net(&app, owner, "tok-rm", Visibility::Listed).await;
    let me = current_account_id(&app, &cookie).await;

    // Unfavorite before ever favoriting: idempotent success.
    let (status, _) = send_json(
        app.router(),
        "DELETE",
        &format!("/api/favorites/{net}"),
        None,
        Some(&cookie),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert_eq!(favorite_count(&app, me).await, 0);

    send_json(
        app.router(),
        "PUT",
        &format!("/api/favorites/{net}"),
        None,
        Some(&cookie),
    )
    .await;
    let (status, _) = send_json(
        app.router(),
        "DELETE",
        &format!("/api/favorites/{net}"),
        None,
        Some(&cookie),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert_eq!(
        favorite_count(&app, me).await,
        0,
        "unfavorite removes the row"
    );
}

#[tokio::test]
async fn favorite_without_a_cookie_is_401_and_writes_nothing() {
    let app = test_app().await;
    let owner = seed_account(&app, "owner@example.com").await;
    let net = seed_net(&app, owner, "tok-401", Visibility::Listed).await;

    let (status, body) = send_json(
        app.router(),
        "PUT",
        &format!("/api/favorites/{net}"),
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(body["type"], "/errors/unauthenticated");

    let total: i64 = sqlx::query_scalar("SELECT count(*) FROM net_favorites")
        .fetch_one(&app.pool)
        .await
        .expect("count");
    assert_eq!(total, 0, "an unauthenticated favorite writes no row");

    // The list endpoint is equally gated.
    let (status, _) = send_json(app.router(), "GET", "/api/favorites", None, None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn favorites_are_account_isolated() {
    let app = test_app().await;
    let cookie_a = sign_in_and_consent(&app, "alice@example.com").await;
    let cookie_b = sign_in_and_consent(&app, "bob@example.com").await;
    let owner = seed_account(&app, "owner@example.com").await;
    let net = seed_net(&app, owner, "tok-iso", Visibility::Listed).await;

    // Alice favorites the net.
    send_json(
        app.router(),
        "PUT",
        &format!("/api/favorites/{net}"),
        None,
        Some(&cookie_a),
    )
    .await;

    // Bob's list is empty — he cannot see Alice's favorite.
    let (status, body) =
        send_json(app.router(), "GET", "/api/favorites", None, Some(&cookie_b)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        body["items"].as_array().expect("array").len(),
        0,
        "Bob sees none of Alice's favorites"
    );

    // Alice's list has exactly the one.
    let (status, body) =
        send_json(app.router(), "GET", "/api/favorites", None, Some(&cookie_a)).await;
    assert_eq!(status, StatusCode::OK);
    let favorites = body["items"].as_array().expect("array");
    assert_eq!(favorites.len(), 1);
    assert_eq!(favorites[0]["id"], net.to_string());
}

#[tokio::test]
async fn list_surfaces_unlisted_and_archived_favorites_with_indicator_and_token() {
    let app = test_app().await;
    let cookie = sign_in_and_consent(&app, "fav@example.com").await;
    let owner = seed_account(&app, "owner@example.com").await;

    let listed = seed_net(&app, owner, "tok-listed", Visibility::Listed).await;
    let unlisted = seed_net(&app, owner, "tok-unlisted", Visibility::Unlisted).await;
    let archived = seed_net(&app, owner, "tok-archived", Visibility::Listed).await;
    app.state
        .net_definitions
        .archive(archived, now_millis())
        .await
        .expect("archive");

    for net in [listed, unlisted, archived] {
        send_json(
            app.router(),
            "PUT",
            &format!("/api/favorites/{net}"),
            None,
            Some(&cookie),
        )
        .await;
    }

    let (status, body) =
        send_json(app.router(), "GET", "/api/favorites", None, Some(&cookie)).await;
    assert_eq!(status, StatusCode::OK);
    let favorites = body["items"].as_array().expect("array");
    assert_eq!(
        favorites.len(),
        3,
        "Listed, Unlisted, AND archived favorites all appear"
    );

    let by_id = |id: Uuid| {
        favorites
            .iter()
            .find(|f| f["id"] == id.to_string())
            .expect("present")
    };

    // The Unlisted favorite is present and carries its link token (return-link).
    let unlisted_body = by_id(unlisted);
    assert_eq!(unlisted_body["linkToken"], "tok-unlisted");
    assert!(
        unlisted_body["archivedAt"].is_null(),
        "the unlisted net is active"
    );

    // The archived favorite is present with archivedAt set (the indicator) —
    // never silently dropped.
    let archived_body = by_id(archived);
    assert!(
        archived_body["archivedAt"].is_string(),
        "archived favorite carries its archival instant"
    );

    // favoritedAt is present on every row.
    assert!(by_id(listed)["favoritedAt"].is_string());
}

#[tokio::test]
async fn a_favorited_net_whose_connection_rows_are_gone_is_skipped_not_fatal() {
    // A favoriter has no write access to the nets it favorites, so a stranger's
    // damaged net (zero `net_connections` rows — a hand-run DELETE, a partial
    // restore) must not 500 this account's favorites list, nor the personal-data
    // export that reuses the same read. The damaged net is skipped and logged;
    // the rest of the list answers.
    //
    // A WALK, with the damaged row seeded
    // INSIDE the first page (not at its end): the page comes back short by one,
    // its cursor still advances past the damaged row, and every healthy row is
    // reached exactly once. The page is split on the raw SQL rows BEFORE the
    // skip; splitting after it would drop the row the over-fetch probe had
    // pushed off the page (the failure mode).
    let app = test_app().await;
    let cookie = sign_in_and_consent(&app, "fav@example.com").await;
    let me = current_account_id(&app, &cookie).await;
    let owner = seed_account(&app, "owner@example.com").await;

    // Newest-first the read serves [4, 3(damaged), 2, 1, 0]; limit=3 puts the
    // damaged row second on page 1 — inside it.
    let seeded = seed_favorites_at(&app, me, owner, &[1_000, 2_000, 3_000, 4_000, 5_000]).await;
    let damaged = seeded[3].1;
    sqlx::query("DELETE FROM net_connections WHERE definition_id = $1")
        .bind(damaged)
        .execute(&app.pool)
        .await
        .expect("remove every connection row");

    let pages = walk_pages(&app, "/api/favorites", &cookie, 3).await;
    assert_eq!(pages.len(), 2, "two pages: {pages:?}");
    assert_eq!(
        pages[0].len(),
        2,
        "page 1 is short by exactly the damaged row it held"
    );
    assert_eq!(
        pages[1].len(),
        2,
        "page 2 is the remaining two healthy rows"
    );

    let seen: Vec<String> = pages
        .iter()
        .flatten()
        .map(|f| f["id"].as_str().expect("id").to_owned())
        .collect();
    let mut expected: Vec<(u64, Uuid)> = seeded
        .iter()
        .copied()
        .filter(|(_, id)| *id != damaged)
        .collect();
    expected.sort_by(|a, b| b.cmp(a));
    let expected: Vec<String> = expected.iter().map(|(_, id)| id.to_string()).collect();
    assert_eq!(
        seen, expected,
        "every healthy favorite exactly once, the damaged one absent, order kept"
    );
}

#[tokio::test]
async fn the_favorites_read_refuses_what_it_does_not_recognise() {
    // The read is STRICT on the forward rule. An
    // unknown key, a malformed limit, and a MALFORMED cursor (not `millis:uuid`)
    // are each `400 application/problem+json /errors/validation`; unauthenticated
    // is `401` before any parsing. A well-formed cursor is honoured whatever read
    // issued it — `PageQuery::cursor()` parses, it does not verify — so "a cursor
    // this server did not issue" is not a case this test can express. The
    // `detail` is deliberately NOT pinned — `MyNetsPage` renders
    // `messageForProblem`, not `detail`. A fresh account, and the limiter is
    // not in play: axum runs the strict extractor BEFORE the handler body, so
    // the two malformed-query requests are refused without ever reaching the
    // limiter, the unauthenticated one stops at `require_session`, and only the
    // garbage-cursor request spends a token (its cursor parses inside the body).
    let app = test_app().await;
    let cookie = sign_in_and_consent(&app, "strict@example.com").await;

    for uri in [
        "/api/favorites?limitt=5",
        "/api/favorites?limit=abc",
        "/api/favorites?cursor=garbage",
    ] {
        let (status, headers, problem) =
            send_capture(app.router(), "GET", uri, None, Some(&cookie)).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{uri} is a 400: {problem}");
        assert_eq!(
            headers
                .get(header::CONTENT_TYPE)
                .and_then(|v| v.to_str().ok()),
            Some("application/problem+json"),
            "{uri} media type"
        );
        assert_eq!(problem["type"], "/errors/validation", "{uri} slug");
        assert_eq!(problem["status"], 400, "{uri} status field");
    }

    let (status, problem) =
        send_json(app.router(), "GET", "/api/favorites?limitt=5", None, None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "401 before any parsing");
    assert_eq!(problem["type"], "/errors/unauthenticated");
}

#[tokio::test]
async fn list_never_leaks_owner_account_ids() {
    // Highest-value security test: a co-owned favorited net's owner ids must be
    // absent from the raw response. The projection has no owner column.
    let app = test_app().await;
    let cookie = sign_in_and_consent(&app, "fav@example.com").await;
    let owner_a = seed_account(&app, "ownera@example.com").await;
    let owner_b = seed_account(&app, "ownerb@example.com").await;
    let net = seed_net(&app, owner_a, "tok-coowned", Visibility::Listed).await;
    app.state
        .net_definitions
        .add_owner(net, owner_b, now_millis(), usize::MAX)
        .await
        .expect("add co-owner");

    send_json(
        app.router(),
        "PUT",
        &format!("/api/favorites/{net}"),
        None,
        Some(&cookie),
    )
    .await;

    let (status, body) =
        send_json(app.router(), "GET", "/api/favorites", None, Some(&cookie)).await;
    assert_eq!(status, StatusCode::OK);
    let raw = serde_json::to_string(&body).expect("serialize");
    assert!(
        !raw.contains(&owner_a.to_string()),
        "owner A id must not appear in the response"
    );
    assert!(
        !raw.contains(&owner_b.to_string()),
        "owner B id must not appear in the response"
    );
    assert!(
        !raw.contains("ownerAccountIds"),
        "the projection omits ownerAccountIds entirely"
    );
    // But the net itself IS in the list (positive control).
    assert_eq!(
        body["items"].as_array().expect("array")[0]["id"],
        net.to_string()
    );
}

#[tokio::test]
async fn favoriting_a_nonexistent_net_is_404_and_writes_no_orphan_row() {
    let app = test_app().await;
    let cookie = sign_in_and_consent(&app, "fav@example.com").await;
    let ghost = Uuid::now_v7();

    let (status, body) = send_json(
        app.router(),
        "PUT",
        &format!("/api/favorites/{ghost}"),
        None,
        Some(&cookie),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body["type"], "/errors/net-definition-not-found");

    let total: i64 = sqlx::query_scalar("SELECT count(*) FROM net_favorites")
        .fetch_one(&app.pool)
        .await
        .expect("count");
    assert_eq!(total, 0, "no orphan favorite row for a nonexistent net");
}

#[tokio::test]
async fn favorite_writes_are_rate_limited_per_account_with_429() {
    let app = test_app().await;
    let cookie = sign_in_and_consent(&app, "fav@example.com").await;
    let owner = seed_account(&app, "owner@example.com").await;

    // The per-account favorite burst is 20 (rate_limit::FAVORITE_BURST). Drive
    // 20 distinct favorites (each a real net so none is a 404), then the 21st
    // write must be refused with 429 + Retry-After.
    for i in 0..20 {
        let net = seed_net(&app, owner, &format!("tok-rl-{i}"), Visibility::Listed).await;
        let (status, _) = send_json(
            app.router(),
            "PUT",
            &format!("/api/favorites/{net}"),
            None,
            Some(&cookie),
        )
        .await;
        assert_eq!(
            status,
            StatusCode::NO_CONTENT,
            "favorite {i} within the burst"
        );
    }

    let net = seed_net(&app, owner, "tok-rl-over", Visibility::Listed).await;
    let (status, headers, body) = send_capture(
        app.router(),
        "PUT",
        &format!("/api/favorites/{net}"),
        None,
        Some(&cookie),
    )
    .await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(body["type"], "/errors/rate-limited");
    assert!(
        headers.contains_key(header::RETRY_AFTER),
        "429 carries a Retry-After header"
    );
}

#[tokio::test]
async fn favorites_reads_are_rate_limited_per_account_with_429() {
    let app = test_app().await;
    let cookie = sign_in_and_consent(&app, "reader@example.com").await;

    // The per-account favorites-READ burst is 30 (rate_limit::FAVORITES_READ_BURST),
    // a SEPARATE bucket from the favorite-WRITE limiter. The replenish period
    // (~2s) is real wall-clock time and each request is a DB-backed round trip,
    // so — like the IP-governor tests — assert "eventually 429" over a
    // loop comfortably past the burst rather than an exact Nth request: a slow
    // runner refilling one cell mid-loop would flake an exact-boundary assertion.
    let mut saw_429 = false;
    let mut saw_retry_after = false;
    let mut saw_slug = false;
    for _ in 0..70 {
        let (status, headers, body) =
            send_capture(app.router(), "GET", "/api/favorites", None, Some(&cookie)).await;
        if status == StatusCode::TOO_MANY_REQUESTS {
            saw_429 = true;
            saw_retry_after |= headers.contains_key(header::RETRY_AFTER);
            saw_slug |= body["type"] == "/errors/rate-limited";
        }
    }
    assert!(
        saw_429,
        "favorites reads from one account must eventually 429"
    );
    assert!(saw_retry_after, "a 429 carries a Retry-After header");
    assert!(saw_slug, "a 429 is the /errors/rate-limited problem");
}

#[tokio::test]
async fn favorites_read_limit_is_account_keyed_not_shared() {
    let app = test_app().await;
    let noisy = sign_in_and_consent(&app, "noisy@example.com").await;
    let quiet = sign_in_and_consent(&app, "quiet@example.com").await;

    // Exhaust the noisy account's read budget — loop comfortably past the
    // burst (not an exact Nth request) to dodge the same real-wall-clock
    // replenishment flakiness noted above.
    let mut noisy_saw_429 = false;
    for _ in 0..70 {
        let (status, _) =
            send_json(app.router(), "GET", "/api/favorites", None, Some(&noisy)).await;
        noisy_saw_429 |= status == StatusCode::TOO_MANY_REQUESTS;
    }
    assert!(
        noisy_saw_429,
        "the noisy account eventually exceeds its own read quota"
    );

    // A DIFFERENT account is unaffected — the limiter keys on account_id, not IP
    // (both accounts share the test's absent-IP path, so an IP key would collapse
    // them into one throttled bucket; account keying keeps them independent).
    let (quiet_status, _) =
        send_json(app.router(), "GET", "/api/favorites", None, Some(&quiet)).await;
    assert_eq!(
        quiet_status,
        StatusCode::OK,
        "a different account has its own read bucket"
    );
}

// ---- Paged read ---------------------------------------------------

/// Seeds `millis` favorites for `me` at `base + each`, returning the seeded
/// `(favorited_at_millis, id)` pairs. Seeded THROUGH the repo with explicit
/// millis so a shared instant is a fact of the fixture, not a
/// wall-clock accident.
async fn seed_favorites_at(
    app: &TestApp,
    me: Uuid,
    owner: Uuid,
    millis: &[u64],
) -> Vec<(u64, Uuid)> {
    let base = now_millis();
    let mut seeded = Vec::with_capacity(millis.len());
    for (i, offset) in millis.iter().enumerate() {
        let net = seed_net(app, owner, &format!("tok-walk-{i}"), Visibility::Listed).await;
        app.state
            .favorites
            .add(me, net, base + offset)
            .await
            .expect("seed favorite");
        seeded.push((base + offset, net));
    }
    seeded
}

/// Walks `uri` by echoing `nextCursor`, returning every page's `items`. Capped
/// so a cursor that stops advancing fails instead of hanging CI.
async fn walk_pages(app: &TestApp, uri: &str, cookie: &str, limit: usize) -> Vec<Vec<Value>> {
    let mut pages = Vec::new();
    let mut next = format!("{uri}?limit={limit}");
    for _ in 0..10 {
        let (status, body) = send_json(app.router(), "GET", &next, None, Some(cookie)).await;
        assert_eq!(status, StatusCode::OK, "got {body}");
        let items = body["items"].as_array().expect("items array").clone();
        assert!(
            items.len() <= limit,
            "a page never exceeds the requested limit: {} > {limit}",
            items.len()
        );
        pages.push(items);
        // `.get`, not `body["nextCursor"]`: serde_json's `Index` answers `Null`
        // for an ABSENT key, which would read a body that lost the key as "last
        // page, done" — the one contract break this fence exists to catch.
        match body.get("nextCursor") {
            None => panic!("nextCursor is absent from the page body: {body}"),
            Some(Value::Null) => return pages,
            Some(Value::String(cursor)) => {
                next = format!("{uri}?limit={limit}&cursor={cursor}");
            }
            Some(other) => panic!("nextCursor is a string or null, got {other}"),
        }
    }
    panic!("the cursor never reached null in ten pages — it is not advancing");
}

// ---- Membership read -------------------------------------------------------

/// `GET /api/favorites/membership?ids=…` for the given ids, returning the
/// status and body.
async fn membership(app: &TestApp, cookie: Option<&str>, ids: &[Uuid]) -> (StatusCode, Value) {
    let joined: Vec<String> = ids.iter().map(Uuid::to_string).collect();
    send_json(
        app.router(),
        "GET",
        &format!("/api/favorites/membership?ids={}", joined.join(",")),
        None,
        cookie,
    )
    .await
}

#[tokio::test]
async fn membership_answers_exactly_the_asked_ids_this_account_favorited() {
    // The batch membership read replaces the frontend's unbounded page walk
    // (the star on a discovery card / public net page). It answers the SUBSET of
    // the asked ids the calling account has favorited — never another account's
    // favorites, never an id that was not asked about, and an unknown or
    // unfavorited id simply does not appear.
    let app = test_app().await;
    let cookie = sign_in_and_consent(&app, "member@example.com").await;
    let me = current_account_id(&app, &cookie).await;
    let owner = seed_account(&app, "owner@example.com").await;
    let other = seed_account(&app, "other@example.com").await;

    let mine_a = seed_net(&app, owner, "tok-m-a", Visibility::Listed).await;
    let mine_b = seed_net(&app, owner, "tok-m-b", Visibility::Unlisted).await;
    let not_mine = seed_net(&app, owner, "tok-m-c", Visibility::Listed).await;
    let unasked = seed_net(&app, owner, "tok-m-d", Visibility::Listed).await;
    let now = now_millis();
    for net in [mine_a, mine_b, unasked] {
        app.state
            .favorites
            .add(me, net, now)
            .await
            .expect("favorite");
    }
    // Another account favorites the net I did NOT — account scoping is the
    // whole answer here, so it must be in the fixture.
    app.state
        .favorites
        .add(other, not_mine, now)
        .await
        .expect("other's favorite");

    let (status, body) = membership(
        &app,
        Some(&cookie),
        &[mine_a, not_mine, mine_b, Uuid::now_v7()],
    )
    .await;
    assert_eq!(status, StatusCode::OK, "got {body}");
    let favorited: std::collections::BTreeSet<Uuid> = body["favorited"]
        .as_array()
        .expect("favorited array")
        .iter()
        .map(|v| v.as_str().expect("uuid string").parse().expect("uuid"))
        .collect();
    assert_eq!(
        favorited,
        [mine_a, mine_b].into_iter().collect(),
        "exactly the asked ids this account favorited — not another account's, not an unasked one"
    );
    assert_eq!(
        body.as_object().expect("object").len(),
        1,
        "the body carries `favorited` and nothing else: {body}"
    );
}

#[tokio::test]
async fn membership_is_strict_and_refuses_more_ids_than_the_page_cap() {
    // STRICT on the forward rule, and the reason is sharper here than
    // on the paged reads: a dropped `ids` key would answer "none favorited" for
    // every net, with a 200. An unknown key, a missing `ids`, a malformed id and
    // MORE than `MAX_PAGE_LIMIT` ids are each `400 /errors/validation` — the cap
    // refuses rather than truncates, because truncating would answer "not
    // favorited" for every id past it. Exactly `MAX_PAGE_LIMIT` ids is accepted.
    let app = test_app().await;
    let cookie = sign_in_and_consent(&app, "strict-member@example.com").await;
    let ids: Vec<Uuid> = (0..MAX_PAGE_LIMIT).map(|_| Uuid::now_v7()).collect();
    let at_cap: Vec<String> = ids.iter().map(Uuid::to_string).collect();

    for uri in [
        format!("/api/favorites/membership?idss={}", at_cap[0]),
        "/api/favorites/membership".to_owned(),
        "/api/favorites/membership?ids=not-a-uuid".to_owned(),
        format!("/api/favorites/membership?ids={},abc", at_cap[0]),
    ] {
        let (status, headers, problem) =
            send_capture(app.router(), "GET", &uri, None, Some(&cookie)).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{uri} is a 400: {problem}");
        assert_eq!(
            headers
                .get(header::CONTENT_TYPE)
                .and_then(|v| v.to_str().ok()),
            Some("application/problem+json"),
            "{uri} media type"
        );
        assert_eq!(problem["type"], "/errors/validation", "{uri} slug");
    }

    let (status, body) = membership(&app, Some(&cookie), &ids).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "exactly MAX_PAGE_LIMIT ids is within the cap: {body}"
    );
    let mut over = ids.clone();
    over.push(Uuid::now_v7());
    let (status, problem) = membership(&app, Some(&cookie), &over).await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "one id past the cap is refused, not truncated: {problem}"
    );
    assert_eq!(problem["type"], "/errors/validation");
}

#[tokio::test]
async fn membership_requires_a_session() {
    let app = test_app().await;
    let (status, problem) = membership(&app, None, &[Uuid::now_v7()]).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(problem["type"], "/errors/unauthenticated");
}

#[tokio::test]
async fn membership_spends_the_same_read_budget_as_the_favorites_list() {
    // One bucket for both favorites reads: exhaust it through the list, and the
    // membership read is refused on the same account. Two immediate calls
    // rather than one: the bucket refills a token every ~2 s of wall clock, so
    // a single call landing exactly on a refill could read 200 for the wrong
    // reason; two back-to-back calls cannot both be fed by one refill.
    let app = test_app().await;
    let cookie = sign_in_and_consent(&app, "budget@example.com").await;
    let net = Uuid::now_v7();

    let mut list_saw_429 = false;
    for _ in 0..70 {
        let (status, _) =
            send_json(app.router(), "GET", "/api/favorites", None, Some(&cookie)).await;
        if status == StatusCode::TOO_MANY_REQUESTS {
            list_saw_429 = true;
            break;
        }
    }
    assert!(
        list_saw_429,
        "the list read exhausts the account's read budget"
    );

    let first = membership(&app, Some(&cookie), &[net]).await.0;
    let second = membership(&app, Some(&cookie), &[net]).await.0;
    assert!(
        first == StatusCode::TOO_MANY_REQUESTS || second == StatusCode::TOO_MANY_REQUESTS,
        "the membership read is metered by the bucket the list read just emptied — got {first} then {second}"
    );
}

#[tokio::test]
async fn the_favorites_read_pages_through_every_favorite_without_gap_or_repeat() {
    // Five favorites whose middle pair shares a favorited_at
    // millisecond, seeded so that with limit=2 the pair STRADDLES the first page
    // edge — exactly where a timestamp-only cursor skips one or serves one twice.
    let app = test_app().await;
    let cookie = sign_in_and_consent(&app, "walker@example.com").await;
    let me = current_account_id(&app, &cookie).await;
    let owner = seed_account(&app, "owner@example.com").await;
    let seeded = seed_favorites_at(&app, me, owner, &[1_000, 2_000, 3_000, 3_000, 4_000]).await;

    let pages = walk_pages(&app, "/api/favorites", &cookie, 2).await;

    // THE FIXTURE'S OWN PRECONDITION: the tie really does straddle the first edge.
    assert!(pages.len() >= 2, "more than one page: {pages:?}");
    assert_eq!(
        pages[0][1]["favoritedAt"], pages[1][0]["favoritedAt"],
        "page 1's last row and page 2's first row must share a favoritedAt, \
         or this test proves nothing about a shared-timestamp boundary"
    );

    // Totality and order, against the ids THIS test seeded — never a second read.
    let seen: Vec<String> = pages
        .iter()
        .flatten()
        .map(|f| f["id"].as_str().expect("id").to_owned())
        .collect();
    let mut expected = seeded.clone();
    // Newest-favorited first; within a shared millisecond, the higher id first
    // (`DESC, DESC` — the row-value cursor's total order).
    expected.sort_by(|a, b| b.cmp(a));
    let expected: Vec<String> = expected.iter().map(|(_, id)| id.to_string()).collect();
    assert_eq!(
        seen, expected,
        "every seeded favorite exactly once, newest-favorited first, across every boundary"
    );
    let unique: std::collections::HashSet<_> = seen.iter().collect();
    assert_eq!(unique.len(), seen.len(), "no favorite is served twice");
}

/// Reads the signed-in account's id via `GET /api/accounts/me`.
async fn current_account_id(app: &TestApp, cookie: &str) -> Uuid {
    let (status, body) =
        send_json(app.router(), "GET", "/api/accounts/me", None, Some(cookie)).await;
    assert_eq!(status, StatusCode::OK);
    body["id"].as_str().expect("id").parse().expect("uuid")
}
