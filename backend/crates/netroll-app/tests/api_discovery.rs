// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
//! Full-loop API tests for the PUBLIC discovery landing endpoint: real router,
//! real Postgres (testcontainers), capturing mailer. Asserts status codes, the
//! `{ activeNow, upcoming }` wire shape, the load-bearing REDACTION (no owner
//! ids leak; a Listed net's link token is served on purpose, an Unlisted one's
//! never is), membership, filter/sort behaviour and problem+json validation.

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
    /// Direct Postgres access, for the one test that must diverge the table's
    /// HEAP order from its `position` order — a thing no endpoint can do,
    /// because `replace_connections` always rewrites the set in dense position
    /// order. `api_net_definitions.rs`'s own pin set the precedent.
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

/// GET returning the RAW response body string (for substring-absence assertions
/// that must inspect the exact bytes on the wire).
async fn get_raw(router: Router, uri: &str) -> (StatusCode, String) {
    let request = Request::builder()
        .method("GET")
        .uri(uri)
        .body(Body::empty())
        .expect("build request");
    let response = router.oneshot(request).await.expect("route request");
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024)
        .await
        .expect("body");
    (
        status,
        String::from_utf8(bytes.to_vec()).expect("utf8 body"),
    )
}

/// GET returning status + `content-type` + the RAW body bytes.
///
/// An extractor rejection's body is not necessarily JSON, so the media type has
/// to be asserted BEFORE any parse is attempted — [`send_json`] would panic on a
/// `text/plain` rejection body and hide which contract actually answered.
async fn get_untyped(router: Router, uri: &str) -> (StatusCode, String, String) {
    let request = Request::builder()
        .method("GET")
        .uri(uri)
        .body(Body::empty())
        .expect("build request");
    let response = router.oneshot(request).await.expect("route request");
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

/// Creates a net from `overrides` merged over the minimal valid body, returns
/// the created definition body.
async fn create_net(app: &TestApp, cookie: &str, overrides: Value) -> Value {
    let mut body = json!({
        "title": "Sunday Traffic Net",
        "connections": [
            { "kind": "hf", "plannedFrequencyHz": 14_230_000, "band": "20m", "mode": "ssb" }
        ],
        "netCategory": "traffic",
        "netType": "open"
    });
    // `band`/`mode`/`plannedFrequencyHz` are the first CONNECTION's facts since
    // So an override naming one lands on `connections[0]`; every
    // other key is a scalar field of the definition.
    for (k, v) in overrides.as_object().expect("overrides object") {
        if matches!(k.as_str(), "band" | "mode" | "plannedFrequencyHz") {
            body["connections"][0][k] = v.clone();
        } else {
            body[k] = v.clone();
        }
    }
    let (status, created) = send_json(
        app.router(),
        "POST",
        "/api/net-definitions",
        Some(body),
        Some(cookie),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    created
}

/// Gives the net a single FUTURE one-off occurrence (far enough ahead to stay
/// upcoming relative to the app's system clock).
async fn schedule_future(app: &TestApp, cookie: &str, id: &str, start: &str) {
    let (status, _) = send_json(
        app.router(),
        "PUT",
        &format!("/api/net-definitions/{id}/schedule"),
        Some(json!({ "kind": "one-off", "timezone": "UTC", "oneOffStartAt": start })),
        Some(cookie),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
}

/// Starts a live session on `id` and returns its session id.
async fn start_session(app: &TestApp, cookie: &str, id: &str) -> String {
    let (status, session) = send_json(
        app.router(),
        "POST",
        "/api/net-sessions",
        Some(json!({ "definitionId": id })),
        Some(cookie),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    session["id"].as_str().expect("session id").to_owned()
}

#[tokio::test]
async fn discovery_is_public_and_requires_no_session() {
    let app = test_app().await;

    // Reachable with NO session cookie (merged outside require_session).
    let (status, _) = send_json(app.router(), "GET", "/api/discovery", None, None).await;
    assert_eq!(status, StatusCode::OK, "discovery is a public read");

    // Contrast: a session-gated route refuses the same account-less request.
    let (gated, _) = send_json(app.router(), "GET", "/api/accounts/me", None, None).await;
    assert_eq!(
        gated,
        StatusCode::UNAUTHORIZED,
        "a gated route still requires a session — proving discovery is deliberately ungated"
    );
}

#[tokio::test]
async fn discovery_shape_has_empty_active_now_and_populated_upcoming() {
    let app = test_app().await;
    let cookie = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;
    let created = create_net(&app, &cookie, json!({ "title": "Sunday Traffic Net" })).await;
    let id = created["id"].as_str().expect("id");
    schedule_future(&app, &cookie, id, "2027-01-01T20:00:00Z").await;

    let (status, body) = send_json(app.router(), "GET", "/api/discovery", None, None).await;
    assert_eq!(status, StatusCode::OK);

    // Stable wire contract: activeNow is present but empty here — no session
    // was started for this net (the test right below proves activeNow DOES
    // populate once one is; this used to be a hardcoded-always-empty gap
    // left over from before net_sessions existed, never wired up after).
    let active = body["activeNow"].as_array().expect("activeNow is an array");
    assert!(active.is_empty(), "no session is live for this net");

    let upcoming = body["upcoming"].as_array().expect("upcoming is an array");
    assert_eq!(upcoming.len(), 1, "the one scheduled net is upcoming");
    let row = &upcoming[0];
    assert_eq!(row["title"], "Sunday Traffic Net");
    assert_eq!(row["connections"][0]["band"], "20m");
    assert_eq!(row["connections"][0]["mode"], "ssb");
    assert_eq!(row["netCategory"], "traffic");
    assert_eq!(
        row["id"], created["id"],
        "the net's definition id is carried"
    );
    assert!(
        row["occurrenceId"].is_string(),
        "the occurrence id is carried"
    );
    // Times are RFC 3339 UTC on the wire; local render is the client's.
    assert_eq!(row["scheduledStartAt"], "2027-01-01T20:00:00+00:00");
}

#[tokio::test]
async fn discovery_active_now_lists_a_live_session_of_a_listed_net() {
    // The real gap this closes: a net can be live (someone started it) with
    // NO schedule at all — the "Upcoming" list and "Live now" are genuinely
    // independent, not one derived from the other.
    let app = test_app().await;
    let cookie = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;
    let created = create_net(&app, &cookie, json!({ "title": "Live Right Now Net" })).await;
    let id = created["id"].as_str().expect("id");
    let session_id = start_session(&app, &cookie, id).await;

    let (status, body) = send_json(app.router(), "GET", "/api/discovery", None, None).await;
    assert_eq!(status, StatusCode::OK);

    let upcoming = body["upcoming"].as_array().expect("upcoming is an array");
    assert!(upcoming.is_empty(), "no schedule was ever set");

    let active = body["activeNow"].as_array().expect("activeNow is an array");
    assert_eq!(active.len(), 1, "the started session is live right now");
    let row = &active[0];
    assert_eq!(row["title"], "Live Right Now Net");
    assert_eq!(
        row["id"], created["id"],
        "the net's definition id is carried"
    );
    // The frontend's "Watch live"/"Check in" hero links build `/live/:id`
    // from this field (design handoff `1a`) — for a live entry that has to
    // resolve to the live SESSION, not an occurrence (there may be none).
    assert_eq!(
        row["occurrenceId"], session_id,
        "the live session id, so the hero card's Watch-live link resolves"
    );
}

#[tokio::test]
async fn discovery_active_now_excludes_a_live_session_on_an_unlisted_net() {
    let app = test_app().await;
    let cookie = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;
    let created = create_net(
        &app,
        &cookie,
        json!({ "title": "Private Live Net", "visibility": "unlisted" }),
    )
    .await;
    let id = created["id"].as_str().expect("id");
    start_session(&app, &cookie, id).await;

    let (status, body) = send_json(app.router(), "GET", "/api/discovery", None, None).await;
    assert_eq!(status, StatusCode::OK);

    let active = body["activeNow"].as_array().expect("activeNow is an array");
    assert!(
        active.is_empty(),
        "an Unlisted net's live session must never leak into the public discovery feed"
    );
}

#[tokio::test]
async fn an_upcoming_row_carries_the_listed_nets_link_token_from_the_same_read() {
    // The title on a discovery card opens the net, and the
    // client builds `/nets/t/{linkToken}` from the row — so the token has to
    // ride the same joined row the rest of the card comes from.
    let app = test_app().await;
    let cookie = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;
    let created = create_net(&app, &cookie, json!({ "title": "Sunday Traffic Net" })).await;
    let id = created["id"].as_str().expect("id");
    schedule_future(&app, &cookie, id, "2027-01-01T20:00:00Z").await;
    let expected_token = created["linkToken"].as_str().expect("link token");

    let (status, body) = send_json(app.router(), "GET", "/api/discovery", None, None).await;
    assert_eq!(status, StatusCode::OK);

    let upcoming = body["upcoming"].as_array().expect("upcoming is an array");
    assert_eq!(upcoming.len(), 1);
    assert_eq!(
        upcoming[0]["linkToken"].as_str(),
        Some(expected_token),
        "an upcoming card carries the token POST /api/net-definitions issued"
    );
}

#[tokio::test]
async fn an_active_now_row_carries_the_listed_nets_link_token_from_the_same_read() {
    // The two collections are two independent SQL statements:
    // a token added to the upcoming read alone leaves every live card without
    // one, so the live collection is fenced on its own.
    let app = test_app().await;
    let cookie = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;
    let created = create_net(&app, &cookie, json!({ "title": "Live Right Now Net" })).await;
    let id = created["id"].as_str().expect("id");
    start_session(&app, &cookie, id).await;
    let expected_token = created["linkToken"].as_str().expect("link token");

    let (status, body) = send_json(app.router(), "GET", "/api/discovery", None, None).await;
    assert_eq!(status, StatusCode::OK);

    let active = body["activeNow"].as_array().expect("activeNow is an array");
    assert_eq!(active.len(), 1);
    assert_eq!(
        active[0]["linkToken"].as_str(),
        Some(expected_token),
        "a live card carries the token POST /api/net-definitions issued"
    );
}

#[tokio::test]
async fn an_unlisted_nets_token_never_crosses_the_discovery_wire_live_or_scheduled() {
    // A regression fence: now that discovery serves tokens,
    // the Listed-only predicate on BOTH reads is what keeps an Unlisted net's
    // token — a real capability — off a world-reachable surface. The net is
    // seeded BOTH live and scheduled so a broken predicate on either statement
    // fails this, not only the one an occurrence-only fixture reaches.
    let app = test_app().await;
    let cookie = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;
    let created = create_net(
        &app,
        &cookie,
        json!({ "title": "Private Net", "visibility": "unlisted" }),
    )
    .await;
    let id = created["id"].as_str().expect("id");
    let token = created["linkToken"].as_str().expect("link token");
    start_session(&app, &cookie, id).await;
    schedule_future(&app, &cookie, id, "2027-01-01T20:00:00Z").await;

    let (status, raw) = get_raw(app.router(), "/api/discovery").await;
    assert_eq!(status, StatusCode::OK);
    let body: Value = serde_json::from_str(&raw).expect("body is JSON");
    assert!(
        body["activeNow"].as_array().expect("array").is_empty(),
        "an Unlisted net's live session is absent from activeNow"
    );
    assert!(
        body["upcoming"].as_array().expect("array").is_empty(),
        "an Unlisted net's occurrence is absent from upcoming"
    );
    assert!(
        !raw.contains(token),
        "an Unlisted net's token never appears anywhere in the discovery bytes"
    );
}

#[tokio::test]
async fn discovery_serves_a_listed_nets_token_but_never_its_owner_ids_or_visibility() {
    // Three independent properties share this test, and one of them reverses:
    // the token of a Listed net IS served on purpose — the card's title
    // opens `/nets/t/{linkToken}` — while owner identities and `visibility`
    // stay redacted, and those two halves are what this test still guards.
    let app = test_app().await;
    let cookie = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;
    let created = create_net(&app, &cookie, json!({ "title": "Sunday Traffic Net" })).await;
    let id = created["id"].as_str().expect("id");
    schedule_future(&app, &cookie, id, "2027-01-01T20:00:00Z").await;

    let link_token = created["linkToken"]
        .as_str()
        .expect("link token")
        .to_owned();
    let owner_id = created["ownerAccountIds"][0]
        .as_str()
        .expect("owner id")
        .to_owned();
    assert!(!link_token.is_empty() && !owner_id.is_empty());

    // A co-owned net is the realistic production shape — the
    // redaction guarantee must hold for EVERY owner, not just the sole/first
    // one; checking `ownerAccountIds[0]` alone would miss a co-owner.
    sign_in_consent_callsign(&app, "co-owner@example.com", "k2xyz").await;
    let (status, add_owner_body) = send_json(
        app.router(),
        "POST",
        &format!("/api/net-definitions/{id}/owners"),
        Some(json!({ "callsign": "k2xyz" })),
        Some(&cookie),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "add a co-owner");
    let co_owner_id = add_owner_body["ownerAccountIds"][1]
        .as_str()
        .expect("co-owner id")
        .to_owned();
    assert_ne!(owner_id, co_owner_id);

    // The raw response bytes must contain NO owner account id, of EITHER
    // owner — the load-bearing security property that remains.
    let (status, raw) = get_raw(app.router(), "/api/discovery").await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        !raw.contains(&owner_id),
        "discovery must never leak the first owner's account id"
    );
    assert!(
        !raw.contains(&co_owner_id),
        "discovery must never leak a co-owner's account id"
    );

    // The typed row carries the token — and nothing else that was redacted.
    let (_, body) = send_json(app.router(), "GET", "/api/discovery", None, None).await;
    let row = &body["upcoming"][0];
    assert_eq!(
        row["linkToken"].as_str(),
        Some(link_token.as_str()),
        "a Listed net's token is served so its title can open the net"
    );
    assert!(
        row.get("ownerAccountIds").is_none(),
        "no ownerAccountIds field"
    );
    assert!(
        row.get("visibility").is_none(),
        "no visibility field (always listed)"
    );
}

#[tokio::test]
async fn discovery_excludes_unlisted_and_archived() {
    let app = test_app().await;
    let cookie = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;

    let listed = create_net(&app, &cookie, json!({ "title": "Listed Net" })).await;
    let listed_id = listed["id"].as_str().expect("id").to_owned();
    schedule_future(&app, &cookie, &listed_id, "2027-01-01T20:00:00Z").await;

    let unlisted = create_net(
        &app,
        &cookie,
        json!({ "title": "Hidden Net", "visibility": "unlisted" }),
    )
    .await;
    let unlisted_id = unlisted["id"].as_str().expect("id").to_owned();
    schedule_future(&app, &cookie, &unlisted_id, "2027-01-02T20:00:00Z").await;

    let archived = create_net(&app, &cookie, json!({ "title": "Archived Net" })).await;
    let archived_id = archived["id"].as_str().expect("id").to_owned();
    schedule_future(&app, &cookie, &archived_id, "2027-01-03T20:00:00Z").await;
    let (status, _) = send_json(
        app.router(),
        "DELETE",
        &format!("/api/net-definitions/{archived_id}"),
        None,
        Some(&cookie),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    let (status, body) = send_json(app.router(), "GET", "/api/discovery", None, None).await;
    assert_eq!(status, StatusCode::OK);

    let mut ids: Vec<String> = Vec::new();
    for coll in ["activeNow", "upcoming"] {
        for row in body[coll].as_array().expect("array") {
            ids.push(row["id"].as_str().expect("id").to_owned());
        }
    }
    assert!(ids.contains(&listed_id), "the Listed net appears");
    assert!(
        !ids.contains(&unlisted_id),
        "an Unlisted net never appears in discovery"
    );
    assert!(
        !ids.contains(&archived_id),
        "an archived net never appears in discovery"
    );
}

#[tokio::test]
async fn discovery_filters_narrow_and_sort_reorders() {
    let app = test_app().await;
    let cookie = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;

    // "Zulu" on 20m sooner; "Alpha" on 2m later.
    let zulu = create_net(&app, &cookie, json!({ "title": "Zulu Net", "band": "20m" })).await;
    let zulu_id = zulu["id"].as_str().expect("id").to_owned();
    schedule_future(&app, &cookie, &zulu_id, "2027-01-01T20:00:00Z").await;

    let alpha = create_net(
        &app,
        &cookie,
        json!({ "title": "Alpha Net", "band": "2m", "mode": "fm" }),
    )
    .await;
    let alpha_id = alpha["id"].as_str().expect("id").to_owned();
    schedule_future(&app, &cookie, &alpha_id, "2027-02-01T20:00:00Z").await;

    // Band filter narrows to the 2m net only.
    let (status, body) = send_json(app.router(), "GET", "/api/discovery?band=2m", None, None).await;
    assert_eq!(status, StatusCode::OK);
    let ids: Vec<&str> = body["upcoming"]
        .as_array()
        .expect("array")
        .iter()
        .map(|r| r["id"].as_str().expect("id"))
        .collect();
    assert_eq!(
        ids,
        vec![alpha_id.as_str()],
        "band=2m returns only the 2m net"
    );

    // Default (time) sort: Zulu (sooner) first.
    let (_, by_time) = send_json(app.router(), "GET", "/api/discovery", None, None).await;
    let time_order: Vec<&str> = by_time["upcoming"]
        .as_array()
        .expect("array")
        .iter()
        .map(|r| r["id"].as_str().expect("id"))
        .collect();
    assert_eq!(time_order, vec![zulu_id.as_str(), alpha_id.as_str()]);

    // sort=name reorders: Alpha before Zulu regardless of time.
    let (_, by_name) = send_json(app.router(), "GET", "/api/discovery?sort=name", None, None).await;
    let name_order: Vec<&str> = by_name["upcoming"]
        .as_array()
        .expect("array")
        .iter()
        .map(|r| r["id"].as_str().expect("id"))
        .collect();
    assert_eq!(name_order, vec![alpha_id.as_str(), zulu_id.as_str()]);
}

#[tokio::test]
async fn the_response_echoes_the_filters_and_sort_the_server_actually_applied() {
    // Leniency means a typo'd key is dropped in silence, so
    // the compensating control is the server stating what it applied. The echo
    // carries the SORT in the same object as the filters because `band`/`mode`
    // are no longer sort options, which turns a live `?sort=band` URL into a
    // fallback that must be visible.
    let app = test_app().await;
    let cookie = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;
    let created = create_net(
        &app,
        &cookie,
        json!({ "title": "Sunday Traffic Net", "band": "20m" }),
    )
    .await;
    let id = created["id"].as_str().expect("id");
    schedule_future(&app, &cookie, id, "2027-01-01T20:00:00Z").await;

    // (i) applied filters and sort echo exactly what was honoured.
    let (status, body) = send_json(
        app.router(),
        "GET",
        "/api/discovery?band=20m&sort=name",
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let applied = body["applied"].as_object().expect("applied echo");
    assert_eq!(applied["band"], "20m");
    assert_eq!(applied["sort"], "name");
    assert!(
        !applied.contains_key("mode"),
        "an unfiltered dimension is ABSENT, never null — key presence is what says \"applied\""
    );

    // (ii) an unknown key is dropped, and the echo shows it was not applied.
    let (status, body) =
        send_json(app.router(), "GET", "/api/discovery?bnad=20m", None, None).await;
    assert_eq!(status, StatusCode::OK, "leniency is unchanged");
    let applied = body["applied"].as_object().expect("applied echo");
    assert!(
        !applied.contains_key("band"),
        "a typo'd key applied no band filter, and the echo is how a client can tell"
    );

    // (iii) no sort requested echoes the default the server actually used.
    let (_, body) = send_json(app.router(), "GET", "/api/discovery", None, None).await;
    assert_eq!(
        body["applied"]["sort"], "time",
        "the applied sort is always stated, so a fallback can never be silent"
    );

    // (iv) a blank filter is not an applied filter.
    let (status, body) = send_json(app.router(), "GET", "/api/discovery?band=", None, None).await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        !body["applied"]
            .as_object()
            .expect("applied echo")
            .contains_key("band"),
        "blank folds to no filter, so it must not echo as one"
    );
}

#[tokio::test]
async fn the_applied_echo_is_keyed_by_the_query_param_names_not_the_internal_ones() {
    // `q` and `type` are renamed on the way in. An echo
    // keyed `name`/`netType` could not be joined back to the request a client
    // sent, which is the echo's whole purpose.
    let app = test_app().await;
    let cookie = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;
    let created = create_net(
        &app,
        &cookie,
        json!({ "title": "Sunday Traffic Net", "netType": "roll-call" }),
    )
    .await;
    let id = created["id"].as_str().expect("id");
    schedule_future(&app, &cookie, id, "2027-01-01T20:00:00Z").await;

    let (status, body) = send_json(
        app.router(),
        "GET",
        "/api/discovery?q=%20%20Sunday%20%20&type=roll-call",
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let applied = body["applied"].as_object().expect("applied echo");
    assert_eq!(applied["q"], "Sunday", "post-trim, as applied");
    assert_eq!(applied["type"], "roll-call");
    assert!(!applied.contains_key("name") && !applied.contains_key("netType"));
}

#[tokio::test]
async fn discovery_rejects_an_unknown_enum_filter_with_a_field_level_400() {
    let app = test_app().await;
    let cookie = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;
    let created = create_net(&app, &cookie, json!({ "title": "Sunday Traffic Net" })).await;
    let id = created["id"].as_str().expect("id");
    schedule_future(&app, &cookie, id, "2027-01-01T20:00:00Z").await;

    let (status, body) = send_json(
        app.router(),
        "GET",
        "/api/discovery?band=nonsense",
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["type"], "/errors/discovery-query-invalid");
    // The rejection is total — no partial/unfiltered result rides along.
    assert!(
        body.get("upcoming").is_none(),
        "an invalid filter returns a problem, never a partial result set"
    );
    // The field-level detail names the offending field.
    assert!(
        body["detail"]
            .as_str()
            .expect("detail")
            .starts_with("band:"),
        "the problem detail names the band field"
    );
}

#[tokio::test]
async fn a_structurally_invalid_query_string_answers_in_problem_json_unauthenticated() {
    // On an unauthenticated site no auth layer runs first, so
    // this is a rejection an anonymous caller can observe directly. A repeated
    // key cannot deserialize into a single-valued field, and that refusal must
    // be problem+json rather than axum's `text/plain` default.
    //
    // `/api/discovery` is not the only unauthenticated site among the `Query`
    // extractors: `public_session_events_since` is also unauthenticated, and so
    // is the public WS upgrade. Both WS upgrades answering problem+json (not
    // `text/plain`) to an anonymous caller are covered by
    // `api_net_session_ws.rs::a_malformed_ws_resume_cursor_answers_in_problem_json_on_both_upgrade_routes`.
    let app = test_app().await;

    let (status, content_type, raw) =
        get_untyped(app.router(), "/api/discovery?band=2m&band=40m").await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(
        content_type, "application/problem+json",
        "a query rejection answers in the problem+json contract"
    );
    let problem: Value = serde_json::from_str(&raw).expect("problem body is JSON");
    assert_eq!(problem["type"], "/errors/validation");
    assert_eq!(problem["status"], 400);
    // The rejection is total, not a partially-filtered 200: `DiscoveryParams`
    // has container-level `default` and no `deny_unknown_fields`, so a
    // pass-through extractor would have bound `band=40m` and answered a
    // filtered list. Proven by the slug rather than by an absent field —
    // `parse_discovery_query`'s own refusal maps to
    // `/errors/discovery-query-invalid`, so `/errors/validation` can only have
    // come from the `AppQuery` path. (The previous
    // `assert!(problem.get("upcoming").is_none())` here could not fail: a
    // `Problem` carries only type/title/status/detail.)
}

// ---------------------------------------------------------------------------
// IP-keyed read governor on the public reads.
//
// These MUST drive `api_router_ip_limited` and inject `ConnectInfo`: a
// `SmartIpKeyExtractor` layer is only exercised when a peer/forwarded IP is
// present (the bare `api_router` used by every other test has no governor by
// design — see the regression guard below). Modeled on the auth IP-layer
// tests (`api_auth.rs`). Asserts LOGIC — status codes, the `/errors/rate-limited`
// slug, the `Retry-After` header, and per-IP bucket independence — never prose.
// ---------------------------------------------------------------------------

/// GET `uri` through the IP-limited router from `peer`, optionally spoofing a
/// forwarded client IP, returning status + headers + parsed body.
async fn get_via_ip_limited(
    router: Router,
    uri: &str,
    peer: std::net::SocketAddr,
    forwarded_for: Option<&str>,
) -> (StatusCode, axum::http::HeaderMap, Value) {
    use axum::extract::ConnectInfo;

    let mut builder = Request::builder().method("GET").uri(uri);
    if let Some(client) = forwarded_for {
        builder = builder.header("x-forwarded-for", client);
    }
    let mut request = builder.body(Body::empty()).expect("build request");
    request.extensions_mut().insert(ConnectInfo(peer));
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

#[tokio::test]
async fn public_reads_are_ip_throttled_eventually_with_retry_after_and_slug() {
    use netroll_app::http::api_router_ip_limited;
    use netroll_app::http::rate_limit::READ_BURST;
    use std::net::SocketAddr;

    let app = test_app().await;
    let router = api_router_ip_limited(app.state.clone());

    // BOTH governed public reads, each from its own single peer. A tight loop of
    // > 2 * READ_BURST reads must eventually be throttled; because the layer
    // replenishes on real wall-clock time we assert "at least one 429", not an
    // exact Nth. `/api/discovery` returns 200 under threshold; a non-resolving
    // by-token returns 404 under threshold (the layer runs BEFORE the handler,
    // so it throttles regardless of the handler's own status).
    //
    // NOTE: `SmartIpKeyExtractor`'s key is an `IpAddr` — it DROPS the port. Two
    // peers that differ only in port (e.g. `127.0.0.1:41001` vs `127.0.0.1:41002`)
    // collapse into the SAME bucket. Each scenario below therefore varies the IP
    // octet, not the port, so the two loops genuinely exercise independent buckets
    // (a ports-only version would share one bucket across both endpoints here).
    for (uri, peer_octet) in [
        ("/api/discovery", 1u8),
        ("/api/net-definitions/by-token/does-not-exist", 2u8),
    ] {
        let peer = SocketAddr::from(([127, 0, 0, peer_octet], 41_000));
        let mut saw_429 = false;
        let mut saw_retry_after = false;
        let mut saw_slug = false;
        for _ in 0..(2 * READ_BURST + 5) {
            let (status, headers, body) = get_via_ip_limited(router.clone(), uri, peer, None).await;
            if status == StatusCode::TOO_MANY_REQUESTS {
                saw_429 = true;
                saw_retry_after |= headers.contains_key(header::RETRY_AFTER);
                saw_slug |= body["type"] == "/errors/rate-limited";
            }
        }
        assert!(
            saw_429,
            "reads from one IP against {uri} must eventually 429"
        );
        assert!(saw_retry_after, "a 429 for {uri} carries Retry-After");
        assert!(
            saw_slug,
            "a 429 for {uri} is the /errors/rate-limited problem"
        );
    }
}

#[tokio::test]
async fn public_reads_under_threshold_are_never_throttled() {
    use netroll_app::http::api_router_ip_limited;
    use netroll_app::http::rate_limit::READ_BURST;
    use std::net::SocketAddr;

    let app = test_app().await;
    let router = api_router_ip_limited(app.state.clone());
    // Distinct IPs (NOT just distinct ports — `SmartIpKeyExtractor`'s key is an
    // `IpAddr`, which drops the port) so each endpoint's under-threshold loop
    // below spends its OWN bucket. Sharing one IP across both loops would make
    // their combined total (`under * 2`) equal `READ_BURST` exactly — the
    // boundary, not "comfortably under" it.
    let peer_discovery = SocketAddr::from(([127, 0, 0, 11], 41_010));
    let peer_by_token = SocketAddr::from(([127, 0, 0, 12], 41_010));

    // A modest number of reads (well under the burst) from one peer all return
    // their normal status, never 429: requests under the threshold succeed.
    // Half the burst is comfortably under each endpoint's own ceiling.
    let under = READ_BURST / 2;
    for _ in 0..under {
        let (status, _, _) =
            get_via_ip_limited(router.clone(), "/api/discovery", peer_discovery, None).await;
        assert_eq!(
            status,
            StatusCode::OK,
            "an under-threshold discovery read succeeds"
        );
    }
    for _ in 0..under {
        let (status, _, _) = get_via_ip_limited(
            router.clone(),
            "/api/net-definitions/by-token/nope",
            peer_by_token,
            None,
        )
        .await;
        assert_eq!(
            status,
            StatusCode::NOT_FOUND,
            "an under-threshold by-token read returns its normal 404, not 429"
        );
    }
}

#[tokio::test]
async fn read_governor_isolates_distinct_forwarded_client_ips() {
    use netroll_app::http::api_router_ip_limited;
    use netroll_app::http::rate_limit::READ_BURST;
    use std::net::SocketAddr;

    let app = test_app().await;
    let router = api_router_ip_limited(app.state.clone());
    // Every request arrives from ONE TCP peer (the reverse proxy); the forwarded
    // header is the real client key.
    let proxy_peer = SocketAddr::from(([127, 0, 0, 1], 41_020));

    // Many requests from N DISTINCT forwarded clients through one proxy peer:
    // each has its own bucket, so nobody is throttled even though the shared
    // peer sees far more than one bucket's worth of traffic.
    let clients = (2 * READ_BURST) as usize;
    for n in 0..clients {
        let (status, _, _) = get_via_ip_limited(
            router.clone(),
            "/api/discovery",
            proxy_peer,
            Some(&format!("203.0.113.{}", n % 250)),
        )
        .await;
        assert_eq!(
            status,
            StatusCode::OK,
            "distinct forwarded clients must not share the proxy's bucket (client {n})"
        );
    }

    // One hammering forwarded client DOES eventually trip its own bucket.
    let mut saw_429 = false;
    for _ in 0..(2 * READ_BURST + 5) {
        let (status, _, _) = get_via_ip_limited(
            router.clone(),
            "/api/discovery",
            proxy_peer,
            Some("198.51.100.7"),
        )
        .await;
        saw_429 |= status == StatusCode::TOO_MANY_REQUESTS;
    }
    assert!(
        saw_429,
        "one hammering forwarded client trips its own bucket"
    );

    // A fresh forwarded client still gets a normal status in the same run —
    // the throttle is scoped to the offending IP, not the shared peer.
    let (fresh, _, _) = get_via_ip_limited(
        router.clone(),
        "/api/discovery",
        proxy_peer,
        Some("198.51.100.200"),
    )
    .await;
    assert_eq!(
        fresh,
        StatusCode::OK,
        "a fresh client is unaffected while another IP is throttled"
    );
}

#[tokio::test]
async fn bare_api_router_has_no_read_governor_and_never_500s() {
    // Regression guard: the bare `api_router` (what `app.router()` builds,
    // and what every other test drives) carries NO governor and NO `ConnectInfo`
    // dependency. If the read governor were layered here, `SmartIpKeyExtractor`
    // would fail to extract a key and 500. Drive both public reads with no
    // ConnectInfo and assert the honest handler status, never a 500.
    let app = test_app().await;

    let (status, _) = send_json(app.router(), "GET", "/api/discovery", None, None).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "discovery on the bare router returns 200, never a key-extraction 500"
    );

    let (status, _) = send_json(
        app.router(),
        "GET",
        "/api/net-definitions/by-token/whatever",
        None,
        None,
    )
    .await;
    assert_eq!(
        status,
        StatusCode::NOT_FOUND,
        "by-token on the bare router returns its 404, never a key-extraction 500"
    );
}

#[tokio::test]
async fn an_unrecognised_query_parameter_is_ignored_on_the_public_discovery_read() {
    // The LENIENT counter-direction, and it must pass BEFORE
    // and AFTER. `/api/discovery` is public, anonymous, and all nine of its
    // parameters are optional filters
    // with a container-level `default`, so a dropped filter widens a discovery
    // list to the endpoint's own no-filter answer rather than misrepresenting a
    // restriction. This test exists to fail loudly if the strictness that
    // applies to the four filtered reads leaks into project-wide strictness —
    // the mirror image of the guard above.
    //
    // Asserted positively rather than as "not a 400": the unrecognised key must
    // produce the SAME answer as no query string at all, which is what "the key
    // is ignored" means. A bare `assert_ne!(status, 400)` would also hold if the
    // key had been partially applied.
    let app = test_app().await;

    let (bare_status, bare_content_type, bare_body) =
        get_untyped(app.router(), "/api/discovery").await;
    assert_eq!(bare_status, StatusCode::OK);
    assert_eq!(bare_content_type, "application/json");

    for uri in [
        "/api/discovery?bandd=40m",
        "/api/discovery?_t=1724716800",
        "/api/discovery?actorr=00000000-0000-0000-0000-000000000001",
    ] {
        let (status, content_type, body) = get_untyped(app.router(), uri).await;
        assert_eq!(status, StatusCode::OK, "{uri} stays lenient");
        assert_eq!(content_type, "application/json", "{uri} media type");
        assert_eq!(
            body, bare_body,
            "{uri} answers exactly as the unfiltered read — the key is ignored, not applied"
        );
    }
}

// ── Filters across every connection ─────────────────────────────
//
// Every fixture below builds its connection set through
// `PUT /api/net-definitions/{id}/connections` and reads the ids back out of the
// response. `create_net` writes the FLAT fields, which mint exactly one
// connection — so a net with three bands cannot be expressed by it, and a
// criterion named "matches on any of its bands" asserted against a
// single-connection fixture would pass the moment it was written, for the wrong
// reason.

/// Replaces `id`'s connection list and returns the definition body the server
/// echoed back. The ids in that body are the ONLY ids the assertions name.
async fn put_connections(
    app: &TestApp,
    cookie: &str,
    id: &str,
    expected_version: i64,
    connections: Value,
) -> Value {
    let (status, body) = send_json(
        app.router(),
        "PUT",
        &format!("/api/net-definitions/{id}/connections"),
        Some(json!({
            "expectedDefinitionVersion": expected_version,
            "connections": connections,
        })),
        Some(cookie),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "connection write: {body}");
    body
}

fn rf(kind: &str, frequency_hz: i64, band: &str, mode: &str) -> Value {
    json!({ "kind": kind, "plannedFrequencyHz": frequency_hz, "band": band, "mode": mode })
}

/// Every definition id in `upcoming`, in the order the server returned them.
fn upcoming_ids(body: &Value) -> Vec<String> {
    body["upcoming"]
        .as_array()
        .expect("upcoming array")
        .iter()
        .map(|r| r["id"].as_str().expect("id").to_owned())
        .collect()
}

/// The one `upcoming` row for `id`, or `None` when the filter excluded it.
fn upcoming_row<'a>(body: &'a Value, id: &str) -> Option<&'a Value> {
    body["upcoming"]
        .as_array()
        .expect("upcoming array")
        .iter()
        .find(|r| r["id"].as_str() == Some(id))
}

/// A listed net with `connections`, scheduled far enough ahead to stay upcoming.
/// Returns `(definition_id, connection ids in position order)`.
async fn multi_connection_net(
    app: &TestApp,
    cookie: &str,
    title: &str,
    connections: Value,
) -> (String, Vec<String>) {
    let created = create_net(app, cookie, json!({ "title": title })).await;
    let id = created["id"].as_str().expect("id").to_owned();
    let version = created["definitionVersion"].as_i64().expect("version");
    let written = put_connections(app, cookie, &id, version, connections).await;
    let ids: Vec<String> = written["connections"]
        .as_array()
        .expect("connections echoed back")
        .iter()
        .map(|c| c["id"].as_str().expect("connection id").to_owned())
        .collect();
    schedule_future(app, cookie, &id, "2027-01-01T20:00:00Z").await;
    (id, ids)
}

#[tokio::test]
async fn an_upcoming_net_whose_connection_rows_are_gone_is_skipped_not_served_as_unreachable() {
    // The owned-nets and favorites lists skip
    // a definition with zero `net_connections` rows; discovery used to be the
    // one list that served it — a 200 card with `connections: []`, a net
    // reachable by nothing. It now takes the same posture the ACTIVE-NOW read
    // already takes for an unreadable snapshot: that card is dropped, the
    // listing is not.
    let app = test_app().await;
    let cookie = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;
    let healthy = create_net(&app, &cookie, json!({ "title": "Healthy Net" })).await;
    let damaged = create_net(&app, &cookie, json!({ "title": "Damaged Net" })).await;
    let healthy_id = healthy["id"].as_str().expect("id").to_owned();
    let damaged_id = damaged["id"].as_str().expect("id").to_owned();
    for id in [&healthy_id, &damaged_id] {
        schedule_future(&app, &cookie, id, "2027-01-01T20:00:00Z").await;
    }
    sqlx::query("DELETE FROM net_connections WHERE definition_id = $1")
        .bind(uuid::Uuid::parse_str(&damaged_id).expect("uuid"))
        .execute(&app.pool)
        .await
        .expect("remove every connection row");

    let (status, body) = send_json(app.router(), "GET", "/api/discovery", None, None).await;
    assert_eq!(status, StatusCode::OK, "got {body}");
    assert_eq!(upcoming_ids(&body), vec![healthy_id]);
}

#[tokio::test]
async fn a_band_filter_matches_a_net_through_any_of_its_connections() {
    // Match-any: the net has three bands and is found by each of them,
    // including the two that are NOT in the flat mirror column.
    let app = test_app().await;
    let cookie = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;
    let (id, _) = multi_connection_net(
        &app,
        &cookie,
        "Three Ways Net",
        json!([
            rf("hf", 14_230_000, "20m", "ssb"),
            rf("repeater", 145_230_000, "2m", "fm"),
            rf("repeater", 440_100_000, "70cm", "fm"),
        ]),
    )
    .await;

    for band in ["20m", "2m", "70cm"] {
        let (status, body) = send_json(
            app.router(),
            "GET",
            &format!("/api/discovery?band={band}"),
            None,
            None,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert!(
            upcoming_ids(&body).contains(&id),
            "?band={band} must find the net through the connection that carries that band"
        );
    }
}

#[tokio::test]
async fn band_and_mode_are_satisfied_by_one_connection_not_by_two_different_ones() {
    // Two halves with DIFFERENT expected colours, shipped together:
    //
    // * the PRESENCE half (`?band=2m&mode=fm`) is genuinely RED before the fix
    // — the flat mirror says `20m`/`ssb`, so nothing matches;
    // * the ABSENCE half (`?band=2m&mode=ssb`) is a REGRESSION PIN, green
    // before the fix and green after it. It exists to refuse ONE specific
    // implementation: two independent `EXISTS` subqueries, which would
    // return this net because it has a 2m way and an ssb way — while having
    // no 2m ssb way at all. That is wrong data with a 200, and it also makes
    // "which connection matched" unanswerable.
    let app = test_app().await;
    let cookie = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;
    let (id, _) = multi_connection_net(
        &app,
        &cookie,
        "Two Ways Net",
        json!([
            rf("hf", 14_230_000, "20m", "ssb"),
            rf("repeater", 145_230_000, "2m", "fm"),
        ]),
    )
    .await;

    let (_, present) = send_json(
        app.router(),
        "GET",
        "/api/discovery?band=2m&mode=fm",
        None,
        None,
    )
    .await;
    assert!(
        upcoming_ids(&present).contains(&id),
        "the 2m connection IS fm, so one connection satisfies both filters"
    );

    let (_, absent) = send_json(
        app.router(),
        "GET",
        "/api/discovery?band=2m&mode=ssb",
        None,
        None,
    )
    .await;
    assert!(
        !upcoming_ids(&absent).contains(&id),
        "no ONE connection is both 2m and ssb, so the net is not a way to get on 2m ssb"
    );
}

#[tokio::test]
async fn a_row_returned_by_a_band_filter_names_the_connection_that_matched() {
    // Query-returned, never re-derived client-side: a hand-written twin of
    // the filter predicate would eventually highlight a connection the server
    // did not match on. Unfiltered, nothing is named — not a guess, and not the
    // first connection.
    let app = test_app().await;
    let cookie = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;
    let (id, connection_ids) = multi_connection_net(
        &app,
        &cookie,
        "Named Match Net",
        json!([
            rf("hf", 14_230_000, "20m", "ssb"),
            rf("repeater", 145_230_000, "2m", "fm"),
        ]),
    )
    .await;

    let (_, filtered) = send_json(app.router(), "GET", "/api/discovery?band=2m", None, None).await;
    let row = upcoming_row(&filtered, &id).expect("the 2m filter finds the net");
    assert_eq!(
        row["matchedConnectionId"].as_str(),
        Some(connection_ids[1].as_str()),
        "the named connection is the position-1 one — the id the write echoed back"
    );

    let (_, unfiltered) = send_json(app.router(), "GET", "/api/discovery", None, None).await;
    let row = upcoming_row(&unfiltered, &id).expect("unfiltered lists the net");
    assert!(
        row.as_object()
            .expect("row object")
            .contains_key("matchedConnectionId"),
        "the key is always present so a client never has to guess whether it was omitted"
    );
    assert!(
        row["matchedConnectionId"].is_null(),
        "no band or mode filter was applied, so no connection matched"
    );
}

#[tokio::test]
async fn when_two_connections_match_the_named_one_is_the_lower_position() {
    // The determinism half, and the ONLY thing guarding `ORDER BY c.position`
    // in `list_discovery_upcoming`'s matched-connection subquery.
    //
    // ⚠️ THE OBVIOUS VERSION OF THIS TEST CANNOT FAIL FOR THE REASON IT EXISTS,
    // AND IT TOOK TWO MEASUREMENTS TO SEE WHY. Written the plain way — PUT three
    // connections, ask `?band=2m`, assert the position-1 id — it stays green with
    // the `ORDER BY` deleted, for two independent reasons stacked on top of each
    // other:
    //
    // 1. `replace_connections` DELETEs the whole set and re-INSERTs it in dense
    // position order, so a fixture built through the endpoint always leaves
    // the table's HEAP order equal to its `position` order. Repeating the
    // request in a loop proves nothing: identical queries return identical
    // heap order.
    // 2. Even once the heap order IS diverged, the planner picks
    // `net_connections_definition_id_position_key` for the correlated
    // subquery — its leading column is the correlation column — and that
    // index HANDS BACK position order for free. Measured: with the index
    // available, the unordered subquery still answers with the position-1
    // row, so the mutation is invisible.
    //
    // Both accidents are removed here. The unique index is dropped so the
    // subplan must be a `Seq Scan` and therefore answers in HEAP order, and the
    // two matching rows' positions are then swapped so heap order and position
    // order disagree. Measured on this exact shape: unordered → the position-2
    // row; `ORDER BY c.position` → the position-1 row. Same instance, same
    // query, two answers by plan — the non-determinism a public unauthenticated
    // endpoint must not have.
    //
    // Dropping the index is not a claim that the index is absent in production.
    // It is what makes the query answer for itself instead of borrowing an
    // ordering from an access path it never asked for. A future migration that
    // reshapes that index must not be able to change which connection this
    // endpoint names.
    let app = test_app().await;
    let cookie = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;
    let (id, connection_ids) = multi_connection_net(
        &app,
        &cookie,
        "Two 2m Ways Net",
        json!([
            rf("hf", 14_230_000, "20m", "ssb"),
            rf("repeater", 145_230_000, "2m", "fm"),
            rf("repeater", 146_520_000, "2m", "fm"),
        ]),
    )
    .await;
    let definition_id = uuid::Uuid::parse_str(&id).expect("uuid");

    // Reaching Postgres directly, as `api_net_definitions.rs`'s own pin does:
    // no endpoint can produce either of these states, and that is the point.
    sqlx::query(
        "ALTER TABLE net_connections DROP CONSTRAINT net_connections_definition_id_position_key",
    )
    .execute(&app.pool)
    .await
    .expect("take the position index away from the planner");
    sqlx::query(
        "UPDATE net_connections SET position = 3 - position \
         WHERE definition_id = $1 AND band = '2m'",
    )
    .bind(definition_id)
    .execute(&app.pool)
    .await
    .expect("swap the two matching rows' positions");

    // Non-vacuity check on the SETUP itself: if the swap silently failed, the
    // assertion below would pass for the old accidental reason.
    let swapped: Vec<(String, i32)> = sqlx::query_as(
        "SELECT id::text, position FROM net_connections \
         WHERE definition_id = $1 AND band = '2m' ORDER BY position",
    )
    .bind(definition_id)
    .fetch_all(&app.pool)
    .await
    .expect("read the swapped positions back");
    assert_eq!(
        swapped,
        vec![
            (connection_ids[2].clone(), 1),
            (connection_ids[1].clone(), 2)
        ],
        "the connection written LAST must now hold the LOWEST position, or heap \
         order and position order still agree and this test proves nothing"
    );

    let (_, body) = send_json(app.router(), "GET", "/api/discovery?band=2m", None, None).await;
    let row = upcoming_row(&body, &id).expect("found");
    assert_eq!(
        row["matchedConnectionId"].as_str(),
        Some(connection_ids[2].as_str()),
        "the LOWEST-POSITION matching connection — which a sequential scan reaches \
         SECOND, so only `ORDER BY c.position` can name it"
    );
}

#[tokio::test]
async fn an_internet_only_net_is_matched_by_neither_its_stale_band_nor_its_stale_mode() {
    // The net has no RF way at all. When this was written the definition's
    // flat `band`/`mode` columns still held `20m`/`ssb` (the mirror's single
    // writer refused to invent a value for a NOT NULL column it had none for);
    // The flat columns are gone, so there is no stale band left anywhere
    // on the definition. The pin still earns its place: discovery consults the
    // connections and NOTHING on the definition may match a filter for a net
    // that has no RF way — the "so nothing regresses" `COALESCE` this test was
    // written to forbid has no column to fall back to now, and must stay gone.
    let app = test_app().await;
    let cookie = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;
    let (internet_only, _) = multi_connection_net(
        &app,
        &cookie,
        "EchoLink Only Net",
        json!([{ "kind": "echolink", "node": "12345" }]),
    )
    .await;
    // A net that really is on 20m ssb, so neither filter below is vacuously
    // empty — each returns a row, just never the internet-only one.
    let (rf_net, _) = multi_connection_net(
        &app,
        &cookie,
        "Actually On Twenty Net",
        json!([rf("hf", 14_230_000, "20m", "ssb")]),
    )
    .await;

    for uri in ["/api/discovery?band=20m", "/api/discovery?mode=ssb"] {
        let (status, body) = send_json(app.router(), "GET", uri, None, None).await;
        assert_eq!(status, StatusCode::OK);
        let ids = upcoming_ids(&body);
        assert!(
            ids.contains(&rf_net),
            "{uri} still finds the net that genuinely has that way on"
        );
        assert!(
            !ids.contains(&internet_only),
            "{uri} must not find a net whose only way on is an EchoLink node"
        );
    }
}

// ── Filter by connection kind ──────────────────────────────────

/// One EchoLink way in — the connection shape with no band and no mode.
fn echolink(node: &str) -> Value {
    json!({ "kind": "echolink", "node": node })
}

#[tokio::test]
async fn a_kind_filter_finds_a_net_with_no_band_or_mode() {
    // BOTH halves. The EchoLink-only net is findable by nothing else — it has
    // no band and no mode. `?kind=echolink` must return it
    // and NOT the HF net; `?kind=hf` the reverse. A predicate that is
    // accidentally a no-op passes the first half alone, which is why the
    // absence half is asserted on each request.
    let app = test_app().await;
    let cookie = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;
    let (internet_only, _) = multi_connection_net(
        &app,
        &cookie,
        "EchoLink Only Net",
        json!([echolink("12345")]),
    )
    .await;
    let (rf_net, _) = multi_connection_net(
        &app,
        &cookie,
        "Twenty Metre Net",
        json!([rf("hf", 14_230_000, "20m", "ssb")]),
    )
    .await;

    let (status, body) = send_json(
        app.router(),
        "GET",
        "/api/discovery?kind=echolink",
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "got {body}");
    assert_eq!(
        upcoming_ids(&body),
        vec![internet_only.clone()],
        "?kind=echolink finds the EchoLink net and not the HF one"
    );

    let (status, body) = send_json(app.router(), "GET", "/api/discovery?kind=hf", None, None).await;
    assert_eq!(status, StatusCode::OK, "got {body}");
    assert_eq!(
        upcoming_ids(&body),
        vec![rf_net],
        "?kind=hf finds the HF net and not the EchoLink one"
    );
}

#[tokio::test]
async fn kind_and_band_are_satisfied_by_one_connection_not_by_two_different_ones() {
    // Same two-colour structure as the band/mode sibling above: the
    // PRESENCE half (`?kind=repeater&band=2m&mode=fm`) is the behaviour; the
    // ABSENCE half (`?kind=echolink&band=20m`) refuses the implementation that
    // gives `kind` its own EXISTS — which would return a net whose EchoLink
    // way and whose 20m way are different connections, describing no way to
    // reach it at all.
    let app = test_app().await;
    let cookie = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;
    let (two_ways, _) = multi_connection_net(
        &app,
        &cookie,
        "Two Ways Net",
        json!([rf("hf", 14_230_000, "20m", "ssb"), echolink("12345")]),
    )
    .await;
    let (repeater_net, _) = multi_connection_net(
        &app,
        &cookie,
        "Repeater Net",
        json!([rf("repeater", 145_230_000, "2m", "fm")]),
    )
    .await;

    let (status, present) = send_json(
        app.router(),
        "GET",
        "/api/discovery?kind=repeater&band=2m&mode=fm",
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "got {present}");
    assert_eq!(
        upcoming_ids(&present),
        vec![repeater_net],
        "one repeater connection is 2m AND fm, so it satisfies all three"
    );

    let (status, absent) = send_json(
        app.router(),
        "GET",
        "/api/discovery?kind=echolink&band=20m",
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "got {absent}");
    // The EMPTY set, not merely "without two_ways": the repeater net has neither
    // an EchoLink way nor a 20m way, so anything at all here is a wrong row.
    assert!(
        upcoming_ids(&absent).is_empty(),
        "no ONE connection is both an EchoLink node and a 20m way — {two_ways} in particular must be absent; got {absent}"
    );
}

#[tokio::test]
async fn a_row_returned_by_a_kind_filter_names_the_connection_that_matched() {
    // In BOTH directions, because the kind predicate lives in TWO guarded
    // places in one query — the matched-connection subquery and the EXISTS —
    // and each half fails silently on its own: subquery-only narrows nothing,
    // EXISTS-only marks nothing. So one assertion per half.
    let app = test_app().await;
    let cookie = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;
    let (id, connection_ids) = multi_connection_net(
        &app,
        &cookie,
        "Three Ways Net",
        json!([
            rf("hf", 14_230_000, "20m", "ssb"),
            echolink("12345"),
            rf("repeater", 145_230_000, "2m", "fm"),
        ]),
    )
    .await;
    let (hf_only, _) = multi_connection_net(
        &app,
        &cookie,
        "HF Only Net",
        json!([rf("hf", 7_200_000, "40m", "ssb")]),
    )
    .await;

    let (status, filtered) = send_json(
        app.router(),
        "GET",
        "/api/discovery?kind=echolink",
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "got {filtered}");
    // The MARK half: the EXISTS alone leaves this null.
    let row = upcoming_row(&filtered, &id).expect("the echolink filter finds the net");
    assert_eq!(
        row["matchedConnectionId"].as_str(),
        Some(connection_ids[1].as_str()),
        "the named connection is the EchoLink one — the id the write echoed back"
    );
    // The NARROWING half: the subquery alone leaves this present.
    assert!(
        upcoming_row(&filtered, &hf_only).is_none(),
        "the HF-only net is absent from upcoming entirely, not present-and-unmarked"
    );

    // Two matching connections: the lower position is named, as for band.
    // Inherits `when_two_connections_match_the_named_one_is_the_lower_position`'s
    // guard on the shared `ORDER BY c.position`; this asserts the kind path
    // reaches that same subquery rather than re-proving the ordering.
    let (two_nodes, two_node_ids) = multi_connection_net(
        &app,
        &cookie,
        "Two Nodes Net",
        json!([
            rf("hf", 14_230_000, "20m", "ssb"),
            echolink("11111"),
            echolink("22222")
        ]),
    )
    .await;
    let (_, filtered) = send_json(
        app.router(),
        "GET",
        "/api/discovery?kind=echolink",
        None,
        None,
    )
    .await;
    let row = upcoming_row(&filtered, &two_nodes).expect("found");
    assert_eq!(
        row["matchedConnectionId"].as_str(),
        Some(two_node_ids[1].as_str()),
        "the lower-position EchoLink node is the one named"
    );

    let (_, unfiltered) = send_json(app.router(), "GET", "/api/discovery", None, None).await;
    let row = upcoming_row(&unfiltered, &id).expect("unfiltered lists the net");
    assert!(
        row.as_object()
            .expect("row object")
            .contains_key("matchedConnectionId"),
        "the key is always present"
    );
    assert!(
        row["matchedConnectionId"].is_null(),
        "no filter was applied, so no connection matched"
    );
}

#[tokio::test]
async fn the_applied_echo_states_an_applied_kind_and_omits_an_unapplied_one() {
    // Key PRESENCE, never `is_null()`: `Value`'s index returns `Null` for
    // a missing key and a null value alike, so an `is_null()` assertion passes
    // before the field exists.
    let app = test_app().await;
    let cookie = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;
    multi_connection_net(
        &app,
        &cookie,
        "EchoLink Only Net",
        json!([echolink("12345")]),
    )
    .await;

    let (status, body) = send_json(
        app.router(),
        "GET",
        "/api/discovery?kind=echolink",
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "got {body}");
    let applied = body["applied"].as_object().expect("applied echo");
    assert!(applied.contains_key("kind"), "an applied kind is stated");
    assert_eq!(applied["kind"], "echolink");

    for uri in [
        "/api/discovery",
        "/api/discovery?kind=",
        "/api/discovery?kind=%20%20",
    ] {
        let (status, body) = send_json(app.router(), "GET", uri, None, None).await;
        assert_eq!(status, StatusCode::OK, "{uri} got {body}");
        let applied = body["applied"].as_object().expect("applied echo");
        assert!(
            !applied.contains_key("kind"),
            "{uri}: an unapplied kind is ABSENT from the echo, never null"
        );
        assert_eq!(
            upcoming_ids(&body).len(),
            1,
            "{uri}: a blank kind is not a filter"
        );
    }
}

#[tokio::test]
async fn an_unknown_kind_is_a_field_level_400() {
    // Four out-of-vocabulary spellings — including `other`, which
    // IS a kind and is refused as a filter by decision — each answer the same
    // field-level problem naming `kind`.
    let app = test_app().await;
    let cookie = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;
    multi_connection_net(
        &app,
        &cookie,
        "EchoLink Only Net",
        json!([echolink("12345")]),
    )
    .await;

    for wrong in ["zzz", "EchoLink", "echo-link", "other"] {
        let (status, content_type, body) =
            get_untyped(app.router(), &format!("/api/discovery?kind={wrong}")).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "?kind={wrong}");
        assert_eq!(content_type, "application/problem+json", "?kind={wrong}");
        let body: Value = serde_json::from_str(&body).expect("problem json");
        assert_eq!(
            body["type"], "/errors/discovery-query-invalid",
            "?kind={wrong}"
        );
        assert!(
            body["detail"]
                .as_str()
                .expect("detail")
                .starts_with("kind:"),
            "?kind={wrong}: the problem detail names the kind field"
        );
        assert!(
            body.get("upcoming").is_none(),
            "?kind={wrong}: a rejection carries no partial result"
        );
    }
}

#[tokio::test]
async fn every_discovery_row_carries_its_own_connection_set_in_the_owners_order() {
    // Three nets with 1, 2 and 3 connections: each row carries ITS OWN set,
    // in position order, with the ids the write echoed back — the shape a
    // per-row query and a mis-keyed batch both get wrong.
    let app = test_app().await;
    let cookie = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;
    let (one_id, one_conns) = multi_connection_net(
        &app,
        &cookie,
        "A One Way Net",
        json!([rf("hf", 14_230_000, "20m", "ssb")]),
    )
    .await;
    let (two_id, two_conns) = multi_connection_net(
        &app,
        &cookie,
        "B Two Way Net",
        json!([
            rf("repeater", 145_230_000, "2m", "fm"),
            { "kind": "echolink", "node": "12345" },
        ]),
    )
    .await;
    let (three_id, three_conns) = multi_connection_net(
        &app,
        &cookie,
        "C Three Way Net",
        json!([
            rf("hf", 7_238_000, "40m", "ssb"),
            { "kind": "dmr", "talkgroup": "31000", "network": "Brandmeister" },
            { "kind": "dstar", "reflector": "REF001 C" },
        ]),
    )
    .await;

    let (status, body) = send_json(app.router(), "GET", "/api/discovery", None, None).await;
    assert_eq!(status, StatusCode::OK);

    for (id, expected_ids, expected_kinds) in [
        (&one_id, &one_conns, vec!["hf"]),
        (&two_id, &two_conns, vec!["repeater", "echolink"]),
        (&three_id, &three_conns, vec!["hf", "dmr", "dstar"]),
    ] {
        let row = upcoming_row(&body, id).expect("row present");
        let connections = row["connections"]
            .as_array()
            .expect("the card carries the connection set");
        assert_eq!(
            connections
                .iter()
                .map(|c| c["id"].as_str().expect("id"))
                .collect::<Vec<_>>(),
            expected_ids.iter().map(String::as_str).collect::<Vec<_>>(),
            "each net's own connections, in the owner's position order"
        );
        assert_eq!(
            connections
                .iter()
                .map(|c| c["kind"].as_str().expect("kind"))
                .collect::<Vec<_>>(),
            expected_kinds
        );
    }
}

#[tokio::test]
async fn a_live_cards_connections_come_from_the_sessions_own_frozen_snapshot() {
    // `list_active_now` reads each live session's own `definition_snapshot`.
    // A session's connections are copied BY VALUE precisely because
    // they may legitimately differ from the definition's on the night: the
    // assertion below edits the DEFINITION mid-run and the card does not move.
    let app = test_app().await;
    let cookie = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;
    let created = create_net(&app, &cookie, json!({ "title": "Live Connections Net" })).await;
    let id = created["id"].as_str().expect("id").to_owned();
    let version = created["definitionVersion"].as_i64().expect("version");
    put_connections(
        &app,
        &cookie,
        &id,
        version,
        json!([
            rf("repeater", 145_230_000, "2m", "fm"),
            { "kind": "allstar", "node": "54321" },
        ]),
    )
    .await;
    start_session(&app, &cookie, &id).await;

    // The owner rewrites the net's ways in WHILE it is running. A card reading
    // the definition would repaint; one reading the session's frozen copy does
    // not, and the second is what an operator on the air actually needs.
    let after_start = sqlx::query_scalar::<_, i32>(
        "SELECT definition_version FROM net_definitions WHERE id = $1",
    )
    .bind(uuid::Uuid::parse_str(&id).expect("uuid"))
    .fetch_one(&app.pool)
    .await
    .expect("definition version");
    put_connections(
        &app,
        &cookie,
        &id,
        after_start as i64,
        json!([{ "kind": "echolink", "node": "99999" }]),
    )
    .await;

    let (status, body) = send_json(app.router(), "GET", "/api/discovery", None, None).await;
    assert_eq!(status, StatusCode::OK);
    let live = body["activeNow"]
        .as_array()
        .expect("activeNow array")
        .iter()
        .find(|r| r["id"].as_str() == Some(id.as_str()))
        .expect("the live session is on the card");
    assert_eq!(
        live["connections"]
            .as_array()
            .expect("the live card carries the connection set")
            .iter()
            .map(|c| c["kind"].as_str().expect("kind"))
            .collect::<Vec<_>>(),
        vec!["repeater", "allstar"],
        "the SESSION's frozen copy, not the definition's rewritten list"
    );
}

#[tokio::test]
async fn a_live_card_shows_the_frequency_a_connection_moved_to_not_the_one_it_started_on() {
    // The snapshot is frozen at start (the test above) but the net is not: a
    // `frequency.changed` moves ONE connection mid-run, and every other session
    // surface overlays that move onto the snapshot before serving it. The live
    // discovery card must answer the same question the same way — a visitor
    // clicking "watch live" tunes to where the net IS, not where it began.
    let app = test_app().await;
    let cookie = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;
    let created = create_net(&app, &cookie, json!({ "title": "QSY Mid-Run Net" })).await;
    let id = created["id"].as_str().expect("id").to_owned();
    let version = created["definitionVersion"].as_i64().expect("version");
    let written = put_connections(
        &app,
        &cookie,
        &id,
        version,
        json!([
            rf("repeater", 145_230_000, "2m", "fm"),
            { "kind": "allstar", "node": "54321" },
        ]),
    )
    .await;
    let connection_ids: Vec<String> = written["connections"]
        .as_array()
        .expect("connections echoed back")
        .iter()
        .map(|c| c["id"].as_str().expect("connection id").to_owned())
        .collect();
    let session_id = start_session(&app, &cookie, &id).await;

    let (status, body) = send_json(
        app.router(),
        "POST",
        &format!("/api/net-sessions/{session_id}/frequency"),
        Some(json!({ "connectionId": connection_ids[0], "operatingFrequency": "146.520" })),
        Some(&cookie),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");

    let (status, body) = send_json(app.router(), "GET", "/api/discovery", None, None).await;
    assert_eq!(status, StatusCode::OK);
    let live = body["activeNow"]
        .as_array()
        .expect("activeNow array")
        .iter()
        .find(|r| r["id"].as_str() == Some(id.as_str()))
        .expect("the live session is on the card");
    let card_connections = live["connections"]
        .as_array()
        .expect("the live card carries the connection set");
    let moved = card_connections
        .iter()
        .find(|c| c["id"].as_str() == Some(connection_ids[0].as_str()))
        .expect("the moved connection is on the card");
    assert_eq!(
        moved["plannedFrequencyHz"], 146_520_000,
        "the card carries the frequency the connection moved TO, not the snapshot's"
    );
    let untouched = card_connections
        .iter()
        .find(|c| c["id"].as_str() == Some(connection_ids[1].as_str()))
        .expect("the unmoved connection is on the card");
    assert!(
        untouched["plannedFrequencyHz"].is_null(),
        "a connection nobody moved is left exactly as the snapshot recorded it"
    );
}

#[tokio::test]
async fn a_retired_sort_token_is_a_200_that_states_the_fallback_on_the_applied_echo() {
    // A `?sort=band` link was valid when somebody shared it.
    // Deleting the enum variant alone turns it into a 400 on a public URL; the
    // parse branch falls back deliberately instead, and the fallback rides the
    // EXISTING `applied` object as one more key rather than a second echo —
    // two echoes on one response is how they disagree.
    let app = test_app().await;
    let cookie = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;
    let created = create_net(&app, &cookie, json!({ "title": "Sunday Traffic Net" })).await;
    let id = created["id"].as_str().expect("id");
    schedule_future(&app, &cookie, id, "2027-01-01T20:00:00Z").await;

    for token in ["band", "mode"] {
        let (status, body) = send_json(
            app.router(),
            "GET",
            &format!("/api/discovery?sort={token}"),
            None,
            None,
        )
        .await;
        assert_eq!(
            status,
            StatusCode::OK,
            "?sort={token} is an already-shared URL and must degrade, never break"
        );
        let applied = body["applied"].as_object().expect("applied echo");
        assert_eq!(applied["sort"], "time", "the ordering actually used");
        // Key PRESENCE, never `is_null()`: `serde_json` yields `Null` for a
        // missing key and for a null value alike, so `is_null()` would pass on
        // a server that never emitted the key at all.
        assert!(
            applied.contains_key("sortUnavailable"),
            "the response must SAY a fallback happened, not just quietly reorder"
        );
        assert_eq!(
            applied["sortUnavailable"], token,
            "and name the sort it could not honour"
        );
        assert!(
            body["upcoming"]
                .as_array()
                .expect("upcoming")
                .iter()
                .any(|r| r["id"].as_str() == Some(id)),
            "a 200 with results, not an empty degraded answer"
        );
    }
}

#[tokio::test]
async fn an_honoured_or_absent_sort_states_no_fallback_at_all() {
    // REGRESSION PIN, expected GREEN on write. Guards the fallback key being
    // emitted unconditionally — a statement that is always there states nothing.
    let app = test_app().await;
    let cookie = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;
    let created = create_net(&app, &cookie, json!({ "title": "Sunday Traffic Net" })).await;
    let id = created["id"].as_str().expect("id");
    schedule_future(&app, &cookie, id, "2027-01-01T20:00:00Z").await;

    for uri in [
        "/api/discovery",
        "/api/discovery?sort=name",
        "/api/discovery?sort=",
    ] {
        let (status, body) = send_json(app.router(), "GET", uri, None, None).await;
        assert_eq!(status, StatusCode::OK);
        assert!(
            !body["applied"]
                .as_object()
                .expect("applied echo")
                .contains_key("sortUnavailable"),
            "{uri} honoured what it was asked for, so there is nothing to state"
        );
    }
}

#[tokio::test]
async fn a_sort_token_that_was_never_a_sort_key_is_still_a_field_level_400() {
    // REGRESSION PIN, expected GREEN on write and green after. It reds on a
    // fall-back-for-everything implementation, which would turn a typo into a
    // silently wrong ordering — wrong data with a 200, and a filter silently
    // ignored.
    let app = test_app().await;
    let cookie = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;
    let created = create_net(&app, &cookie, json!({ "title": "Sunday Traffic Net" })).await;
    let id = created["id"].as_str().expect("id");
    schedule_future(&app, &cookie, id, "2027-01-01T20:00:00Z").await;

    for uri in ["/api/discovery?sort=zzz", "/api/discovery?sort=Time"] {
        let (status, body) = send_json(app.router(), "GET", uri, None, None).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{uri}");
        assert_eq!(body["type"], "/errors/discovery-query-invalid");
        assert!(
            body.get("upcoming").is_none(),
            "{uri} rejects totally — no partial result rides along"
        );
    }
}

/// A `definition_snapshot` of the older shape: flat `band`/`mode` and **no
/// `connections` key**. Planted directly, because no code path can write one
/// any more.
async fn plant_snapshot_without_a_connection_set(app: &TestApp, session_id: &str) {
    sqlx::query("UPDATE net_sessions SET definition_snapshot = $2 WHERE id = $1")
        .bind(uuid::Uuid::parse_str(session_id).expect("uuid"))
        .bind(json!({
            "title": "Sunday Traffic Net",
            "plannedFrequencyHz": 14_230_000_i64,
            "band": "20m",
            "mode": "ssb",
            "netCategory": "traffic",
            "netType": "open"
        }))
        .execute(&app.pool)
        .await
        .expect("plant a snapshot with no connection set");
}

#[tokio::test]
async fn one_unreplayable_live_session_drops_its_card_and_not_the_discovery_page() {
    // `GET /api/discovery` is the PUBLIC landing
    // read, and after this migration EVERY session that already exists is
    // of the older shape — so one of them still `lifecycle = 'live'` used to abort the
    // whole handler, taking `activeNow`, `upcoming` AND `applied` down for every
    // anonymous visitor. The owner could not even clear it by hand, because
    // `close` authorizes through the same refusing read.
    //
    // An unreadable SESSION does not make the LISTING unreadable: the card is
    // dropped, the page answers.
    let app = test_app().await;
    let cookie = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;

    let broken = create_net(&app, &cookie, json!({ "title": "Older Snapshot Net" })).await;
    let broken_id = broken["id"].as_str().expect("id").to_owned();
    let broken_session = start_session(&app, &cookie, &broken_id).await;
    plant_snapshot_without_a_connection_set(&app, &broken_session).await;

    // A CO-RESIDENT healthy live net, without which "the page answers" would be
    // satisfied by a page that answers with nothing at all.
    let healthy = create_net(&app, &cookie, json!({ "title": "Readable Net" })).await;
    let healthy_id = healthy["id"].as_str().expect("id").to_owned();
    start_session(&app, &cookie, &healthy_id).await;

    // And an UPCOMING row, so the assertion also covers the two sections that
    // have nothing to do with the broken session and used to fall with it.
    let scheduled = create_net(&app, &cookie, json!({ "title": "Scheduled Net" })).await;
    let scheduled_id = scheduled["id"].as_str().expect("id").to_owned();
    schedule_future(&app, &cookie, &scheduled_id, "2027-01-01T20:00:00Z").await;

    let (status, body) = send_json(app.router(), "GET", "/api/discovery", None, None).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "the whole page still answers: {body}"
    );

    let active: Vec<&str> = body["activeNow"]
        .as_array()
        .expect("activeNow array")
        .iter()
        .filter_map(|r| r["id"].as_str())
        .collect();
    assert!(
        active.contains(&healthy_id.as_str()),
        "the readable live net is still on the page: {body}"
    );
    assert!(
        !active.contains(&broken_id.as_str()),
        "the unreadable session's CARD is what drops: {body}"
    );
    assert!(
        body["upcoming"]
            .as_array()
            .expect("upcoming array")
            .iter()
            .any(|r| r["id"].as_str() == Some(scheduled_id.as_str())),
        "`upcoming` never depended on that session and must not fall with it: {body}"
    );
    assert!(body["applied"].is_object(), "`applied` answers too: {body}");
}

#[tokio::test]
async fn a_session_that_leaves_live_is_absent_from_active_now_rather_than_a_refusal() {
    // The live rows and the snapshots used to be
    // read by two SEPARATE, non-transactional queries, and the `ok_or_else`
    // between them conflated "not in the second result set" with "unreplayable"
    // — so a session that closed mid-read answered a permanent-sounding 410 to a
    // routine event.
    //
    // The race itself is unrepresentable once the snapshot rides the SAME join,
    // so it cannot be provoked from here. What this pins is the outcome that
    // conflation produced: a session leaving `live` is simply ABSENT, never a
    // refusal. A REGRESSION PIN, green on write, and non-vacuous — dropping the
    // `lifecycle = 'live'` predicate from the join reds the second assertion.
    let app = test_app().await;
    let cookie = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;
    let created = create_net(&app, &cookie, json!({ "title": "Closing Net" })).await;
    let id = created["id"].as_str().expect("id").to_owned();
    let version = created["definitionVersion"].as_i64().expect("version");
    put_connections(
        &app,
        &cookie,
        &id,
        version,
        json!([
            rf("repeater", 145_230_000, "2m", "fm"),
            { "kind": "allstar", "node": "54321" },
        ]),
    )
    .await;
    let session_id = start_session(&app, &cookie, &id).await;

    // Close the session directly in storage, WITHOUT touching the snapshot: the
    // row leaves `lifecycle = 'live'` exactly as a concurrent close would.
    sqlx::query("UPDATE net_sessions SET lifecycle = 'closed', closed_at = now() WHERE id = $1")
        .bind(uuid::Uuid::parse_str(&session_id).expect("uuid"))
        .execute(&app.pool)
        .await
        .expect("close the session under the read");

    let (status, body) = send_json(app.router(), "GET", "/api/discovery", None, None).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "a session leaving `live` is a routine event, not a permanent refusal: {body}"
    );
    assert!(
        !body["activeNow"]
            .as_array()
            .expect("activeNow array")
            .iter()
            .any(|r| r["id"].as_str() == Some(id.as_str())),
        "the closed session is simply not live any more: {body}"
    );
}

// ---------------------------------------------------------------------------
// `q` is matched with `ILIKE`, so a `%`, `_` or `\` a visitor
// types must stay a LITERAL. Each fence seeds the row an UNESCAPED pattern
// would ALSO match (the near-miss), so it fails if the escaping is removed
// rather than staying green because nothing else could have matched.
// ---------------------------------------------------------------------------

/// Six listed, upcoming nets: three wildcard-bearing targets, each beside its
/// near-miss. Returns title -> definition id.
async fn wildcard_fixture(
    app: &TestApp,
    cookie: &str,
) -> std::collections::HashMap<&'static str, String> {
    let titles = [
        "100% Rag", "1009 Rag", // `%` unescaped would match this too
        "A_B Net", "AxB Net", // `_` unescaped would match this too
        "A\\B Net", "AB Net", // `\` unescaped swallows the `B` and matches this instead
    ];
    let mut ids = std::collections::HashMap::new();
    for title in titles {
        let created = create_net(app, cookie, json!({ "title": title })).await;
        let id = created["id"].as_str().expect("id").to_owned();
        schedule_future(app, cookie, &id, "2027-03-01T20:00:00Z").await;
        ids.insert(title, id);
    }
    ids
}

#[tokio::test]
async fn a_percent_in_q_matches_only_a_literal_percent() {
    let app = test_app().await;
    let cookie = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;
    let ids = wildcard_fixture(&app, &cookie).await;

    let (status, body) =
        send_json(app.router(), "GET", "/api/discovery?q=100%25", None, None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        upcoming_ids(&body),
        vec![ids["100% Rag"].clone()],
        "`100%` reaches `100% Rag` and NOT its near-miss `1009 Rag`"
    );
}

#[tokio::test]
async fn an_underscore_in_q_matches_only_a_literal_underscore() {
    let app = test_app().await;
    let cookie = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;
    let ids = wildcard_fixture(&app, &cookie).await;

    let (status, body) = send_json(app.router(), "GET", "/api/discovery?q=A_B", None, None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        upcoming_ids(&body),
        vec![ids["A_B Net"].clone()],
        "`A_B` reaches `A_B Net` and NOT its near-miss `AxB Net`"
    );
}

#[tokio::test]
async fn a_backslash_in_q_matches_only_a_literal_backslash() {
    let app = test_app().await;
    let cookie = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;
    let ids = wildcard_fixture(&app, &cookie).await;

    let (status, body) = send_json(app.router(), "GET", "/api/discovery?q=A%5CB", None, None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        upcoming_ids(&body),
        vec![ids["A\\B Net"].clone()],
        "`A\\B` reaches `A\\B Net` and NOT `AB Net`, which an unescaped `\\B` collapses to"
    );
}

#[tokio::test]
async fn the_applied_echo_carries_the_raw_term_not_the_storage_pattern() {
    // The `%…%` wrapping and escaping are the adapter's
    // business. A client joining the echo back to its own request must see
    // what it sent, never `%100\%%`.
    let app = test_app().await;
    let cookie = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;
    let ids = wildcard_fixture(&app, &cookie).await;

    let (status, body) =
        send_json(app.router(), "GET", "/api/discovery?q=100%25", None, None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["applied"]["q"], "100%");
    assert_eq!(upcoming_ids(&body), vec![ids["100% Rag"].clone()]);

    let (status, body) = send_json(app.router(), "GET", "/api/discovery?q=A%5CB", None, None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["applied"]["q"], "A\\B");
}

#[tokio::test]
async fn a_case_mismatched_q_still_matches_the_title() {
    // There is no `lower()` wrapping folding case on both sides; `ILIKE` and
    // pg_trgm do their own case-insensitive comparison. That is a change in HOW case is folded, not
    // whether it is — pin the behaviour so a future rewrap in `lower()`
    // (which would defeat the trigram index) isn't the only thing that
    // notices.
    let app = test_app().await;
    let cookie = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;
    let created = create_net(&app, &cookie, json!({ "title": "RagChew Net" })).await;
    let id = created["id"].as_str().expect("id").to_owned();
    schedule_future(&app, &cookie, &id, "2027-03-01T20:00:00Z").await;

    let (status, body) =
        send_json(app.router(), "GET", "/api/discovery?q=ragchew", None, None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        upcoming_ids(&body),
        vec![id],
        "a lowercase `q` must still match a differently-cased title: {body}"
    );
}

// ---- The landing says what it left out --------------------------
//
// Both collections were cut in silence — `upcoming` at `DISCOVERY_LIMIT` with no
// signal, `activeNow` not at all. The signal is `applied.truncated`, an array of
// the WIRE collection names that were cut, OMITTED when nothing was (this
// endpoint's omit-null convention, chosen over an always-present array).
//
// ⚠️ The truncation tests below are the ONLY fence on the omit-when-empty key:
// the client reads an absent `truncated` as "nothing was cut" and cannot throw
// on it, so a backend that silently stops sending the key is caught HERE or
// nowhere (Question 2's ruling).

use netroll_app::http::discovery::{ACTIVE_NOW_LIMIT, DISCOVERY_LIMIT};

/// `applied.truncated` as the collection names it carries — EMPTY when the key
/// is absent, which is the wire's "nothing was cut".
fn truncated(body: &Value) -> Vec<String> {
    match &body["applied"]["truncated"] {
        Value::Null => Vec::new(),
        other => other
            .as_array()
            .expect("truncated is an array")
            .iter()
            .map(|c| c.as_str().expect("a collection name").to_owned())
            .collect(),
    }
}

fn active_now_ids(body: &Value) -> Vec<String> {
    body["activeNow"]
        .as_array()
        .expect("activeNow array")
        .iter()
        .map(|r| r["occurrenceId"].as_str().expect("session id").to_owned())
        .collect()
}

/// Seeds `count` FUTURE occurrences on `definition_id` directly, one hour
/// apart. `upcoming` counts OCCURRENCES, so the cap is crossed by one
/// definition with many rows; the spacing is for
/// `UNIQUE (definition_id, scheduled_start_at)`. One set-based INSERT, never a
/// loop of round trips.
async fn seed_upcoming_occurrences(pool: &PgPool, definition_id: &str, count: i64) {
    let ids: Vec<uuid::Uuid> = (0..count).map(|_| uuid::Uuid::now_v7()).collect();
    sqlx::query(
        "INSERT INTO net_occurrences (id, definition_id, scheduled_start_at)
         SELECT u.id, $2, TIMESTAMPTZ '2027-06-01T00:00:00Z' + (u.ord * INTERVAL '1 hour')
           FROM UNNEST($1::uuid[]) WITH ORDINALITY AS u(id, ord)",
    )
    .bind(&ids)
    .bind(uuid::Uuid::parse_str(definition_id).expect("uuid"))
    .execute(pool)
    .await
    .expect("seed occurrences");
}

/// Brings the instance to EXACTLY `total_live` live, Listed sessions and returns
/// their ids in the order the read serves them (`started_at ASC`).
///
/// `activeNow` counts LIVE SESSIONS and `net_sessions_one_live_per_definition`
/// allows one per definition, so N cards need N definitions — impossible over
/// HTTP past `NET_CREATION_BURST` and the per-user net cap. ONE session is
/// started over HTTP so its real `definition_snapshot` can be cloned onto every
/// seeded row (a snapshot whose `connections` does not decode drops its card
/// and would make a truncation test pass or fail for the wrong reason); the
/// other `total_live - 1` are three set-based INSERTs sharing one ordinal, the
/// shape `api_admin.rs`'s account seeder takes. The seeded rows start in
/// January and the template starts NOW, so the template orders LAST.
///
/// `ids` lets a caller hand in the seeded session ids (to control the id order
/// of a pair that will share a `started_at`); `None` mints them ascending.
async fn seed_live_listed_sessions(
    app: &TestApp,
    total_live: i64,
    ids: Option<Vec<uuid::Uuid>>,
) -> Vec<uuid::Uuid> {
    let cookie = sign_in_consent_callsign(app, "template@example.com", "w1tpl").await;
    let template = create_net(app, &cookie, json!({ "title": "Snapshot Template Net" })).await;
    let template_id = template["id"].as_str().expect("id").to_owned();
    let template_session = start_session(app, &cookie, &template_id).await;
    let template_session = uuid::Uuid::parse_str(&template_session).expect("uuid");

    let seeded = total_live - 1;
    let session_ids: Vec<uuid::Uuid> =
        ids.unwrap_or_else(|| (0..seeded).map(|_| uuid::Uuid::now_v7()).collect());
    assert_eq!(
        session_ids.len() as i64,
        seeded,
        "one id per seeded session"
    );
    let definition_ids: Vec<uuid::Uuid> = (0..seeded).map(|_| uuid::Uuid::now_v7()).collect();
    let connection_ids: Vec<uuid::Uuid> = (0..seeded).map(|_| uuid::Uuid::now_v7()).collect();

    sqlx::query(
        "INSERT INTO net_definitions (id, title, net_category, net_type, link_token)
         SELECT u.id, 'Seeded Live Net ' || u.ord, 'traffic', 'open',
                md5(u.id::text) || md5(u.id::text || 'link')
           FROM UNNEST($1::uuid[]) WITH ORDINALITY AS u(id, ord)",
    )
    .bind(&definition_ids)
    .execute(&app.pool)
    .await
    .expect("seed definitions");
    sqlx::query(
        "INSERT INTO net_connections
                (id, definition_id, position, kind, planned_frequency_hz, band, mode)
         SELECT t.connection_id, t.definition_id, 0, 'hf', 14230000, '20m', 'ssb'
           FROM UNNEST($1::uuid[], $2::uuid[]) AS t(connection_id, definition_id)",
    )
    .bind(&connection_ids)
    .bind(&definition_ids)
    .execute(&app.pool)
    .await
    .expect("seed connections");
    sqlx::query(
        "INSERT INTO net_sessions
                (id, definition_id, definition_version, definition_snapshot, lifecycle, started_at)
         SELECT t.session_id, t.definition_id, 1,
                (SELECT definition_snapshot FROM net_sessions WHERE id = $3),
                'live',
                TIMESTAMPTZ '2026-01-01T00:00:00Z' + (t.ord * INTERVAL '1 second')
           FROM UNNEST($1::uuid[], $2::uuid[]) WITH ORDINALITY AS t(session_id, definition_id, ord)",
    )
    .bind(&session_ids)
    .bind(&definition_ids)
    .bind(template_session)
    .execute(&app.pool)
    .await
    .expect("seed live sessions");

    let mut all = session_ids;
    all.push(template_session);
    all
}

#[tokio::test]
async fn more_upcoming_than_the_cap_are_served_to_the_cap_and_the_cut_is_stated() {
    // The assertion is on the SIGNAL, never on the count as a literal: the
    // constant is imported, and will move.
    let app = test_app().await;
    let cookie = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;
    let created = create_net(&app, &cookie, json!({ "title": "Busy Net" })).await;
    let id = created["id"].as_str().expect("id");
    seed_upcoming_occurrences(&app.pool, id, DISCOVERY_LIMIT + 1).await;

    let (status, body) = send_json(app.router(), "GET", "/api/discovery", None, None).await;
    assert_eq!(status, StatusCode::OK, "got {body}");
    assert_eq!(
        truncated(&body),
        vec!["upcoming".to_owned()],
        "the cut collection is named: {}",
        body["applied"]
    );
    assert_eq!(
        upcoming_ids(&body).len() as i64,
        DISCOVERY_LIMIT,
        "the cap's worth of real rows is still served"
    );
}

#[tokio::test]
async fn exactly_the_cap_of_upcoming_is_complete_and_states_no_cut() {
    // The trap, arriving here too. A collection matching EXACTLY
    // the cap is COMPLETE; `rows.len() == limit` would call it cut. Green from
    // the start — its job is to red under that mutation.
    let app = test_app().await;
    let cookie = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;
    let created = create_net(&app, &cookie, json!({ "title": "Full Net" })).await;
    let id = created["id"].as_str().expect("id");
    seed_upcoming_occurrences(&app.pool, id, DISCOVERY_LIMIT).await;

    let (status, body) = send_json(app.router(), "GET", "/api/discovery", None, None).await;
    assert_eq!(status, StatusCode::OK, "got {body}");
    assert!(
        body["applied"].get("truncated").is_none(),
        "exactly the cap is complete, and the key is OMITTED, not empty: {}",
        body["applied"]
    );
    assert_eq!(upcoming_ids(&body).len() as i64, DISCOVERY_LIMIT);
}

#[tokio::test]
async fn more_live_sessions_than_the_bound_are_served_to_the_bound_and_the_cut_is_stated() {
    // Today `list_active_now` has no LIMIT at all, so every live Listed
    // session is served and nothing is said.
    let app = test_app().await;
    let live = seed_live_listed_sessions(&app, ACTIVE_NOW_LIMIT + 1, None).await;

    let (status, body) = send_json(app.router(), "GET", "/api/discovery", None, None).await;
    assert_eq!(status, StatusCode::OK, "got {body}");
    assert_eq!(
        truncated(&body),
        vec!["activeNow".to_owned()],
        "the cut collection is named: {}",
        body["applied"]
    );
    let served = active_now_ids(&body);
    assert_eq!(
        served.len() as i64,
        ACTIVE_NOW_LIMIT,
        "at most the bound's worth of cards"
    );
    // The bound falls at the END of the ordering: the earliest-started cards
    // are the ones served, and the one cut is the last.
    let expected: Vec<String> = live[..ACTIVE_NOW_LIMIT as usize]
        .iter()
        .map(ToString::to_string)
        .collect();
    assert_eq!(
        served, expected,
        "served in `started_at` order, oldest first"
    );
}

#[tokio::test]
async fn exactly_the_bound_of_live_sessions_is_complete_and_states_no_cut() {
    // The exactly-at-the-bound case: the same trap as `upcoming`'s.
    let app = test_app().await;
    let live = seed_live_listed_sessions(&app, ACTIVE_NOW_LIMIT, None).await;

    let (status, body) = send_json(app.router(), "GET", "/api/discovery", None, None).await;
    assert_eq!(status, StatusCode::OK, "got {body}");
    assert!(
        body["applied"].get("truncated").is_none(),
        "exactly the bound is complete, and the key is OMITTED: {}",
        body["applied"]
    );
    assert_eq!(
        active_now_ids(&body).len(),
        live.len(),
        "every live card is served"
    );
}

#[tokio::test]
async fn both_collections_cut_are_both_named_in_envelope_order() {
    // The value is the list of cut collections, `activeNow` before
    // `upcoming` — the envelope's own field order.
    let app = test_app().await;
    seed_live_listed_sessions(&app, ACTIVE_NOW_LIMIT + 1, None).await;
    let cookie = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;
    let created = create_net(&app, &cookie, json!({ "title": "Busy Net" })).await;
    seed_upcoming_occurrences(
        &app.pool,
        created["id"].as_str().expect("id"),
        DISCOVERY_LIMIT + 1,
    )
    .await;

    let (status, body) = send_json(app.router(), "GET", "/api/discovery", None, None).await;
    assert_eq!(status, StatusCode::OK, "got {body}");
    assert_eq!(
        truncated(&body),
        vec!["activeNow".to_owned(), "upcoming".to_owned()],
        "{}",
        body["applied"]
    );
}

#[tokio::test]
async fn two_live_sessions_sharing_a_started_at_at_the_boundary_are_ordered_by_id() {
    // `ORDER BY s.started_at ASC` alone leaves two sessions that started in
    // the same instant in arbitrary order — invisible until a bound makes the
    // boundary observable. `(started_at, id)` is total: the LOWER id is served
    // and the higher is cut, whatever order the heap holds them in.
    //
    // The pair is inserted LOWER id first, and that orientation is MEASURED, not
    // assumed: with `s.id` dropped from the
    // ORDER BY, Postgres's bounded sort served the physically LATER row of the
    // tie, so lower-first is the orientation under which the tiebreak-less
    // query serves the wrong one and this test reds. Higher-first stayed green
    // under the same mutation. The tie order without the tiebreak is a sort
    // internal, not a contract — which is the whole reason the tiebreak exists.
    //
    // bound + 2 live in all: the template orders last, so the SEEDED rows fill
    // positions 1..=bound+1 and the pair at the end of them is positions bound
    // and bound + 1 — one served, one cut.
    let app = test_app().await;
    let seeded = ACTIVE_NOW_LIMIT as usize + 1;
    let ids: Vec<uuid::Uuid> = (0..seeded).map(|_| uuid::Uuid::now_v7()).collect();
    let lower = ids[seeded - 2];
    let higher = ids[seeded - 1];
    assert!(lower < higher, "UUIDv7 ids mint ascending");
    seed_live_listed_sessions(&app, ACTIVE_NOW_LIMIT + 2, Some(ids)).await;
    sqlx::query(
        "UPDATE net_sessions SET started_at = (SELECT started_at FROM net_sessions WHERE id = $1)
          WHERE id = $2",
    )
    .bind(lower)
    .bind(higher)
    .execute(&app.pool)
    .await
    .expect("share the instant");

    // The PRECONDITION, asserted directly (idiom): a reshuffle that
    // stops the pair straddling the boundary would prove nothing.
    let distinct_instants: i64 = sqlx::query_scalar(
        "SELECT COUNT(DISTINCT started_at) FROM net_sessions WHERE id = ANY($1)",
    )
    .bind(vec![lower, higher])
    .fetch_one(&app.pool)
    .await
    .expect("count instants");
    assert_eq!(
        distinct_instants, 1,
        "the pair really does share a started_at"
    );
    let rank_of_pair: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM net_sessions
          WHERE lifecycle = 'live'
            AND started_at < (SELECT started_at FROM net_sessions WHERE id = $1)",
    )
    .bind(lower)
    .fetch_one(&app.pool)
    .await
    .expect("rank");
    assert_eq!(
        rank_of_pair + 2,
        ACTIVE_NOW_LIMIT + 1,
        "the pair sits exactly across the bound's boundary"
    );

    let (status, body) = send_json(app.router(), "GET", "/api/discovery", None, None).await;
    assert_eq!(status, StatusCode::OK, "got {body}");
    let served = active_now_ids(&body);
    assert!(
        served.contains(&lower.to_string()),
        "the lower id of the tied pair is served"
    );
    assert!(
        !served.contains(&higher.to_string()),
        "the higher id of the tied pair is the one cut"
    );
}

