// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
//! Full-loop API tests for the profile check-in-history widget: real router,
//! real Postgres (testcontainers).
//!
//! The endpoint returns ONLY the caller's own SELF check-ins; a staff-entered
//! add of the caller's callsign belongs to nobody's history. Asserts behaviour.

use std::sync::{Arc, Mutex};

use axum::Router;
use axum::body::Body;
use axum::http::{HeaderMap, Request, StatusCode, header};
use netroll_app::http::{AppState, api_router};
use netroll_domain::callsign::parse_callsign;
use netroll_domain::check_in::{CheckInSource, StayingStatus};
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

/// The exact wire contract of one history item. Widening this set is the
/// failure mode the redaction test exists to catch.
const ITEM_FIELDS: [&str; 7] = [
    "netSessionId",
    "netTitle",
    "band",
    "mode",
    // WHICH way in this check-in arrived on, as its LABEL. `band`
    // and `mode` are now derived from it rather than from the net's first
    // connection, and a way in reached by NAME — an EchoLink node, a talkgroup,
    // a reflector — carries neither, so this is the field such a row has to
    // speak with. Still stable at add time, which is this read's selection rule.
    "via",
    "callsign",
    "checkedInAt",
];

/// Keys that must NEVER appear on an item: `addedBy`/`actorId` are redacted
/// everywhere on the public wire, `definitionId`/`linkToken` would let a caller
/// walk to an unlisted definition, and the rest are operator-editable fields
/// this read cannot keep current (it folds no `checkin.updated`).
const FORBIDDEN_ITEM_FIELDS: [&str; 10] = [
    "addedBy",
    "actorId",
    "definitionId",
    "linkToken",
    "signalReport",
    "staying",
    "name",
    "location",
    "notes",
    "email",
];

#[derive(Default)]
struct CapturingMailer {
    magic_links: Mutex<Vec<String>>,
}

impl Mailer for CapturingMailer {
    fn send_magic_link<'a>(
        &'a self,
        _to: &'a str,
        link: &'a str,
    ) -> BoxFuture<'a, Result<(), MailError>> {
        Box::pin(async move {
            self.magic_links.lock().expect("lock").push(link.to_owned());
            Ok(())
        })
    }
    fn send_email_change<'a>(
        &'a self,
        _to: &'a str,
        _link: &'a str,
    ) -> BoxFuture<'a, Result<(), MailError>> {
        Box::pin(async move { Ok(()) })
    }
    fn send_email_change_notice<'a>(
        &'a self,
        _to: &'a str,
        _new_email: &'a str,
    ) -> BoxFuture<'a, Result<(), MailError>> {
        Box::pin(async move { Ok(()) })
    }
}

impl CapturingMailer {
    fn last_link(&self) -> String {
        self.magic_links
            .lock()
            .expect("lock")
            .last()
            .expect("a magic link was sent")
            .clone()
    }
}

struct TestApp {
    _container: ContainerAsync<Postgres>,
    state: AppState,
    mailer: Arc<CapturingMailer>,
    _pool: PgPool,
}

impl TestApp {
    fn router(&self) -> Router {
        api_router(self.state.clone())
    }
}

fn now_millis() -> u64 {
    1_800_000_000_000
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
        _pool: pool,
    }
}

/// Signs in `email` (magic-link → session), returning the session cookie and the
/// account id. The history is session-gated but NOT consent-gated, so no consent
/// step is needed — the same carve-out the export uses.
async fn sign_in(app: &TestApp, email: &str) -> (String, Uuid) {
    let (status, _, _) = send_raw(
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
    let (status, headers, account) = send_raw(
        app.router(),
        "POST",
        "/api/sessions",
        Some(json!({ "token": token })),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    let cookie = headers
        .get(header::SET_COOKIE)
        .expect("cookie")
        .to_str()
        .expect("ascii")
        .split(';')
        .next()
        .expect("pair")
        .to_owned();
    let id = account["id"].as_str().expect("id").parse().expect("uuid");
    (cookie, id)
}

async fn send_raw(
    router: Router,
    method: &str,
    uri: &str,
    body: Option<Value>,
    cookie: Option<&str>,
) -> (StatusCode, HeaderMap, Value) {
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
        .expect("body")
        .to_vec();
    let json = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).expect("body is JSON")
    };
    (status, headers, json)
}

