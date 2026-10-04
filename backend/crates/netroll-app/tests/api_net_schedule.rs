// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
//! Full-loop API tests for the net schedule sub-resource, delete-as-archive,
//! and the public archival field: real router, real Postgres
//! (testcontainers), capturing mailer. Asserts status codes, problem+json type
//! slugs, occurrence instants, and DB side-effects — never message prose
//! (house TDD rule).

use std::sync::{Arc, Mutex};

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
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

async fn sign_in_only(app: &TestApp, email: &str) -> String {
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
    response
        .headers()
        .get(header::SET_COOKIE)
        .expect("cookie")
        .to_str()
        .expect("ascii")
        .split(';')
        .next()
        .expect("pair")
        .to_owned()
}

async fn sign_in_and_consent(app: &TestApp, email: &str) -> String {
    let cookie = sign_in_only(app, email).await;

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
    let cookie = sign_in_and_consent(app, email).await;
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

async fn create_net(app: &TestApp, cookie: &str) -> Value {
    let (status, body) = send_json(
        app.router(),
        "POST",
        "/api/net-definitions",
        Some(minimal_definition_json()),
        Some(cookie),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    body
}

async fn schedule_row_count(app: &TestApp, id: &str) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM net_schedules WHERE definition_id = $1::uuid")
        .bind(id)
        .fetch_one(&app.pool)
        .await
        .expect("count schedules")
}

async fn occurrence_row_count(app: &TestApp, id: &str) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM net_occurrences WHERE definition_id = $1::uuid")
        .bind(id)
        .fetch_one(&app.pool)
        .await
        .expect("count occurrences")
}