#[tokio::test]
async fn a_dropped_live_card_inside_the_served_window_cannot_make_active_now_read_complete() {
    // ONE fixture, deliberately: bound + 1 live sessions with the
    // EARLIEST-ordered one unreadable, so the dropped row sits INSIDE the
    // served window rather than past it.
    //
    // bound applied in SQL, split on the RAW rows, then the replay loop:
    // bound + 1 raw → cut to bound (the last never enters the loop) → the
    // damaged first row drops → bound − 1 served, `activeNow` named.
    // bound applied AFTER the fold, or split after the skip:
    // all bound + 1 replayed → damaged row drops → bound survivors → served
    // bound, and a flag computed on the survivors reads COMPLETE — the very
    // silence to avoid, re-created by the fix.
    //
    // The two counts differ and the flags differ; that PAIR is the assertion.
    // A served-count assertion alone cannot tell a SQL LIMIT from a
    // `.truncate()` after the fold, so this test does not claim the count
    // proves where the bound landed — the pair does.
    let app = test_app().await;
    let live = seed_live_listed_sessions(&app, ACTIVE_NOW_LIMIT + 1, None).await;
    plant_snapshot_without_a_connection_set(&app, &live[0].to_string()).await;

    let (status, body) = send_json(app.router(), "GET", "/api/discovery", None, None).await;
    assert_eq!(status, StatusCode::OK, "got {body}");
    let served = active_now_ids(&body);
    assert_eq!(
        truncated(&body),
        vec!["activeNow".to_owned()],
        "a dropped card never reads as an exhausted list: {}",
        body["applied"]
    );
    assert_eq!(
        served.len() as i64,
        ACTIVE_NOW_LIMIT - 1,
        "the served list comes back SHORT of the bound, and that is the correct answer"
    );
    assert!(
        !served.contains(&live[0].to_string()),
        "the unreadable card is the one that dropped"
    );
    assert!(
        !served.contains(&live[ACTIVE_NOW_LIMIT as usize].to_string()),
        "the (bound + 1)th row was never promoted onto the page"
    );
}