/// GET returning status + `content-type` + the RAW body bytes.
///
/// An extractor rejection's body is not necessarily JSON, so the media type has
/// to be asserted BEFORE any parse is attempted — [`send_raw`] would panic on a
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
    let request = builder.body(Body::empty()).expect("build request");
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

/// The one connection every fixture net is born with: a
/// definition carries no connection fact of its own, so `create` takes the set
/// as an argument. Fully qualified because this file already imports the
/// connection types locally where it builds `via` fixtures.
fn sample_connections() -> netroll_domain::net::connection::NetConnectionSet {
    netroll_domain::net::connection::NetConnectionSet::new(vec![
        netroll_domain::net::connection::NetConnection {
            id: uuid::Uuid::now_v7(),
            position: 0,
            kind: netroll_domain::net::connection::NetConnectionKind::Hf {
                planned_frequency_hz: 14_230_000,
                band: netroll_domain::net::enums::Band::TwentyMeters,
                mode: netroll_domain::net::enums::Mode::Ssb,
            },
        },
    ])
    .expect("one connection is a valid set")
}

fn sample_fields(title: &str) -> NetDefinitionFields {
    parse_net_definition_fields(RawNetDefinition {
        title: Some(title.to_owned()),
        description: Some("Weekly NTS".to_owned()),
        country: Some("USA".to_owned()),
        state: Some("CT".to_owned()),
        grid: Some("fn31pr".to_owned()),
        net_category: Some("traffic".to_owned()),
        net_type: Some("open".to_owned()),
        expected_duration: Some("90".to_owned()),
        visibility: None,
    })
    .expect("valid fields")
}

/// Starts a live session on a fresh net owned by `owner`, returning its id.
async fn live_session(app: &TestApp, owner: Uuid, title: &str, token: &str) -> Uuid {
    use netroll_adapters::pg::net_sessions::StartOutcome;
    let def = app
        .state
        .net_definitions
        .create(
            &sample_fields(title),
            &sample_connections(),
            owner,
            token,
            now_millis(),
        )
        .await
        .expect("create net");
    match app
        .state
        .net_sessions
        .start(&def, Some(owner), now_millis())
        .await
        .expect("start session")
    {
        StartOutcome::Started(row) => row.id,
        other => panic!("expected Started, got {other:?}"),
    }
}

/// Starts a live session on a CROSS-MODE net — HF first, EchoLink second — so a
/// per-check-in `via` can be told apart from "the net's first connection", which
/// is exactly what a single-connection fixture cannot do.
async fn cross_mode_session(
    app: &TestApp,
    owner: Uuid,
    title: &str,
    token: &str,
) -> netroll_adapters::pg::net_sessions::NetSessionRow {
    use netroll_adapters::pg::net_sessions::StartOutcome;
    use netroll_domain::net::connection::{NetConnection, NetConnectionKind, NetConnectionSet};
    use netroll_domain::net::enums::{Band, Mode};

    let mut def = app
        .state
        .net_definitions
        .create(
            &sample_fields(title),
            &sample_connections(),
            owner,
            token,
            now_millis(),
        )
        .await
        .expect("create net");
    def.connections = NetConnectionSet::new(vec![
        NetConnection {
            id: Uuid::now_v7(),
            position: 0,
            kind: NetConnectionKind::Hf {
                planned_frequency_hz: 14_230_000,
                band: Band::TwentyMeters,
                mode: Mode::Ssb,
            },
        },
        NetConnection {
            id: Uuid::now_v7(),
            position: 1,
            kind: NetConnectionKind::EchoLink {
                node: "12345".to_owned(),
            },
        },
    ])
    .expect("two connections are a valid set");
    match app
        .state
        .net_sessions
        .start(&def, Some(owner), now_millis())
        .await
        .expect("start session")
    {
        StartOutcome::Started(row) => *row,
        other => panic!("expected Started, got {other:?}"),
    }
}