#[tokio::test]
async fn set_one_off_schedule_materializes_one_occurrence() {
    let app = test_app().await;
    let cookie = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;
    let created = create_net(&app, &cookie).await;
    let id = created["id"].as_str().expect("id").to_owned();

    let (status, body) = send_json(
        app.router(),
        "PUT",
        &format!("/api/net-definitions/{id}/schedule"),
        Some(json!({
            "kind": "one-off",
            "timezone": "America/New_York",
            "oneOffStartAt": "2027-01-01T20:00:00Z"
        })),
        Some(&cookie),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let occ = body["occurrences"].as_array().expect("occurrences array");
    assert_eq!(
        occ.len(),
        1,
        "a one-off materializes exactly one occurrence"
    );
    // Served as an RFC 3339 UTC instant (canonical +00:00 form).
    assert_eq!(occ[0]["scheduledStartAt"], "2027-01-01T20:00:00+00:00");

    // The GET endpoint agrees.
    let (status, body) = send_json(
        app.router(),
        "GET",
        &format!("/api/net-definitions/{id}/occurrences"),
        None,
        Some(&cookie),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["occurrences"].as_array().expect("array").len(), 1);
}

#[tokio::test]
async fn set_recurring_schedule_is_idempotent_on_repeat() {
    let app = test_app().await;
    let cookie = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;
    let created = create_net(&app, &cookie).await;
    let id = created["id"].as_str().expect("id").to_owned();

    let weekly = json!({
        "kind": "recurring",
        "timezone": "America/New_York",
        "frequency": "weekly",
        "timeOfDay": "20:00",
        "weekday": "tuesday"
    });
    let (status, body) = send_json(
        app.router(),
        "PUT",
        &format!("/api/net-definitions/{id}/schedule"),
        Some(weekly.clone()),
        Some(&cookie),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let occ = body["occurrences"].as_array().expect("array").clone();
    assert!(occ.len() >= 4, "a weekly rule fills the horizon");
    // Ascending order.
    let starts: Vec<&str> = occ
        .iter()
        .map(|o| o["scheduledStartAt"].as_str().expect("str"))
        .collect();
    let mut sorted = starts.clone();
    sorted.sort();
    assert_eq!(starts, sorted, "occurrences are ascending");

    let count_after_first = occurrence_row_count(&app, &id).await;
    // Re-PUT the same rule → the occurrence set is unchanged (idempotent).
    let (status, _) = send_json(
        app.router(),
        "PUT",
        &format!("/api/net-definitions/{id}/schedule"),
        Some(weekly),
        Some(&cookie),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        occurrence_row_count(&app, &id).await,
        count_after_first,
        "re-setting the same rule adds no duplicate occurrences"
    );
}

#[tokio::test]
async fn invalid_schedule_is_a_field_level_400_with_no_rows_written() {
    let app = test_app().await;
    let cookie = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;
    let created = create_net(&app, &cookie).await;
    let id = created["id"].as_str().expect("id").to_owned();

    let cases = [
        (
            json!({ "kind": "recurring", "timezone": "Mars/Base", "frequency": "weekly", "timeOfDay": "20:00", "weekday": "tuesday" }),
            "timezone:",
        ),
        (
            json!({ "kind": "recurring", "timezone": "UTC", "frequency": "weekly", "timeOfDay": "8:00", "weekday": "tuesday" }),
            "time of day:",
        ),
        (
            json!({ "kind": "recurring", "timezone": "UTC", "frequency": "weekly", "timeOfDay": "20:00" }),
            "weekday:",
        ),
    ];
    for (body, field_prefix) in cases {
        let (status, problem) = send_json(
            app.router(),
            "PUT",
            &format!("/api/net-definitions/{id}/schedule"),
            Some(body),
            Some(&cookie),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(problem["type"], "/errors/schedule-invalid");
        assert!(
            problem["detail"]
                .as_str()
                .expect("detail")
                .starts_with(field_prefix),
            "detail names the offending field ({field_prefix})"
        );
    }
    assert_eq!(
        schedule_row_count(&app, &id).await,
        0,
        "no schedule row written"
    );
    assert_eq!(
        occurrence_row_count(&app, &id).await,
        0,
        "no occurrence row written"
    );
}

#[tokio::test]
async fn schedule_authz_non_owner_403_and_missing_net_404() {
    let app = test_app().await;
    let owner = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;
    let created = create_net(&app, &owner).await;
    let id = created["id"].as_str().expect("id").to_owned();

    let weekly = json!({
        "kind": "recurring", "timezone": "UTC", "frequency": "weekly",
        "timeOfDay": "20:00", "weekday": "tuesday"
    });

    // A different consented+callsigned account is not an owner → 403.
    let intruder = sign_in_consent_callsign(&app, "intruder@example.com", "k5xyz").await;
    let (status, problem) = send_json(
        app.router(),
        "PUT",
        &format!("/api/net-definitions/{id}/schedule"),
        Some(weekly.clone()),
        Some(&intruder),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(problem["type"], "/errors/forbidden");

    // A nonexistent net → 404.
    let missing = uuid::Uuid::now_v7();
    let (status, problem) = send_json(
        app.router(),
        "PUT",
        &format!("/api/net-definitions/{missing}/schedule"),
        Some(weekly),
        Some(&owner),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(problem["type"], "/errors/net-definition-not-found");
}

#[tokio::test]
async fn clear_schedule_removes_future_occurrences() {
    let app = test_app().await;
    let cookie = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;
    let created = create_net(&app, &cookie).await;
    let id = created["id"].as_str().expect("id").to_owned();

    send_json(
        app.router(),
        "PUT",
        &format!("/api/net-definitions/{id}/schedule"),
        Some(json!({
            "kind": "recurring", "timezone": "UTC", "frequency": "daily", "timeOfDay": "12:00"
        })),
        Some(&cookie),
    )
    .await;
    assert!(occurrence_row_count(&app, &id).await > 0);

    let (status, _) = send_json(
        app.router(),
        "DELETE",
        &format!("/api/net-definitions/{id}/schedule"),
        None,
        Some(&cookie),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert_eq!(
        schedule_row_count(&app, &id).await,
        0,
        "schedule row cleared"
    );
    assert_eq!(
        occurrence_row_count(&app, &id).await,
        0,
        "future occurrences cleared"
    );
}

#[tokio::test]
async fn setting_a_schedule_does_not_bump_definition_version() {
    // A schedule is a sub-resource, not a definition field edit (Dev Notes).
    let app = test_app().await;
    let cookie = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;
    let created = create_net(&app, &cookie).await;
    let id = created["id"].as_str().expect("id").to_owned();
    let version_before = created["definitionVersion"].as_i64().expect("version");

    send_json(
        app.router(),
        "PUT",
        &format!("/api/net-definitions/{id}/schedule"),
        Some(json!({
            "kind": "recurring", "timezone": "UTC", "frequency": "daily", "timeOfDay": "12:00"
        })),
        Some(&cookie),
    )
    .await;

    let (_, body) = send_json(
        app.router(),
        "GET",
        &format!("/api/net-definitions/{id}"),
        None,
        Some(&cookie),
    )
    .await;
    assert_eq!(
        body["definitionVersion"].as_i64().expect("version"),
        version_before,
        "setting a schedule must not bump definition_version"
    );
}

#[tokio::test]
async fn public_permalink_reflects_archival_but_stays_resolvable() {
    // An archived net's permalink still resolves (200), and the
    // public body carries archivedAt so the client renders "archived" — never a
    // 404 that would break a shared link.
    let app = test_app().await;
    let cookie = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;
    let created = create_net(&app, &cookie).await;
    let id = created["id"].as_str().expect("id").to_owned();
    let token = created["linkToken"].as_str().expect("token").to_owned();

    // Active: archivedAt is null on the public body.
    let (status, body) = send_json(
        app.router(),
        "GET",
        &format!("/api/net-definitions/by-token/{token}"),
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        body["archivedAt"].is_null(),
        "active net has null archivedAt"
    );
    // The public body still omits owner identities and the token.
    assert!(body.get("ownerAccountIds").is_none());
    assert!(body.get("linkToken").is_none());

    // Archive via owner delete.
    let (status, _) = send_json(
        app.router(),
        "DELETE",
        &format!("/api/net-definitions/{id}"),
        None,
        Some(&cookie),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    // The permalink still resolves (200) and now exposes archivedAt.
    let (status, body) = send_json(
        app.router(),
        "GET",
        &format!("/api/net-definitions/by-token/{token}"),
        None,
        None,
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "the shared permalink still resolves"
    );
    assert!(
        body["archivedAt"].as_str().is_some(),
        "archivedAt is populated on an archived net's public body"
    );
}

#[tokio::test]
async fn archived_net_leaves_upcoming_discovery() {
    let app = test_app().await;
    let cookie = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;
    let created = create_net(&app, &cookie).await;
    let id = created["id"].as_str().expect("id").to_owned();

    send_json(
        app.router(),
        "PUT",
        &format!("/api/net-definitions/{id}/schedule"),
        Some(json!({
            "kind": "recurring", "timezone": "UTC", "frequency": "daily", "timeOfDay": "12:00"
        })),
        Some(&cookie),
    )
    .await;

    // Archive, then the discovery seam must exclude its occurrences.
    send_json(
        app.router(),
        "DELETE",
        &format!("/api/net-definitions/{id}"),
        None,
        Some(&cookie),
    )
    .await;

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_millis() as u64;
    let upcoming = app
        .state
        .schedules
        .list_upcoming_occurrences(now, 500)
        .await
        .expect("list upcoming");
    let target = uuid::Uuid::parse_str(&id).expect("uuid");
    assert!(
        !upcoming.iter().any(|o| o.definition_id == target),
        "an archived net's occurrences never appear in discovery"
    );
}

/// The stored rule's eight rule-bearing columns as one string, so a re-save can
/// be asserted byte-for-byte identical. `updated_at` is
/// deliberately excluded — the upsert always bumps it and it is not part of the
/// rule.
async fn stored_rule_fingerprint(app: &TestApp, id: &str) -> String {
    sqlx::query_scalar(
        "SELECT kind || '|' || timezone
                || '|' || coalesce(one_off_start_at::text, '')
                || '|' || coalesce(frequency, '')
                || '|' || coalesce(local_hour::text, '')
                || '|' || coalesce(local_minute::text, '')
                || '|' || coalesce(weekday::text, '')
                || '|' || coalesce(day_of_month::text, '')
         FROM net_schedules WHERE definition_id = $1::uuid",
    )
    .bind(id)
    .fetch_one(&app.pool)
    .await
    .expect("stored rule fingerprint")
}

/// Every occurrence row's `(id, scheduled_start_at)` in instant order — the
/// no-churn assertion for a no-op re-save (ids change iff rows were recreated).
async fn occurrence_fingerprint(app: &TestApp, id: &str) -> Vec<(String, String)> {
    sqlx::query_as(
        "SELECT id::text, scheduled_start_at::text FROM net_occurrences
         WHERE definition_id = $1::uuid ORDER BY scheduled_start_at",
    )
    .bind(id)
    .fetch_all(&app.pool)
    .await
    .expect("occurrence fingerprint")
}

#[tokio::test]
async fn get_schedule_round_trips_the_stored_recurring_rule() {
    // The read shape IS the write shape, so the panel can
    // hydrate from it and re-submit it unchanged as a no-op. Asserted as
    // round-trip equality on the typed wire values, not on prose.
    let app = test_app().await;
    let cookie = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;
    let created = create_net(&app, &cookie).await;
    let id = created["id"].as_str().expect("id").to_owned();

    let written = json!({
        "kind": "recurring",
        "timezone": "America/New_York",
        "frequency": "weekly",
        "timeOfDay": "21:30",
        "weekday": "friday"
    });
    let (status, _) = send_json(
        app.router(),
        "PUT",
        &format!("/api/net-definitions/{id}/schedule"),
        Some(written.clone()),
        Some(&cookie),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let (status, read) = send_json(
        app.router(),
        "GET",
        &format!("/api/net-definitions/{id}/schedule"),
        None,
        Some(&cookie),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        read, written,
        "the read rule equals the rule written, field for field"
    );

    // An owner-scoped rule read leaks neither the link token nor owner
    // identity (the same redaction posture).
    let object = read.as_object().expect("object body");
    assert!(!object.contains_key("linkToken"));
    assert!(!object.contains_key("ownerAccountIds"));

    // Submitting the read rule back unchanged is a no-op — the stored
    // rule and its future occurrences are byte-for-byte identical.
    let rule_before = stored_rule_fingerprint(&app, &id).await;
    let occurrences_before = occurrence_fingerprint(&app, &id).await;
    assert!(!occurrences_before.is_empty(), "a weekly rule materializes");

    let (status, _) = send_json(
        app.router(),
        "PUT",
        &format!("/api/net-definitions/{id}/schedule"),
        Some(read),
        Some(&cookie),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        stored_rule_fingerprint(&app, &id).await,
        rule_before,
        "re-saving the read rule does not change the stored rule"
    );
    assert_eq!(
        occurrence_fingerprint(&app, &id).await,
        occurrences_before,
        "re-saving the read rule churns no occurrence rows"
    );
}

#[tokio::test]
async fn get_schedule_round_trips_a_stored_one_off_rule() {
    // One-off arm. The instant is normalized to the canonical
    // RFC 3339 UTC form on the way out, so equality is asserted on that form
    // and the no-op invariant carries the round-trip proof.
    let app = test_app().await;
    let cookie = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;
    let created = create_net(&app, &cookie).await;
    let id = created["id"].as_str().expect("id").to_owned();

    let (status, _) = send_json(
        app.router(),
        "PUT",
        &format!("/api/net-definitions/{id}/schedule"),
        Some(json!({
            "kind": "one-off",
            "timezone": "America/New_York",
            "oneOffStartAt": "2027-01-01T20:00:00Z"
        })),
        Some(&cookie),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let (status, read) = send_json(
        app.router(),
        "GET",
        &format!("/api/net-definitions/{id}/schedule"),
        None,
        Some(&cookie),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        read,
        json!({
            "kind": "one-off",
            "timezone": "America/New_York",
            "oneOffStartAt": "2027-01-01T20:00:00+00:00"
        }),
        "a one-off reads back as kind + timezone + the canonical instant, with \
         no recurring fields"
    );

    let rule_before = stored_rule_fingerprint(&app, &id).await;
    let occurrences_before = occurrence_fingerprint(&app, &id).await;
    let (status, _) = send_json(
        app.router(),
        "PUT",
        &format!("/api/net-definitions/{id}/schedule"),
        Some(read),
        Some(&cookie),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(stored_rule_fingerprint(&app, &id).await, rule_before);
    assert_eq!(occurrence_fingerprint(&app, &id).await, occurrences_before);
}

#[tokio::test]
async fn get_schedule_is_204_when_the_net_has_no_schedule() {
    // 204 is the signal the panel uses to tell "unscheduled"
    // from "still loading". A JSON body is NOT always present on this route.
    let app = test_app().await;
    let cookie = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;
    let created = create_net(&app, &cookie).await;
    let id = created["id"].as_str().expect("id").to_owned();

    let (status, body) = send_json(
        app.router(),
        "GET",
        &format!("/api/net-definitions/{id}/schedule"),
        None,
        Some(&cookie),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert_eq!(body, Value::Null, "204 carries no body");
    assert_eq!(schedule_row_count(&app, &id).await, 0);
}

#[tokio::test]
async fn get_schedule_carries_the_same_consent_and_ownership_gate() {
    // The read seam must not become a new unauthenticated or
    // non-owner surface. Same four refusals `load_owned` + `ConsentedAccount`
    // already give the PUT/DELETE arms of this route.
    let app = test_app().await;
    let owner = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;
    let created = create_net(&app, &owner).await;
    let id = created["id"].as_str().expect("id").to_owned();
    send_json(
        app.router(),
        "PUT",
        &format!("/api/net-definitions/{id}/schedule"),
        Some(json!({
            "kind": "recurring", "timezone": "UTC", "frequency": "weekly",
            "timeOfDay": "21:30", "weekday": "friday"
        })),
        Some(&owner),
    )
    .await;

    // A consented, callsigned account that does not own the net → 403.
    let intruder = sign_in_consent_callsign(&app, "intruder@example.com", "k5xyz").await;
    let (status, problem) = send_json(
        app.router(),
        "GET",
        &format!("/api/net-definitions/{id}/schedule"),
        None,
        Some(&intruder),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(problem["type"], "/errors/forbidden");

    // An unknown net → the same uniform 404 the write arms give.
    let missing = uuid::Uuid::now_v7();
    let (status, problem) = send_json(
        app.router(),
        "GET",
        &format!("/api/net-definitions/{missing}/schedule"),
        None,
        Some(&owner),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(problem["type"], "/errors/net-definition-not-found");

    // No session at all → 401, never the rule.
    let (status, problem) = send_json(
        app.router(),
        "GET",
        &format!("/api/net-definitions/{id}/schedule"),
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(problem["type"], "/errors/unauthenticated");

    // Signed in but consent still pending → 403 consent-required.
    let unconsented = sign_in_only(&app, "pending@example.com").await;
    let (status, problem) = send_json(
        app.router(),
        "GET",
        &format!("/api/net-definitions/{id}/schedule"),
        None,
        Some(&unconsented),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(problem["type"], "/errors/consent-required");
}

#[tokio::test]
async fn get_schedule_omits_fields_the_frequency_does_not_use() {
    // `ScheduleBody` used to populate weekday/day-of-month from
    // whatever the row held, independent of `frequency`. `parse_schedule` keeps
    // those `None` outside their own frequency, so this is only reachable by a
    // writer that bypassed the domain — which is why the drifted row is written
    // here with direct SQL rather than through the API. Serving the stray field
    // would emit a body `parse_schedule` discards on resubmit, breaking the
    // round-trip for that row.
    let app = test_app().await;
    let cookie = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;
    let created = create_net(&app, &cookie).await;
    let id = created["id"].as_str().expect("id").to_owned();

    // A DAILY rule carrying both a weekday and a day-of-month.
    sqlx::query(
        "INSERT INTO net_schedules
            (definition_id, kind, timezone, frequency, local_hour, local_minute,
             weekday, day_of_month, created_at, updated_at)
         VALUES ($1::uuid, 'recurring', 'UTC', 'daily', 12, 0, 4, 17, now(), now())",
    )
    .bind(&id)
    .execute(&app.pool)
    .await
    .expect("insert drifted schedule row");

    let (status, read) = send_json(
        app.router(),
        "GET",
        &format!("/api/net-definitions/{id}/schedule"),
        None,
        Some(&cookie),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        read,
        json!({
            "kind": "recurring",
            "timezone": "UTC",
            "frequency": "daily",
            "timeOfDay": "12:00"
        }),
        "a daily rule serves neither weekday nor dayOfMonth, whatever the row holds"
    );

    // And the body it does serve round-trips: resubmitting it is accepted and
    // leaves an equivalent daily rule rather than being partly discarded.
    let (status, _) = send_json(
        app.router(),
        "PUT",
        &format!("/api/net-definitions/{id}/schedule"),
        Some(read.clone()),
        Some(&cookie),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (_, reread) = send_json(
        app.router(),
        "GET",
        &format!("/api/net-definitions/{id}/schedule"),
        None,
        Some(&cookie),
    )
    .await;
    assert_eq!(
        reread, read,
        "the served body is a fixed point of the write path"
    );
}