#[tokio::test]
async fn a_dropped_upcoming_row_inside_the_served_window_cannot_make_upcoming_read_complete() {
    // The `upcoming` half. `attach_connections` drops a definition with no
    // connection rows AFTER the query. Cap + 1 raw rows with the damaged one
    // ordered FIRST: split on the raw rows and the cut is stated with cap − 1
    // served; split after the skip and the probe row is promoted onto a page
    // of exactly the cap, which reads complete.
    let app = test_app().await;
    let cookie = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;
    let healthy = create_net(&app, &cookie, json!({ "title": "Healthy Net" })).await;
    let damaged = create_net(&app, &cookie, json!({ "title": "Damaged Net" })).await;
    let healthy_id = healthy["id"].as_str().expect("id").to_owned();
    let damaged_id = damaged["id"].as_str().expect("id").to_owned();
    // The damaged row orders FIRST: January against the seeder's June.
    schedule_future(&app, &cookie, &damaged_id, "2027-01-01T20:00:00Z").await;
    seed_upcoming_occurrences(&app.pool, &healthy_id, DISCOVERY_LIMIT).await;
    sqlx::query("DELETE FROM net_connections WHERE definition_id = $1")
        .bind(uuid::Uuid::parse_str(&damaged_id).expect("uuid"))
        .execute(&app.pool)
        .await
        .expect("remove every connection row");

    let (status, body) = send_json(app.router(), "GET", "/api/discovery", None, None).await;
    assert_eq!(status, StatusCode::OK, "got {body}");
    assert_eq!(
        truncated(&body),
        vec!["upcoming".to_owned()],
        "a dropped row never reads as an exhausted list: {}",
        body["applied"]
    );
    let ids = upcoming_ids(&body);
    assert_eq!(
        ids.len() as i64,
        DISCOVERY_LIMIT - 1,
        "short of the cap, and that is the correct answer"
    );
    assert!(
        !ids.contains(&damaged_id),
        "the damaged definition is the one that dropped"
    );
}