/// Appends one check-in through the real repo path.
async fn check_in(
    app: &TestApp,
    session: Uuid,
    callsign: &str,
    source: CheckInSource,
    actor: Uuid,
    at_millis: u64,
) {
    use netroll_adapters::pg::net_sessions::AddCheckInOutcome;
    let call = parse_callsign(callsign).expect("valid callsign");
    let outcome = app
        .state
        .net_sessions
        .add_check_in(
            session,
            &call,
            Uuid::now_v7(),
            None,
            None,
            StayingStatus::InAndOut,
            None,
            None,
            None,
            source,
            None,
            None,
            Some(actor),
            at_millis,
        )
        .await
        .expect("add check-in");
    assert!(matches!(outcome, AddCheckInOutcome::Added(_)));
}

/// The `netSessionId` values of one page, in wire order.
fn session_ids(body: &Value) -> Vec<String> {
    body["items"]
        .as_array()
        .expect("items array")
        .iter()
        .map(|i| i["netSessionId"].as_str().expect("id").to_owned())
        .collect()
}

/// Walks the endpoint to exhaustion at `limit`, collecting `netSessionId`s.
async fn walk(app: &TestApp, cookie: &str, limit: u32) -> Vec<String> {
    let mut out = Vec::new();
    let mut cursor: Option<String> = None;
    loop {
        let uri = match &cursor {
            None => format!("/api/accounts/me/check-ins?limit={limit}"),
            Some(c) => format!(
                "/api/accounts/me/check-ins?limit={limit}&cursor={}",
                urlencoding_light(c)
            ),
        };
        let (status, _, body) = send_raw(app.router(), "GET", &uri, None, Some(cookie)).await;
        assert_eq!(status, StatusCode::OK);
        out.extend(session_ids(&body));
        match body["nextCursor"].as_str() {
            Some(next) => cursor = Some(next.to_owned()),
            None => break,
        }
    }
    out
}

/// Percent-encodes the one reserved character an opaque cursor can contain.
fn urlencoding_light(raw: &str) -> String {
    raw.replace(':', "%3A")
}

#[tokio::test]
async fn the_history_returns_the_callers_own_self_check_ins_with_the_page_envelope() {
    let app = test_app().await;
    let (cookie, account) = sign_in(&app, "history-envelope@example.com").await;
    let session = live_session(&app, account, "Checkin Net", "tok-envelope").await;
    check_in(
        &app,
        session,
        "W1AW",
        CheckInSource::SelfService,
        account,
        now_millis(),
    )
    .await;

    let (status, headers, body) = send_raw(
        app.router(),
        "GET",
        "/api/accounts/me/check-ins?limit=10",
        None,
        Some(&cookie),
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    // A browsable widget read, NOT the export: plain JSON, no attachment header.
    assert!(
        headers
            .get(header::CONTENT_TYPE)
            .expect("content-type")
            .to_str()
            .expect("ascii")
            .starts_with("application/json")
    );
    assert!(headers.get(header::CONTENT_DISPOSITION).is_none());

    let items = body["items"].as_array().expect("items array");
    assert_eq!(items.len(), 1);
    assert_eq!(items[0]["netSessionId"], session.to_string());
    assert_eq!(items[0]["netTitle"], "Checkin Net");
    // `band`/`mode` are the connection THIS check-in came in on, and this
    // fixture records no `via` — so they are NULL rather than the net's first
    // connection's. Reporting position zero's band for a check-in nobody
    // recorded a way in for is the substitution the story exists to remove; the
    // one permitted fallback is the ADIF export's, and it is asserted as a
    // fallback where it lives.
    // `contains_key` first, then the null: `Value::Null` is what a MISSING key
    // indexes to, so `is_null()` alone passes for a field that was never
    // serialized — the diff's own convention everywhere else, applied here too.
    let item = items[0].as_object().expect("an item object");
    for key in ["band", "mode", "via"] {
        assert!(item.contains_key(key), "the item serialized `{key}`");
        assert!(item[key].is_null(), "and `{key}` is null, not borrowed");
    }
    assert_eq!(items[0]["callsign"], "W1AW");
    assert!(items[0]["checkedInAt"].is_string());
    // `nextCursor` is present and explicitly null on the last page — clients
    // branch on it, so it is the one deliberate exception to omit-when-absent.
    assert!(
        body.as_object().expect("object").contains_key("nextCursor"),
        "the envelope always carries nextCursor"
    );
    assert!(body["nextCursor"].is_null());
}

#[tokio::test]
async fn a_one_row_per_page_walk_returns_every_check_in_newest_first() {
    // The keyset query is now a subquery with a LATERAL
    // outside it, and the INNER `ORDER BY … DESC` decides which rows land on a
    // page while the outer one only re-sorts that page. A fixture with fewer
    // rows than `limit + 1` cannot see the inner order at all — every row is
    // over-fetched whatever it says — so this one has THREE rows and walks at
    // `limit=1`: an inner ASC would page the two oldest first and the cursor
    // would then step off the end, dropping the newest row from the walk.
    let app = test_app().await;
    let (cookie, account) = sign_in(&app, "walker@example.com").await;
    let mut expected_newest_first = Vec::new();
    for (title, token, at) in [
        ("Oldest Net", "tok-walk-1", 1_000u64),
        ("Middle Net", "tok-walk-2", 2_000),
        ("Newest Net", "tok-walk-3", 3_000),
    ] {
        let session = live_session(&app, account, title, token).await;
        check_in(
            &app,
            session,
            "W1AW",
            CheckInSource::SelfService,
            account,
            at,
        )
        .await;
        expected_newest_first.insert(0, session.to_string());
    }

    let walked = walk(&app, &cookie, 1).await;
    assert_eq!(
        walked, expected_newest_first,
        "three pages of one row each, newest first, no gaps and no repeats"
    );
}

#[tokio::test]
async fn one_account_never_sees_another_accounts_check_ins() {
    // THE HTTP PRIVACY TEST. Two accounts, two cookies, self check-ins
    // interleaved in a SHARED session — and the isolation is re-proven under a
    // limit=1 cursor walk, because a leak that only appears on page two is
    // exactly what a single-page assertion misses.
    let app = test_app().await;
    let (cookie_a, a) = sign_in(&app, "iso-a@example.com").await;
    let (cookie_b, b) = sign_in(&app, "iso-b@example.com").await;

    let shared = live_session(&app, a, "Shared Net", "tok-iso-shared").await;
    let a_only = live_session(&app, a, "A Only Net", "tok-iso-a").await;
    let b_only = live_session(&app, b, "B Only Net", "tok-iso-b").await;

    let t = now_millis();
    check_in(&app, shared, "N1CCK", CheckInSource::SelfService, a, t + 10).await;
    check_in(&app, shared, "K2ABC", CheckInSource::SelfService, b, t + 20).await;
    check_in(&app, a_only, "N1CCK", CheckInSource::SelfService, a, t + 30).await;
    check_in(&app, b_only, "K2ABC", CheckInSource::SelfService, b, t + 40).await;

    let (_, _, a_body) = send_raw(
        app.router(),
        "GET",
        "/api/accounts/me/check-ins",
        None,
        Some(&cookie_a),
    )
    .await;
    let (_, _, b_body) = send_raw(
        app.router(),
        "GET",
        "/api/accounts/me/check-ins",
        None,
        Some(&cookie_b),
    )
    .await;

    let a_sessions: std::collections::HashSet<String> = session_ids(&a_body).into_iter().collect();
    let b_sessions: std::collections::HashSet<String> = session_ids(&b_body).into_iter().collect();
    assert_eq!(
        a_sessions,
        [shared.to_string(), a_only.to_string()]
            .into_iter()
            .collect(),
    );
    assert_eq!(
        b_sessions,
        [shared.to_string(), b_only.to_string()]
            .into_iter()
            .collect(),
    );
    assert!(
        !a_sessions.contains(&b_only.to_string()),
        "A must never see B's session"
    );
    assert!(
        !b_sessions.contains(&a_only.to_string()),
        "B must never see A's session"
    );

    // The same isolation under a full one-row-per-page walk.
    let a_walk = walk(&app, &cookie_a, 1).await;
    let b_walk = walk(&app, &cookie_b, 1).await;
    assert_eq!(a_walk.len(), 2, "no duplicates and no gaps for A");
    assert_eq!(b_walk.len(), 2, "no duplicates and no gaps for B");
    assert_eq!(
        a_walk
            .iter()
            .cloned()
            .collect::<std::collections::HashSet<_>>(),
        a_sessions
    );
    assert_eq!(
        b_walk
            .iter()
            .cloned()
            .collect::<std::collections::HashSet<_>>(),
        b_sessions
    );
    assert!(!a_walk.contains(&b_only.to_string()));
    assert!(!b_walk.contains(&a_only.to_string()));
}

#[tokio::test]
async fn a_staff_entered_check_in_of_my_callsign_is_not_in_my_history() {
    // The on-behalf-of ruling, pinned. B holds N7QRP; A (the NCS) logs
    // N7QRP from the radio. The event's `actor` is A, and the payload carries no
    // account link at all — so the row is nobody's history: not B's (no link to
    // follow, and a callsign string match would be unsafe under reassignment),
    // and not A's (A did not check in).
    let app = test_app().await;
    let (cookie_a, a) = sign_in(&app, "ncs-operator@example.com").await;
    let (cookie_b, b) = sign_in(&app, "quiet-participant@example.com").await;
    app.state
        .accounts
        .set_callsign(b, "N7QRP", now_millis())
        .await
        .expect("set B's callsign");

    let session = live_session(&app, a, "Radio Net", "tok-onbehalf").await;
    check_in(
        &app,
        session,
        "N7QRP",
        CheckInSource::Staff,
        a,
        now_millis(),
    )
    .await;

    let (status_b, _, body_b) = send_raw(
        app.router(),
        "GET",
        "/api/accounts/me/check-ins",
        None,
        Some(&cookie_b),
    )
    .await;
    assert_eq!(status_b, StatusCode::OK);
    assert!(
        body_b["items"].as_array().expect("items").is_empty(),
        "a staff-entered add of B's callsign is not B's history"
    );

    let (status_a, _, body_a) = send_raw(
        app.router(),
        "GET",
        "/api/accounts/me/check-ins",
        None,
        Some(&cookie_a),
    )
    .await;
    assert_eq!(status_a, StatusCode::OK);
    assert!(
        body_a["items"].as_array().expect("items").is_empty(),
        "a staff-entered add A authored is not A's own history either"
    );
}

#[tokio::test]
async fn an_unauthenticated_call_is_refused() {
    let app = test_app().await;

    let (status, _, problem) = send_raw(
        app.router(),
        "GET",
        "/api/accounts/me/check-ins",
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(problem["type"], "/errors/unauthenticated");

    // Extractor ordering: `require_session` is a route_layer, so it rejects
    // BEFORE the query string is parsed. Answering 400 here would confirm the
    // route exists to an unauthenticated caller.
    let (status, _, problem) = send_raw(
        app.router(),
        "GET",
        "/api/accounts/me/check-ins?cursor=not-a-cursor&limit=abc",
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(problem["type"], "/errors/unauthenticated");
}

#[tokio::test]
async fn a_cursor_this_server_did_not_issue_is_a_400() {
    let app = test_app().await;
    let (cookie, _) = sign_in(&app, "bad-cursor@example.com").await;

    let (status, _, _) = send_raw(
        app.router(),
        "GET",
        "/api/accounts/me/check-ins?cursor=nonsense",
        None,
        Some(&cookie),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "an unissued cursor is refused, not silently restarted"
    );
}

#[tokio::test]
async fn a_malformed_limit_answers_in_problem_json_for_an_authenticated_caller() {
    // The session cookie is load-bearing: `require_session` is a
    // route_layer, so without it this is a 401 and the query string is never
    // parsed (see `an_unauthenticated_call_is_refused`, which pins that on
    // purpose). WITH a session the extractor runs, and its rejection must join
    // the problem+json contract rather than axum's `text/plain` default.
    let app = test_app().await;
    let (cookie, _) = sign_in(&app, "bad-limit@example.com").await;

    let (status, content_type, raw) = send_untyped(
        app.router(),
        "/api/accounts/me/check-ins?limit=abc",
        Some(&cookie),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(
        content_type, "application/problem+json",
        "a query rejection answers in the problem+json contract"
    );
    let problem: Value = serde_json::from_str(&raw).expect("problem body is JSON");
    assert_eq!(problem["type"], "/errors/validation");
    assert_eq!(problem["status"], 400);
}

#[tokio::test]
async fn the_history_item_carries_exactly_the_seven_fields() {
    // THE REDACTION TEST. Guards against a future
    // well-meaning widening, not against today's code.
    let app = test_app().await;
    let (cookie, account) = sign_in(&app, "redaction@example.com").await;
    let session = live_session(&app, account, "Redaction Net", "tok-redaction").await;
    check_in(
        &app,
        session,
        "W1AW",
        CheckInSource::SelfService,
        account,
        now_millis(),
    )
    .await;

    let (status, _, body) = send_raw(
        app.router(),
        "GET",
        "/api/accounts/me/check-ins",
        None,
        Some(&cookie),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let item = &body["items"][0];
    let keys: std::collections::BTreeSet<&str> = item
        .as_object()
        .expect("item object")
        .keys()
        .map(String::as_str)
        .collect();
    assert_eq!(
        keys,
        ITEM_FIELDS
            .into_iter()
            .collect::<std::collections::BTreeSet<_>>(),
        "the item's key set is exactly the seven wire fields"
    );
    for forbidden in FORBIDDEN_ITEM_FIELDS {
        assert!(
            !keys.contains(forbidden),
            "`{forbidden}` must never appear on a history item"
        );
    }
    // The envelope itself carries exactly two keys.
    let envelope: std::collections::BTreeSet<&str> = body
        .as_object()
        .expect("envelope object")
        .keys()
        .map(String::as_str)
        .collect();
    assert_eq!(
        envelope,
        ["items", "nextCursor"]
            .into_iter()
            .collect::<std::collections::BTreeSet<_>>()
    );
}

#[tokio::test]
async fn the_export_bundle_still_returns_the_same_row_set() {
    // Distinctness: two reads, different purpose and different shape,
    // but they must never disagree about WHICH rows are yours.
    let app = test_app().await;
    let (cookie, account) = sign_in(&app, "distinct@example.com").await;
    let session_a = live_session(&app, account, "Export Net A", "tok-dist-a").await;
    let session_b = live_session(&app, account, "Export Net B", "tok-dist-b").await;
    let staff_session = live_session(&app, account, "Staff Net", "tok-dist-staff").await;

    let t = now_millis();
    check_in(
        &app,
        session_a,
        "W1AW",
        CheckInSource::SelfService,
        account,
        t + 10,
    )
    .await;
    check_in(
        &app,
        session_b,
        "W1AW",
        CheckInSource::SelfService,
        account,
        t + 20,
    )
    .await;
    // Authored, but not the account's own check-in — excluded from BOTH reads.
    check_in(
        &app,
        staff_session,
        "K2ABC",
        CheckInSource::Staff,
        account,
        t + 30,
    )
    .await;

    let (_, _, history) = send_raw(
        app.router(),
        "GET",
        "/api/accounts/me/check-ins",
        None,
        Some(&cookie),
    )
    .await;
    let history_sessions: std::collections::BTreeSet<String> =
        session_ids(&history).into_iter().collect();

    let (export_status, export_headers, export) = send_raw(
        app.router(),
        "GET",
        "/api/accounts/me/export",
        None,
        Some(&cookie),
    )
    .await;
    assert_eq!(export_status, StatusCode::OK);
    // Unchanged: still an attachment, still the 8-field export row.
    assert!(export_headers.get(header::CONTENT_DISPOSITION).is_some());
    let export_rows = export["selfCheckIns"].as_array().expect("selfCheckIns");
    assert_eq!(export_rows.len(), 2);
    let export_keys: std::collections::BTreeSet<&str> = export_rows[0]
        .as_object()
        .expect("export row")
        .keys()
        .map(String::as_str)
        .collect();
    assert!(
        export_keys.contains("signalReport") && export_keys.contains("staying"),
        "the export keeps its own wider shape"
    );

    let export_sessions: std::collections::BTreeSet<String> = export_rows
        .iter()
        .map(|r| r["netSessionId"].as_str().expect("id").to_owned())
        .collect();
    assert_eq!(
        history_sessions, export_sessions,
        "the widget and the export agree on which rows are yours"
    );
    assert!(!history_sessions.contains(&staff_session.to_string()));
}

#[tokio::test]
async fn an_unrecognised_query_parameter_is_refused_on_the_check_in_history_read() {
    // The session cookie is load-bearing for the same
    // reason `a_malformed_limit_answers_in_problem_json_for_an_authenticated_caller`
    // records: `require_session` is a route_layer, so without it this is a 401
    // and the query string is never parsed.
    //
    // The `detail` assertion deliberately inverts the house rule against
    // asserting prose, on the same grounds `api_admin.rs:1169-1172` states for
    // its own: `ProfilePage.tsx:1200-1201` renders
    // `checkInPagingProblem.detail` VERBATIM to the user on this surface, and
    // the backend always populates `detail` for `ApiError::Validation` (the
    // `ApiError::problem` mapping in `problem.rs`), so this string
    // IS the shipped user-facing copy — not an internal message a wording change
    // may freely alter.
    let app = test_app().await;
    let (cookie, _) = sign_in(&app, "unknown-key@example.com").await;

    for uri in [
        "/api/accounts/me/check-ins?limitt=5",
        "/api/accounts/me/check-ins?_t=1724716800",
    ] {
        let (status, content_type, raw) = send_untyped(app.router(), uri, Some(&cookie)).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{uri} is a 400");
        assert_eq!(content_type, "application/problem+json", "{uri} media type");
        let problem: Value = serde_json::from_str(&raw).expect("problem body is JSON");
        assert_eq!(problem["type"], "/errors/validation", "{uri} slug");
        assert_eq!(problem["status"], 400, "{uri} status field");
        assert_eq!(
            problem["detail"],
            "That request wasn't something the server could read — reload the page and try again.",
            "{uri} detail is the human-facing copy this surface renders verbatim"
        );
    }
}

#[tokio::test]
async fn the_history_reports_the_band_of_the_way_in_this_check_in_actually_used() {
    // The widget once read
    // `definition_snapshot->'connections'->0->>'band'`, which is a fact about
    // the NET: on a cross-mode net it told every EchoLink participant they had
    // been on 20m. Two check-ins on ONE session, one per way in, is the only
    // fixture that can tell the two reads apart.
    use netroll_adapters::pg::net_sessions::AddCheckInOutcome;
    use netroll_domain::net::connection::Via;

    let app = test_app().await;
    let (cookie, account) = sign_in(&app, "maria@example.com").await;
    let row = cross_mode_session(&app, account, "Cross-mode Net", "tok-16-5").await;
    let hf = row.definition_snapshot.connections[0].id;
    let echolink = row.definition_snapshot.connections[1].id;

    for (call, via, at) in [
        ("W1AW", Via::Connection(hf), 1_000u64),
        ("W1AW", Via::Connection(echolink), 2_000),
    ] {
        let outcome = app
            .state
            .net_sessions
            .add_check_in(
                row.id,
                &parse_callsign(call).expect("valid callsign"),
                Uuid::now_v7(),
                None,
                None,
                StayingStatus::InAndOut,
                None,
                None,
                None,
                CheckInSource::SelfService,
                Some(&via),
                None,
                Some(account),
                at,
            )
            .await
            .expect("add check-in");
        assert!(matches!(outcome, AddCheckInOutcome::Added(_)));
    }

    let (status, _headers, body) = send_raw(
        app.router(),
        "GET",
        "/api/accounts/me/check-ins?limit=10",
        None,
        Some(&cookie),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let items = body["items"].as_array().expect("items");
    assert_eq!(items.len(), 2);

    // Newest first: the EchoLink one leads.
    let over_echolink = &items[0];
    let over_hf = &items[1];
    assert_eq!(over_echolink["via"], "EchoLink — 12345");
    assert!(
        over_echolink["band"].is_null() && over_echolink["mode"].is_null(),
        "an internet-carried way in has no band to report: {over_echolink}"
    );
    assert_eq!(over_hf["via"], "HF — 14.230 MHz");
    assert_eq!(over_hf["band"], "20m");
    assert_eq!(over_hf["mode"], "ssb");
    assert_ne!(
        over_echolink["band"], over_hf["band"],
        "two check-ins on ONE session must not report the same band"
    );
}

#[tokio::test]
async fn the_history_reports_the_frequency_the_station_was_worked_on_not_the_one_the_net_moved_to()
{
    // The worked stations must not change.
    // The fixture QSYs BETWEEN two check-ins on the SAME connection, because a
    // never-moving fixture is green under the frozen, the live AND the correct
    // implementation. Both rows once read the snapshot's planned frequency;
    // each now reads the frequency in force at its own seq.
    use netroll_adapters::pg::net_sessions::{AddCheckInOutcome, ChangeFrequencyOutcome};
    use netroll_domain::net::connection::Via;

    let app = test_app().await;
    let (cookie, account) = sign_in(&app, "maria@example.com").await;
    let row = cross_mode_session(&app, account, "Moving Net", "tok-17-1").await;
    let hf = row.definition_snapshot.connections[0].id;
    let session = row.id;

    let app_ref = &app;
    let check_in_over_hf = |at: u64| async move {
        let outcome = app_ref
            .state
            .net_sessions
            .add_check_in(
                session,
                &parse_callsign("W1AW").expect("valid callsign"),
                Uuid::now_v7(),
                None,
                None,
                StayingStatus::InAndOut,
                None,
                None,
                None,
                CheckInSource::SelfService,
                Some(&Via::Connection(hf)),
                None,
                Some(account),
                at,
            )
            .await
            .expect("add check-in");
        assert!(matches!(outcome, AddCheckInOutcome::Added(_)));
    };
    check_in_over_hf(1_000).await;
    let moved = app
        .state
        .net_sessions
        .change_frequency(session, hf, 14_250_000, Some(account), 1_500)
        .await
        .expect("change frequency");
    assert!(
        matches!(moved, ChangeFrequencyOutcome::Changed(_)),
        "{moved:?}"
    );
    check_in_over_hf(2_000).await;

    let (status, _headers, body) = send_raw(
        app.router(),
        "GET",
        "/api/accounts/me/check-ins?limit=10",
        None,
        Some(&cookie),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let items = body["items"].as_array().expect("items");
    assert_eq!(items.len(), 2);

    // Newest first: the post-move check-in leads.
    let after_move = &items[0];
    let before_move = &items[1];
    assert_ne!(
        before_move["via"], after_move["via"],
        "two check-ins either side of a QSY must not read the same frequency"
    );
    assert_eq!(before_move["via"], "HF — 14.230 MHz");
    assert_eq!(after_move["via"], "HF — 14.250 MHz");
    // The band token is the owner's and is NOT re-derived after a move.
    assert_eq!(before_move["band"], "20m");
    assert_eq!(after_move["band"], "20m");
}

#[tokio::test]
async fn a_check_in_with_no_recorded_way_in_reports_neither_a_way_in_nor_a_borrowed_band() {
    // `None` means nobody recorded it. Reporting position zero's band
    // here is precisely the substitution to avoid, and the fallback
    // permitted here is the ADIF's alone.
    let app = test_app().await;
    let (cookie, account) = sign_in(&app, "maria@example.com").await;
    let row = cross_mode_session(&app, account, "Cross-mode Net", "tok-16-5b").await;
    check_in(
        &app,
        row.id,
        "W1AW",
        CheckInSource::SelfService,
        account,
        1_000,
    )
    .await;

    let (status, _headers, body) = send_raw(
        app.router(),
        "GET",
        "/api/accounts/me/check-ins?limit=10",
        None,
        Some(&cookie),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let item = &body["items"][0];
    let object = item.as_object().expect("an item object");
    assert!(
        object.contains_key("netTitle") && object.contains_key("callsign"),
        "the item serialized its always-present keys, so a null below means null"
    );
    // Asserted key-by-key: `Value::Null` is also what a missing key indexes to,
    // so the presence assertion is the half that makes the null mean anything.
    for key in ["via", "band", "mode"] {
        assert!(
            object.contains_key(key),
            "the item serialized `{key}`: {item}"
        );
        assert!(
            item[key].is_null(),
            "`{key}` is null, never position zero's: {item}"
        );
    }
}
