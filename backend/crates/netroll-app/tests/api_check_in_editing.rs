// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
//! Full-loop API tests for the check-in detail edit / remove / soft-lock
//! surface: real router, real Postgres (testcontainers), capturing
//! fake mailer. Asserts status codes, problem+json `type` slugs, body values,
//! and DB/log side-effects — never message prose (house TDD rule).

use std::sync::Arc;
use std::sync::Mutex;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use netroll_app::http::net_sessions::EDITABLE_CHECK_IN_FIELDS;
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

async fn create_net(app: &TestApp, cookie: &str) -> String {
    let (status, body) = send_json(
        app.router(),
        "POST",
        "/api/net-definitions",
        Some(full_definition_json()),
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

async fn grant(
    app: &TestApp,
    cookie: &str,
    session_id: &str,
    callsign: &str,
    role: &str,
) -> StatusCode {
    send_json(
        app.router(),
        "POST",
        &format!("/api/net-sessions/{session_id}/roles"),
        Some(json!({ "callsign": callsign, "role": role })),
        Some(cookie),
    )
    .await
    .0
}

/// Adds a callsign-only check-in and returns its `checkInId`.
async fn add_check_in(app: &TestApp, cookie: &str, session_id: &str, callsign: &str) -> String {
    let (status, body) = send_json(
        app.router(),
        "POST",
        &format!("/api/net-sessions/{session_id}/check-ins"),
        Some(json!({ "callsign": callsign })),
        Some(cookie),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    body["roster"]
        .as_array()
        .expect("roster")
        .iter()
        .find(|e| e["callsign"] == callsign)
        .expect("the added row")["checkInId"]
        .as_str()
        .expect("checkInId")
        .to_owned()
}

fn edit(callsign: &str, version: u64) -> Value {
    json!({ "callsign": callsign, "expectedVersion": version })
}

/// The value each editable field is seeded with before a partial-body edit
/// Keyed by the WIRE name so the guard can assert this map
/// covers exactly [`EDITABLE_CHECK_IN_FIELDS`] — a field added to that const
/// without a seed here fails the suite instead of quietly going uncovered.
fn seeded_field_values() -> Vec<(&'static str, Value)> {
    vec![
        ("name", json!("Maria")),
        ("location", json!("Hartford, CT")),
        ("grid", json!("FN31pr")),
        ("signalReport", json!("599")),
        ("staying", json!("in-and-out")),
        ("precedence", json!("emergency")),
        ("traffic", json!(3)),
        ("notes", json!("handling one piece of traffic")),
        // The public note joins the editable field set, so it joins
        // the survival seed in the same breath — forward-guard is
        // the reason a NEW field cannot rejoin the wipe set silently.
        ("publicNote", json!("relaying for W1BBB this round")),
        // `via` joins the editable field set, so it joins the
        // survival seed in the same breath. The FREE-TEXT variant is used here
        // deliberately — a `connection` variant would need this session's own
        // frozen id, which this table is not given, and the wipe it guards
        // against is identical either way.
        (
            "via",
            json!({ "kind": "unlisted", "text": "Bill's phone patch" }),
        ),
        // The relaying station joins the editable field set, so it
        // joins the survival seed in the same breath. Losing it to a partial
        // body is DATA LOSS — nothing else in the log records who passed the
        // traffic.
        ("relayedBy", json!("W3REL")),
    ]
}

/// Adds `callsign` to an EXISTING session and populates every editable field on
/// it, returning the `check_in_id` with the entry at version 2.
async fn seed_entry_with_every_field(
    app: &TestApp,
    cookie: &str,
    session_id: &str,
    callsign: &str,
) -> String {
    let check_in_id = add_check_in(app, cookie, session_id, callsign).await;

    let mut body = serde_json::Map::new();
    body.insert("callsign".into(), json!(callsign));
    body.insert("expectedVersion".into(), json!(1));
    for (field, value) in seeded_field_values() {
        body.insert(field.into(), value);
    }
    let (status, _) = send_json(
        app.router(),
        "PUT",
        &format!("/api/net-sessions/{session_id}/check-ins/{check_in_id}"),
        Some(Value::Object(body)),
        Some(cookie),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "seeding the full field set succeeds"
    );
    check_in_id
}

/// Seeds a session whose OWNER is checked into it with every editable field
/// populated, and returns `(session_id, check_in_id)` with the entry at
/// version 2: a staff member who is also a participant in their own net.
async fn owner_checked_into_own_net_with_every_field(
    app: &TestApp,
    owner: &str,
) -> (String, String) {
    let definition_id = create_net(app, owner).await;
    let session_id = start_session(app, owner, &definition_id).await;
    let check_in_id = seed_entry_with_every_field(app, owner, &session_id, "W1AW").await;
    (session_id, check_in_id)
}

/// The three-field body the PUBLIC participant surface sends
/// (`SelfCheckInControl.tsx`) — callsign, staying and the CAS, nothing else.
fn public_staying_toggle(callsign: &str, staying: &str, version: u64) -> Value {
    json!({ "callsign": callsign, "staying": staying, "expectedVersion": version })
}

fn entry_of<'a>(summary: &'a Value, check_in_id: &str) -> &'a Value {
    summary["roster"]
        .as_array()
        .expect("roster")
        .iter()
        .find(|e| e["checkInId"] == check_in_id)
        .expect("the entry")
}

// --- A partial body must never wipe an unmentioned field ---------

#[tokio::test]
async fn a_staff_members_public_staying_toggle_preserves_every_other_field() {
    // The net OWNER is checked into their own net with every
    // field logged, then taps the PUBLIC page's staying toggle, whose body
    // carries only callsign/staying/expectedVersion. Their staying changes and
    // NOTHING else does. Before the fix the `role_has(EditCheckIn)` branch sent
    // this down the staff PUT-replace arm and wiped all seven other fields.
    let app = test_app().await;
    let owner = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;
    let (session_id, check_in_id) = owner_checked_into_own_net_with_every_field(&app, &owner).await;

    let (status, body) = send_json(
        app.router(),
        "PUT",
        &format!("/api/net-sessions/{session_id}/check-ins/{check_in_id}"),
        Some(public_staying_toggle("W1AW", "staying-for-comments", 2)),
        Some(&owner),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let entry = entry_of(&body, &check_in_id);
    assert_eq!(
        entry["staying"], "staying-for-comments",
        "the toggle DID change staying"
    );
    assert_eq!(entry["name"], "Maria", "name survived");
    assert_eq!(entry["location"], "Hartford, CT", "location survived");
    assert_eq!(entry["grid"], "FN31pr", "grid survived");
    assert_eq!(entry["signalReport"], "599", "signal report survived");
    assert_eq!(entry["precedence"], "emergency", "precedence survived");
    assert_eq!(entry["traffic"], 3, "traffic survived");
    assert_eq!(
        entry["notes"], "handling one piece of traffic",
        "notes survived"
    );
}

#[tokio::test]
async fn a_staying_only_edit_emits_a_checkin_updated_carrying_the_full_field_set() {
    // The read model is not the whole story: `checkin.updated`
    // carries the FULL post-edit field set, so a wipe writes a SPURIOUS
    // correction into the append-only log that a later replay would reconstruct.
    // Assert the emitted payload itself, not just the folded summary.
    let app = test_app().await;
    let owner = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;
    let (session_id, check_in_id) = owner_checked_into_own_net_with_every_field(&app, &owner).await;

    let (status, _) = send_json(
        app.router(),
        "PUT",
        &format!("/api/net-sessions/{session_id}/check-ins/{check_in_id}"),
        Some(public_staying_toggle("W1AW", "staying-for-comments", 2)),
        Some(&owner),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let payload: Value = sqlx::query_scalar(
        "SELECT payload FROM session_events \
         WHERE session_id = $1 AND kind = 'checkin.updated' \
         ORDER BY seq DESC LIMIT 1",
    )
    .bind(uuid::Uuid::parse_str(&session_id).unwrap())
    .fetch_one(&app.pool)
    .await
    .expect("the latest checkin.updated payload");

    assert_eq!(payload["staying"], "staying-for-comments");
    assert_eq!(payload["name"], "Maria", "logged name intact: {payload}");
    assert_eq!(payload["location"], "Hartford, CT", "{payload}");
    // Canonical Maidenhead: uppercase field, lowercase subsquare.
    assert_eq!(payload["grid"], "FN31pr", "{payload}");
    assert_eq!(payload["signalReport"], "599", "{payload}");
    assert_eq!(payload["precedence"], "emergency", "{payload}");
    assert_eq!(payload["traffic"], 3, "{payload}");
    assert_eq!(
        payload["notes"], "handling one piece of traffic",
        "{payload}"
    );

    // And no spurious correction was derived: only `staying` changed, and
    // staying IS a correcting field, so the fold must show no name/location/
    // grid/report/precedence/traffic correction on this edit.
    let (status, summary) = send_json(
        app.router(),
        "GET",
        &format!("/api/net-sessions/{session_id}"),
        None,
        Some(&owner),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let corrections = entry_of(&summary, &check_in_id)["corrections"]
        .as_array()
        .expect("corrections")
        .clone();
    for wiped in [
        "name",
        "location",
        "grid",
        "signalReport",
        "precedence",
        "traffic",
    ] {
        assert!(
            !corrections
                .iter()
                .any(|c| c["field"] == wiped && c["to"].is_null()),
            "a staying-only toggle derived a clearing correction for {wiped}: {corrections:?}"
        );
    }
}

#[tokio::test]
async fn a_full_body_staff_edit_still_clears_a_blanked_note_and_grid() {
    // The over-correction guard. Fixing the partial-body wipe
    // must NOT cost staff the ability to erase a field on purpose. The detail
    // modal sends "" for a field the operator emptied; PUT-replace must still
    // clear it. This test passes both before and after the fix; it exists to
    // fail loudly if the fix degenerates into "never clear anything".
    let app = test_app().await;
    let owner = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;
    let (session_id, check_in_id) = owner_checked_into_own_net_with_every_field(&app, &owner).await;

    let (status, body) = send_json(
        app.router(),
        "PUT",
        &format!("/api/net-sessions/{session_id}/check-ins/{check_in_id}"),
        Some(json!({
            "callsign": "W1AW",
            "name": "Maria",
            "location": "Hartford, CT",
            // Deliberately emptied by the operator, exactly as the modal sends.
            "grid": "",
            "signalReport": "599",
            "staying": "in-and-out",
            "precedence": "emergency",
            "traffic": 3,
            "notes": "",
            "expectedVersion": 2
        })),
        Some(&owner),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let entry = entry_of(&body, &check_in_id).clone();
    let obj = entry.as_object().expect("entry object");
    assert!(
        obj.get("grid").is_none(),
        "a deliberately blanked grid is still CLEARED: {entry}"
    );
    assert!(
        obj.get("notes").is_none(),
        "a deliberately blanked note is still CLEARED: {entry}"
    );
    // The fields the operator did NOT blank are untouched.
    assert_eq!(entry["name"], "Maria");
    assert_eq!(entry["signalReport"], "599");
    assert_eq!(entry["precedence"], "emergency");
}

#[tokio::test]
async fn omitting_exactly_one_field_from_an_otherwise_full_body_keeps_that_field() {
    // The leave-one-out case: the all-but-staying test below cannot catch a
    // field missed out of the partial-body predicate.
    //
    // The gap: `is_partial()` decides whether the stored entry is folded at all.
    // A body that omits only ONE field, with every other field present, is the
    // ONLY shape that distinguishes "this field is in the predicate" from "this
    // field is not". The all-but-staying shape trips the predicate on the other
    // seven fields regardless, so it would stay green while a ninth field
    // silently reopened the wipe. This loop closes that for every field.
    let app = test_app().await;
    let owner = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;
    // ONE net and ONE session for the whole loop, with a distinct check-in per
    // field: the per-account max-nets cap makes a net-per-iteration loop fail
    // on the cap rather than on the invariant under test.
    let definition_id = create_net(&app, &owner).await;
    let session_id = start_session(&app, &owner, &definition_id).await;

    for (index, omitted) in EDITABLE_CHECK_IN_FIELDS.iter().enumerate() {
        let callsign = format!("W1A{}", (b'A' + index as u8) as char);
        let check_in_id = seed_entry_with_every_field(&app, &owner, &session_id, &callsign).await;

        let mut body = serde_json::Map::new();
        body.insert("callsign".into(), json!(callsign));
        body.insert("expectedVersion".into(), json!(2));
        for (field, value) in seeded_field_values() {
            if field != *omitted {
                body.insert(field.into(), value);
            }
        }
        let (status, after) = send_json(
            app.router(),
            "PUT",
            &format!("/api/net-sessions/{session_id}/check-ins/{check_in_id}"),
            Some(Value::Object(body)),
            Some(&owner),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "omitting {omitted} still edits");

        // The omitted field kept its seeded value; nothing was cleared.
        let entry = entry_of(&after, &check_in_id);
        let expected = seeded_field_values()
            .into_iter()
            .find(|(f, _)| f == omitted)
            .expect("the omitted field is seeded")
            .1;
        assert_eq!(
            entry[*omitted], expected,
            "omitting {omitted} from an otherwise-full body cleared it: {entry}"
        );
    }
}

#[tokio::test]
async fn a_public_note_is_validated_by_the_shared_prose_guard_with_no_event_appended() {
    // The public note is the FIRST free-prose field this
    // project renders on an unauthenticated page — the one surface where the
    // author and the reader are different people — so it goes through the SAME
    // shipped `parse_note` as the staff note: bounded at MAX_NOTE_CHARS,
    // control- and bidi-rejecting, blank-folds-to-none. No second validator.
    let app = test_app().await;
    let owner = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;
    let definition_id = create_net(&app, &owner).await;
    let session_id = start_session(&app, &owner, &definition_id).await;
    let check_in_id = add_check_in(&app, &owner, &session_id, "N1CCK").await;

    for hostile in [
        "x".repeat(2001),
        "look\u{202e}elsewhere".to_owned(),
        "control\u{0007}char".to_owned(),
    ] {
        let (status, body) = send_json(
            app.router(),
            "PUT",
            &format!("/api/net-sessions/{session_id}/check-ins/{check_in_id}"),
            Some(json!({
                "callsign": "N1CCK",
                "publicNote": hostile,
                "expectedVersion": 1
            })),
            Some(&owner),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(body["type"], "/errors/note-invalid");
    }

    // Blank folds to none, exactly as the staff note does, and the entry is
    // still at version 1 — none of the refusals appended an event.
    let (status, after) = send_json(
        app.router(),
        "PUT",
        &format!("/api/net-sessions/{session_id}/check-ins/{check_in_id}"),
        Some(json!({
            "callsign": "N1CCK",
            "publicNote": "   ",
            "expectedVersion": 1
        })),
        Some(&owner),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let entry = entry_of(&after, &check_in_id);
    assert!(
        entry.as_object().expect("obj").get("publicNote").is_none(),
        "a blank public note folds to absent, not to an empty string"
    );
    assert_eq!(
        entry["version"], 2,
        "exactly ONE event was appended — the three refusals appended none"
    );
}

#[tokio::test]
async fn every_editable_field_survives_a_partial_body_edit() {
    // The mechanical forward-guard. The seed is driven from
    // `EDITABLE_CHECK_IN_FIELDS`, the single source of truth beside the request
    // type, so a NEW per-check-in field cannot silently rejoin the wipe set:
    // adding one fails `edit_check_in`'s exhaustive destructure at compile time,
    // and adding it to that const without seeding it fails the first assertion
    // here. The survival check then compares the WHOLE entry object rather than
    // a hand-listed subset, so a field is covered by construction, not by a
    // future author remembering to extend an assertion list.
    let seeded: Vec<&str> = seeded_field_values().into_iter().map(|(k, _)| k).collect();
    let mut declared = EDITABLE_CHECK_IN_FIELDS.to_vec();
    let mut covered = seeded.clone();
    declared.sort_unstable();
    covered.sort_unstable();
    assert_eq!(
        covered, declared,
        "the survival seed must cover EXACTLY the declared editable field set — \
         a field added to EDITABLE_CHECK_IN_FIELDS needs a seed value here"
    );

    let app = test_app().await;
    let owner = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;
    let (session_id, check_in_id) = owner_checked_into_own_net_with_every_field(&app, &owner).await;

    let (status, before) = send_json(
        app.router(),
        "GET",
        &format!("/api/net-sessions/{session_id}"),
        None,
        Some(&owner),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let before_entry = entry_of(&before, &check_in_id).clone();

    let (status, after) = send_json(
        app.router(),
        "PUT",
        &format!("/api/net-sessions/{session_id}/check-ins/{check_in_id}"),
        Some(public_staying_toggle("W1AW", "staying-for-comments", 2)),
        Some(&owner),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let after_entry = entry_of(&after, &check_in_id).clone();

    // Only `staying` (the edit), `version` (the CAS bump) and `corrections`
    // (a fold projection of the staying change) may differ. Everything else —
    // including any field a future story adds — must be byte-identical.
    let strip = |entry: &Value| -> serde_json::Map<String, Value> {
        let mut obj = entry.as_object().expect("entry object").clone();
        obj.remove("staying");
        obj.remove("version");
        obj.remove("corrections");
        obj
    };
    assert_eq!(
        strip(&after_entry),
        strip(&before_entry),
        "a staying-only edit changed something other than staying"
    );
}

// --- Edit happy-path + authz + validation ----------------------------------

#[tokio::test]
async fn a_logger_edits_a_check_in_and_the_summary_folds_the_update() {
    let app = test_app().await;
    let owner = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;
    let logger = sign_in_consent_callsign(&app, "logger@example.com", "w2bcd").await;
    let definition_id = create_net(&app, &owner).await;
    let session_id = start_session(&app, &owner, &definition_id).await;
    assert_eq!(
        grant(&app, &owner, &session_id, "w2bcd", "logger").await,
        StatusCode::OK
    );

    let check_in_id = add_check_in(&app, &logger, &session_id, "W9XYZ").await;

    let (status, body) = send_json(
        app.router(),
        "PUT",
        &format!("/api/net-sessions/{session_id}/check-ins/{check_in_id}"),
        Some(json!({
            "callsign": "W9XY",
            "name": "Maria",
            "location": "Hartford, CT",
            "signalReport": "599",
            "staying": "staying-for-comments",
            "expectedVersion": 1
        })),
        Some(&logger),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let entry = body["roster"]
        .as_array()
        .expect("roster")
        .iter()
        .find(|e| e["checkInId"] == check_in_id.as_str())
        .expect("the edited row");
    assert_eq!(entry["callsign"], "W9XY");
    assert_eq!(entry["name"], "Maria");
    assert_eq!(entry["signalReport"], "599");
    assert_eq!(entry["staying"], "staying-for-comments");
    assert_eq!(entry["version"], 2);
    // A correction was derived for the callsign change (amber annotation source).
    let corr = entry["corrections"].as_array().expect("corrections");
    assert!(
        corr.iter()
            .any(|c| c["field"] == "callsign" && c["from"] == "W9XYZ" && c["to"] == "W9XY")
    );
}

#[tokio::test]
async fn an_edit_replaces_the_grid_and_a_blank_grid_clears_it() {
    // PUT-replace semantics on the grid, and the blank-clears
    // arm the modal actually exercises (it sends "" for a cleared field, and
    // `parse_grid("")` is Err(Empty) — so a missing blank guard would 400 here).
    let app = test_app().await;
    let owner = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;
    let definition_id = create_net(&app, &owner).await;
    let session_id = start_session(&app, &owner, &definition_id).await;
    let check_in_id = add_check_in(&app, &owner, &session_id, "W9XYZ").await;

    let (status, body) = send_json(
        app.router(),
        "PUT",
        &format!("/api/net-sessions/{session_id}/check-ins/{check_in_id}"),
        Some(json!({
            "callsign": "W9XYZ",
            "location": "Hartford, CT",
            "grid": "FN31",
            "expectedVersion": 1
        })),
        Some(&owner),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let entry = body["roster"].as_array().expect("roster")[0].clone();
    assert_eq!(entry["grid"], "FN31");
    assert_eq!(entry["location"], "Hartford, CT");
    let grid_corr = entry["corrections"]
        .as_array()
        .expect("corrections")
        .iter()
        .find(|c| c["field"] == "grid")
        .expect("a grid correction")
        .clone();
    assert!(grid_corr["from"].is_null());
    assert_eq!(grid_corr["to"], "FN31");

    // Replace it with a different grid.
    let (status, body) = send_json(
        app.router(),
        "PUT",
        &format!("/api/net-sessions/{session_id}/check-ins/{check_in_id}"),
        Some(json!({
            "callsign": "W9XYZ",
            "location": "Hartford, CT",
            "grid": "fn42",
            "expectedVersion": 2
        })),
        Some(&owner),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        body["roster"].as_array().expect("roster")[0]["grid"],
        "FN42"
    );

    // Clear it with the empty string the modal sends.
    let (status, body) = send_json(
        app.router(),
        "PUT",
        &format!("/api/net-sessions/{session_id}/check-ins/{check_in_id}"),
        Some(json!({
            "callsign": "W9XYZ",
            "location": "Hartford, CT",
            "grid": "",
            "expectedVersion": 3
        })),
        Some(&owner),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "a blank grid clears, never 400s");
    let entry = body["roster"].as_array().expect("roster")[0].clone();
    assert!(
        entry.as_object().expect("obj").get("grid").is_none(),
        "a cleared grid is ABSENT from the response: {entry}"
    );
}

#[tokio::test]
async fn an_invalid_grid_on_edit_is_a_400_and_appends_no_event() {
    // The PUT path shares `parse_edit_grid`, so the same three
    // malformed values are refused with the same `/errors/grid-invalid` slug and
    // no `checkin.updated` is appended.
    let app = test_app().await;
    let owner = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;
    let definition_id = create_net(&app, &owner).await;
    let session_id = start_session(&app, &owner, &definition_id).await;
    let check_in_id = add_check_in(&app, &owner, &session_id, "W9XYZ").await;

    for bad in ["FN3", "SS11", "FN-31"] {
        let (status, body) = send_json(
            app.router(),
            "PUT",
            &format!("/api/net-sessions/{session_id}/check-ins/{check_in_id}"),
            Some(json!({ "callsign": "W9XYZ", "grid": bad, "expectedVersion": 1 })),
            Some(&owner),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{bad:?} is a 400");
        assert_eq!(body["type"], "/errors/grid-invalid", "{bad:?}");
    }
    let updates: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM session_events WHERE session_id = $1 AND kind = 'checkin.updated'",
    )
    .bind(uuid::Uuid::parse_str(&session_id).unwrap())
    .fetch_one(&app.pool)
    .await
    .expect("count updates");
    assert_eq!(updates, 0, "no phantom checkin.updated");
}

#[tokio::test]
async fn a_relay_cannot_edit_a_check_in() {
    let app = test_app().await;
    let owner = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;
    let relay = sign_in_consent_callsign(&app, "relay@example.com", "w2bcd").await;
    let definition_id = create_net(&app, &owner).await;
    let session_id = start_session(&app, &owner, &definition_id).await;
    assert_eq!(
        grant(&app, &owner, &session_id, "w2bcd", "relay").await,
        StatusCode::OK
    );
    let check_in_id = add_check_in(&app, &owner, &session_id, "W9XYZ").await;

    let (status, body) = send_json(
        app.router(),
        "PUT",
        &format!("/api/net-sessions/{session_id}/check-ins/{check_in_id}"),
        Some(edit("W9XY", 1)),
        Some(&relay),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(body["type"], "/errors/forbidden");
}

#[tokio::test]
async fn a_participant_cannot_edit_a_check_in() {
    let app = test_app().await;
    let owner = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;
    let participant = sign_in_consent(&app, "participant@example.com").await;
    let definition_id = create_net(&app, &owner).await;
    let session_id = start_session(&app, &owner, &definition_id).await;
    let check_in_id = add_check_in(&app, &owner, &session_id, "W9XYZ").await;

    let (status, _) = send_json(
        app.router(),
        "PUT",
        &format!("/api/net-sessions/{session_id}/check-ins/{check_in_id}"),
        Some(edit("W9XY", 1)),
        Some(&participant),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn a_non_member_editing_a_missing_session_is_404_before_403() {
    let app = test_app().await;
    let _owner = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;
    let stranger = sign_in_consent(&app, "stranger@example.com").await;
    // A random session id + check-in id: existence is decided before authority.
    let session_id = uuid::Uuid::now_v7();
    let check_in_id = uuid::Uuid::now_v7();
    let (status, body) = send_json(
        app.router(),
        "PUT",
        &format!("/api/net-sessions/{session_id}/check-ins/{check_in_id}"),
        Some(edit("W9XY", 1)),
        Some(&stranger),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body["type"], "/errors/net-session-not-found");
}

#[tokio::test]
async fn an_invalid_callsign_on_edit_is_a_400() {
    let app = test_app().await;
    let owner = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;
    let definition_id = create_net(&app, &owner).await;
    let session_id = start_session(&app, &owner, &definition_id).await;
    let check_in_id = add_check_in(&app, &owner, &session_id, "W9XYZ").await;
    let (status, body) = send_json(
        app.router(),
        "PUT",
        &format!("/api/net-sessions/{session_id}/check-ins/{check_in_id}"),
        Some(json!({ "callsign": "!!!not-a-call!!!", "expectedVersion": 1 })),
        Some(&owner),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["type"], "/errors/callsign-invalid");
}

#[tokio::test]
async fn editing_on_a_closed_session_is_a_409() {
    let app = test_app().await;
    let owner = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;
    let definition_id = create_net(&app, &owner).await;
    let session_id = start_session(&app, &owner, &definition_id).await;
    let check_in_id = add_check_in(&app, &owner, &session_id, "W9XYZ").await;
    let (status, _) = send_json(
        app.router(),
        "POST",
        &format!("/api/net-sessions/{session_id}/close"),
        None,
        Some(&owner),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let (status, body) = send_json(
        app.router(),
        "PUT",
        &format!("/api/net-sessions/{session_id}/check-ins/{check_in_id}"),
        Some(edit("W9XY", 1)),
        Some(&owner),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(body["type"], "/errors/session-already-closed");
}

// --- Version CAS ------------------------------------------------------------

#[tokio::test]
async fn two_conflicting_edits_the_stale_one_is_refused_stale_version_with_no_phantom_append() {
    let app = test_app().await;
    let owner = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;
    let definition_id = create_net(&app, &owner).await;
    let session_id = start_session(&app, &owner, &definition_id).await;
    let check_in_id = add_check_in(&app, &owner, &session_id, "W9XYZ").await;

    // First edit at version 1 wins → the entry is now version 2.
    let (status, _) = send_json(
        app.router(),
        "PUT",
        &format!("/api/net-sessions/{session_id}/check-ins/{check_in_id}"),
        Some(edit("W9XY", 1)),
        Some(&owner),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    // A second edit still holding the STALE version 1 is refused.
    let (status, body) = send_json(
        app.router(),
        "PUT",
        &format!("/api/net-sessions/{session_id}/check-ins/{check_in_id}"),
        Some(edit("W9AB", 1)),
        Some(&owner),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(body["type"], "/errors/stale-version");

    // No phantom append: the log holds exactly started + added + ONE update.
    let count: i64 =
        sqlx::query_scalar("SELECT count(*) FROM session_events WHERE session_id = $1")
            .bind(uuid::Uuid::parse_str(&session_id).unwrap())
            .fetch_one(&app.pool)
            .await
            .expect("count");
    assert_eq!(count, 3);
}

// --- Remove -----------------------------------------------------------------

#[tokio::test]
async fn removing_a_check_in_drops_the_row_but_the_log_retains_it() {
    let app = test_app().await;
    let owner = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;
    let definition_id = create_net(&app, &owner).await;
    let session_id = start_session(&app, &owner, &definition_id).await;
    let check_in_id = add_check_in(&app, &owner, &session_id, "W9XYZ").await;

    let (status, body) = send_json(
        app.router(),
        "DELETE",
        &format!("/api/net-sessions/{session_id}/check-ins/{check_in_id}"),
        Some(json!({ "expectedVersion": 1 })),
        Some(&owner),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(body["roster"].as_array().expect("roster").is_empty());

    // The append-only log retains started + added + removed.
    let count: i64 =
        sqlx::query_scalar("SELECT count(*) FROM session_events WHERE session_id = $1")
            .bind(uuid::Uuid::parse_str(&session_id).unwrap())
            .fetch_one(&app.pool)
            .await
            .expect("count");
    assert_eq!(count, 3);
    let removed: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM session_events WHERE session_id = $1 AND kind = 'checkin.removed'",
    )
    .bind(uuid::Uuid::parse_str(&session_id).unwrap())
    .fetch_one(&app.pool)
    .await
    .expect("count removed");
    assert_eq!(removed, 1);
}

#[tokio::test]
async fn removing_with_a_stale_version_after_a_concurrent_edit_is_refused_and_the_row_survives() {
    // `remove_check_in` shares
    // `apply_guarded_check_in_edit` with `edit_check_in`, so the version CAS
    // must equally protect a legitimately-just-updated row from being
    // destroyed by a remove that is still holding the PRE-update version —
    // not just protect edit-vs-edit (already covered above).
    let app = test_app().await;
    let owner = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;
    let definition_id = create_net(&app, &owner).await;
    let session_id = start_session(&app, &owner, &definition_id).await;
    let check_in_id = add_check_in(&app, &owner, &session_id, "W9XYZ").await;

    // Someone else's edit lands first → the entry is now version 2.
    let (status, _) = send_json(
        app.router(),
        "PUT",
        &format!("/api/net-sessions/{session_id}/check-ins/{check_in_id}"),
        Some(edit("W9XY", 1)),
        Some(&owner),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    // A remove still holding the STALE pre-update version 1 is refused —
    // the CAS must not skip removal or let it destroy the just-updated row.
    let (status, body) = send_json(
        app.router(),
        "DELETE",
        &format!("/api/net-sessions/{session_id}/check-ins/{check_in_id}"),
        Some(json!({ "expectedVersion": 1 })),
        Some(&owner),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(body["type"], "/errors/stale-version");

    // No phantom removal: no `checkin.removed` was appended, and the row is
    // still present in the folded roster (fetched fresh via GET).
    let (status, summary) = send_json(
        app.router(),
        "GET",
        &format!("/api/net-sessions/{session_id}"),
        None,
        Some(&owner),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let roster = summary["roster"].as_array().expect("roster");
    assert_eq!(roster.len(), 1);
    assert_eq!(roster[0]["checkInId"], check_in_id);

    let removed: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM session_events WHERE session_id = $1 AND kind = 'checkin.removed'",
    )
    .bind(uuid::Uuid::parse_str(&session_id).unwrap())
    .fetch_one(&app.pool)
    .await
    .expect("count removed");
    assert_eq!(removed, 0);
}

// --- Soft-lock --------------------------------------------------------------

#[tokio::test]
async fn acquire_returns_the_lease_and_a_competing_account_gets_409_lock_held() {
    let app = test_app().await;
    let owner = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;
    let logger = sign_in_consent_callsign(&app, "logger@example.com", "w2bcd").await;
    let definition_id = create_net(&app, &owner).await;
    let session_id = start_session(&app, &owner, &definition_id).await;
    assert_eq!(
        grant(&app, &owner, &session_id, "w2bcd", "logger").await,
        StatusCode::OK
    );
    let check_in_id = add_check_in(&app, &owner, &session_id, "W9XYZ").await;

    // The owner acquires the lease.
    let (status, body) = send_json(
        app.router(),
        "POST",
        &format!("/api/net-sessions/{session_id}/check-ins/{check_in_id}/lock"),
        None,
        Some(&owner),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["holderCallsign"], "W1AW");
    assert!(body["expiresAt"].is_string());

    // A DIFFERENT account (the logger) is refused while the lease is valid.
    let (status, body) = send_json(
        app.router(),
        "POST",
        &format!("/api/net-sessions/{session_id}/check-ins/{check_in_id}/lock"),
        None,
        Some(&logger),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(body["type"], "/errors/lock-held");
    assert_eq!(body["detail"], "W1AW");

    // The owner renews their own lease (not a conflict).
    let (status, _) = send_json(
        app.router(),
        "POST",
        &format!("/api/net-sessions/{session_id}/check-ins/{check_in_id}/lock"),
        None,
        Some(&owner),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    // Release is idempotent 204; afterwards the logger CAN acquire.
    let (status, _) = send_json(
        app.router(),
        "DELETE",
        &format!("/api/net-sessions/{session_id}/check-ins/{check_in_id}/lock"),
        None,
        Some(&owner),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (status, _) = send_json(
        app.router(),
        "POST",
        &format!("/api/net-sessions/{session_id}/check-ins/{check_in_id}/lock"),
        None,
        Some(&logger),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn a_different_account_holding_the_lock_blocks_an_edit_with_409_lock_held() {
    let app = test_app().await;
    let owner = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;
    let logger = sign_in_consent_callsign(&app, "logger@example.com", "w2bcd").await;
    let definition_id = create_net(&app, &owner).await;
    let session_id = start_session(&app, &owner, &definition_id).await;
    assert_eq!(
        grant(&app, &owner, &session_id, "w2bcd", "logger").await,
        StatusCode::OK
    );
    let check_in_id = add_check_in(&app, &owner, &session_id, "W9XYZ").await;

    // The logger holds the lease.
    let (status, _) = send_json(
        app.router(),
        "POST",
        &format!("/api/net-sessions/{session_id}/check-ins/{check_in_id}/lock"),
        None,
        Some(&logger),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    // The owner's edit is refused proactively by the lock (the NORMAL collision).
    let (status, body) = send_json(
        app.router(),
        "PUT",
        &format!("/api/net-sessions/{session_id}/check-ins/{check_in_id}"),
        Some(edit("W9XY", 1)),
        Some(&owner),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(body["type"], "/errors/lock-held");

    // The lock holder (logger) may still edit what it holds.
    let (status, _) = send_json(
        app.router(),
        "PUT",
        &format!("/api/net-sessions/{session_id}/check-ins/{check_in_id}"),
        Some(edit("W9XY", 1)),
        Some(&logger),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
}

// --- The roster-edit rejections name their field ---------------------------

#[tokio::test]
async fn a_rejected_note_and_signal_report_each_name_the_field_they_are_about() {
    // `parse_edit_note` and `parse_edit_signal_report` once rendered
    // `ProfileError`'s `Display` verbatim into `detail`, with nothing prefixed
    // and no sentence composed — so a 2001-char note answered a subjectless
    // fragment naming no field. Asserting the field POINTER (not the
    // sentence) is the same property `api_net_definitions.rs:306` pins with
    // `starts_with("visibility:")`.
    let app = test_app().await;
    let owner = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;
    let definition_id = create_net(&app, &owner).await;
    let session_id = start_session(&app, &owner, &definition_id).await;
    let check_in_id = add_check_in(&app, &owner, &session_id, "N1CCK").await;

    let (status, note_body) = send_json(
        app.router(),
        "PUT",
        &format!("/api/net-sessions/{session_id}/check-ins/{check_in_id}"),
        Some(json!({
            "callsign": "N1CCK",
            "notes": "x".repeat(2001),
            "expectedVersion": 1
        })),
        Some(&owner),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(note_body["type"], "/errors/note-invalid", "slug unchanged");
    let note_detail = note_body["detail"].as_str().expect("detail").to_owned();
    assert!(
        note_detail.contains("note"),
        "the rejection must name the field it is about: {note_detail}"
    );

    let (status, report_body) = send_json(
        app.router(),
        "PUT",
        &format!("/api/net-sessions/{session_id}/check-ins/{check_in_id}"),
        Some(json!({
            "callsign": "N1CCK",
            "signalReport": "x".repeat(17),
            "expectedVersion": 1
        })),
        Some(&owner),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(
        report_body["type"], "/errors/signal-report-invalid",
        "slug unchanged"
    );
    let report_detail = report_body["detail"].as_str().expect("detail").to_owned();
    assert!(
        report_detail.contains("signal report"),
        "the rejection must name the field it is about: {report_detail}"
    );

    // Two fields, two bounds, two answers — a shared fragment would collapse
    // these into one, which is the defect the finding names.
    assert_ne!(note_detail, report_detail);
}

#[tokio::test]
async fn a_check_in_note_full_of_blank_lines_comes_back_as_one_blank_line() {
    // On the real path: `parse_note` delegates to the shared
    // prose guard, so the same collapse applied to the net description reaches
    // the per-station note as well — deliberately, and proved here rather than
    // inherited silently. The value is over MAX_NOTE_CHARS raw and well under
    // it after the collapse, so this also pins the ordering end to end:
    // a bound applied before the collapse would answer 400 instead of 200.
    let app = test_app().await;
    let owner = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;
    let definition_id = create_net(&app, &owner).await;
    let session_id = start_session(&app, &owner, &definition_id).await;
    let check_in_id = add_check_in(&app, &owner, &session_id, "N1CCK").await;

    let (status, body) = send_json(
        app.router(),
        "PUT",
        &format!("/api/net-sessions/{session_id}/check-ins/{check_in_id}"),
        Some(json!({
            "callsign": "N1CCK",
            "notes": format!("QSY to 20m{}Standby for traffic", "\n".repeat(2000)),
            "expectedVersion": 1
        })),
        Some(&owner),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "the collapse precedes the bound, so this is not TooLong"
    );
    let notes = entry_of(&body, &check_in_id)["notes"]
        .as_str()
        .expect("notes present");
    assert!(
        !notes.contains("\n\n\n"),
        "no run of blank lines survives: {notes:?}"
    );
    assert_eq!(notes, "QSY to 20m\n\nStandby for traffic");
}

// --- `via` through the ordinary write paths ---------------------

/// The id of the session's FIRST frozen connection — the only ids a `via` may
/// name.
async fn first_connection_id(app: &TestApp, cookie: &str, session_id: &str) -> String {
    let (status, body) = send_json(
        app.router(),
        "GET",
        &format!("/api/net-sessions/{session_id}"),
        None,
        Some(cookie),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    body["connections"][0]["id"]
        .as_str()
        .expect("a session connection id")
        .to_owned()
}

/// How many events the session's log holds — the half a status-code assertion
/// alone cannot see.
async fn event_count(app: &TestApp, cookie: &str, session_id: &str) -> u64 {
    let (status, body) = send_json(
        app.router(),
        "GET",
        &format!("/api/net-sessions/{session_id}"),
        None,
        Some(cookie),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    body["latestSeq"].as_u64().expect("latestSeq")
}

#[tokio::test]
async fn a_via_naming_a_connection_this_session_never_froze_is_refused_with_no_event() {
    // The shipped `/errors/net-connection-not-found` (404)
    // that `change_frequency` already uses, NOT a second idiom.
    // Asserted on the LOG as well as the response: a 404 that still appended a
    // `checkin.added` would leave a `via` no surface could render.
    let app = test_app().await;
    let owner = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;
    let definition_id = create_net(&app, &owner).await;
    let session_id = start_session(&app, &owner, &definition_id).await;
    let before = event_count(&app, &owner, &session_id).await;

    let (status, body) = send_json(
        app.router(),
        "POST",
        &format!("/api/net-sessions/{session_id}/check-ins"),
        Some(json!({
            "callsign": "W9XYZ",
            "via": { "kind": "connection", "connectionId": "00000000-0000-0000-0000-0000deadbeef" }
        })),
        Some(&owner),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body["type"], "/errors/net-connection-not-found");
    assert_eq!(
        event_count(&app, &owner, &session_id).await,
        before,
        "a refused write appends NO event"
    );
}

#[tokio::test]
async fn an_edit_naming_a_connection_this_session_never_froze_is_refused_with_no_event() {
    let app = test_app().await;
    let owner = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;
    let definition_id = create_net(&app, &owner).await;
    let session_id = start_session(&app, &owner, &definition_id).await;
    let check_in_id = add_check_in(&app, &owner, &session_id, "W9XYZ").await;
    let before = event_count(&app, &owner, &session_id).await;

    let (status, body) = send_json(
        app.router(),
        "PUT",
        &format!("/api/net-sessions/{session_id}/check-ins/{check_in_id}"),
        Some(json!({
            "callsign": "W9XYZ",
            "via": { "kind": "connection", "connectionId": "00000000-0000-0000-0000-0000deadbeef" },
            "expectedVersion": 1
        })),
        Some(&owner),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body["type"], "/errors/net-connection-not-found");
    assert_eq!(event_count(&app, &owner, &session_id).await, before);
}

#[tokio::test]
async fn editing_via_derives_one_correction_whose_from_and_to_are_labels() {
    // The fold cannot resolve a connection id to a label — it
    // has never seen a connection set — so the projection does, and the sum type
    // it arrives in is what stops a UUID reaching the wire.
    let app = test_app().await;
    let owner = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;
    let definition_id = create_net(&app, &owner).await;
    let session_id = start_session(&app, &owner, &definition_id).await;
    let connection_id = first_connection_id(&app, &owner, &session_id).await;

    let (status, body) = send_json(
        app.router(),
        "POST",
        &format!("/api/net-sessions/{session_id}/check-ins"),
        Some(json!({
            "callsign": "W9XYZ",
            "via": { "kind": "connection", "connectionId": connection_id }
        })),
        Some(&owner),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    let check_in_id = body["roster"][0]["checkInId"]
        .as_str()
        .expect("checkInId")
        .to_owned();
    assert_eq!(body["roster"][0]["via"]["connectionId"], connection_id);

    let (status, body) = send_json(
        app.router(),
        "PUT",
        &format!("/api/net-sessions/{session_id}/check-ins/{check_in_id}"),
        Some(json!({
            "callsign": "W9XYZ",
            "via": { "kind": "unlisted", "text": "Bill's phone patch" },
            "expectedVersion": 1
        })),
        Some(&owner),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let entry = &body["roster"][0];
    let correction = entry["corrections"]
        .as_array()
        .expect("corrections")
        .iter()
        .find(|c| c["field"] == "via")
        .expect("a via correction");
    assert_eq!(correction["from"], "HF — 14.230 MHz");
    assert_eq!(
        correction["to"], "Bill's phone patch",
        "free text round-trips byte for byte"
    );
    assert!(
        !correction["from"]
            .as_str()
            .expect("from")
            .contains(&connection_id),
        "a corrected via renders its LABEL on both sides, never a UUID"
    );
    assert_eq!(entry["via"]["kind"], "unlisted");
}

#[tokio::test]
async fn an_edit_that_omits_via_keeps_the_stored_one_and_an_explicit_null_clears_it() {
    // partial-body semantics, which `via` inherits rather than
    // inventing: ABSENT keeps, `null` clears. Reading absent as "clear" is the
    // field-wipe defect, one field over.
    let app = test_app().await;
    let owner = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;
    let definition_id = create_net(&app, &owner).await;
    let session_id = start_session(&app, &owner, &definition_id).await;
    let connection_id = first_connection_id(&app, &owner, &session_id).await;

    let (status, body) = send_json(
        app.router(),
        "POST",
        &format!("/api/net-sessions/{session_id}/check-ins"),
        Some(json!({
            "callsign": "W9XYZ",
            "via": { "kind": "connection", "connectionId": connection_id }
        })),
        Some(&owner),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    let check_in_id = body["roster"][0]["checkInId"]
        .as_str()
        .expect("checkInId")
        .to_owned();

    let (status, body) = send_json(
        app.router(),
        "PUT",
        &format!("/api/net-sessions/{session_id}/check-ins/{check_in_id}"),
        Some(json!({ "callsign": "W9XYZ", "name": "Maria", "expectedVersion": 1 })),
        Some(&owner),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        body["roster"][0]["via"]["connectionId"], connection_id,
        "an omitted key keeps the stored way in"
    );

    let (status, body) = send_json(
        app.router(),
        "PUT",
        &format!("/api/net-sessions/{session_id}/check-ins/{check_in_id}"),
        Some(json!({ "callsign": "W9XYZ", "via": null, "expectedVersion": 2 })),
        Some(&owner),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let entry = body["roster"][0].as_object().expect("an entry object");
    assert!(
        entry.contains_key("callsign") && entry.contains_key("version"),
        "the entry serialized its always-present keys, so absence below means absence"
    );
    assert!(
        !entry.contains_key("via"),
        "an explicit null clears it, and an absent value omits the key"
    );
}

#[tokio::test]
async fn a_bad_relaying_callsign_names_its_own_field_and_appends_nothing() {
    // `ApiError::CallsignInvalid` would compile, validate correctly, and then
    // tell the operator their CALLSIGN is wrong on a request whose callsign was
    // fine — the defect a rejected `via` answering `/errors/note-invalid` had.
    // An error names the FAULTING field.
    let app = test_app().await;
    let owner = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;
    let definition_id = create_net(&app, &owner).await;
    let session_id = start_session(&app, &owner, &definition_id).await;
    let check_in_id = add_check_in(&app, &owner, &session_id, "N1CCK").await;

    for hostile in ["not a callsign", "W1AW/A/B/C/D", "\u{202e}W1AW"] {
        let (status, body) = send_json(
            app.router(),
            "PUT",
            &format!("/api/net-sessions/{session_id}/check-ins/{check_in_id}"),
            Some(json!({
                "callsign": "N1CCK",
                "relayedBy": hostile,
                "expectedVersion": 1
            })),
            Some(&owner),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "rejected: {hostile}");
        assert_eq!(
            body["type"], "/errors/relayed-by-invalid",
            "the problem names the RELAYING station, not the request's own callsign, \
             which was valid on every one of these requests"
        );
    }

    // A blank relaying station CLEARS rather than failing — the PUT-replace
    // idiom every other text field here uses — and none of the three refusals
    // above appended an event.
    let (status, after) = send_json(
        app.router(),
        "PUT",
        &format!("/api/net-sessions/{session_id}/check-ins/{check_in_id}"),
        Some(json!({
            "callsign": "N1CCK",
            "relayedBy": "   ",
            "expectedVersion": 1
        })),
        Some(&owner),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let entry = entry_of(&after, &check_in_id);
    assert!(
        entry.as_object().expect("obj").get("relayedBy").is_none(),
        "a blank relaying station folds to absent, not to an empty string"
    );
    assert_eq!(
        entry["version"], 2,
        "exactly ONE event was appended — the three refusals appended none"
    );
}

#[tokio::test]
async fn a_bad_relaying_callsign_on_the_add_path_also_names_its_own_field() {
    // The test above covers PUT only, but `add_check_in` runs its OWN
    // `parse_edit_relayed_by` call — a second, independent boundary. Where that
    // parse sits relative to the staff/self branch has moved, and the staff arm
    // must keep answering 400 `/errors/relayed-by-invalid` across the move.
    let app = test_app().await;
    let owner = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;
    let definition_id = create_net(&app, &owner).await;
    let session_id = start_session(&app, &owner, &definition_id).await;

    for hostile in ["not a callsign", "W1AW/A/B/C/D", "\u{202e}W1AW"] {
        let (status, body) = send_json(
            app.router(),
            "POST",
            &format!("/api/net-sessions/{session_id}/check-ins"),
            Some(json!({ "callsign": "K9XYZ", "relayedBy": hostile })),
            Some(&owner),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "rejected: {hostile}");
        assert_eq!(
            body["type"], "/errors/relayed-by-invalid",
            "the ADD path names the RELAYING station too, not the request's own \
             callsign, which was valid on every one of these requests"
        );
    }

    // None of the three appended: the first REAL add is a 201 onto an empty
    // roster of exactly one.
    let (status, body) = send_json(
        app.router(),
        "POST",
        &format!("/api/net-sessions/{session_id}/check-ins"),
        Some(json!({ "callsign": "K9XYZ" })),
        Some(&owner),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(
        body["roster"].as_array().expect("roster").len(),
        1,
        "the three refusals appended nothing"
    );
}

#[tokio::test]
async fn a_relaying_station_is_normalized_and_needs_no_matching_account() {
    // The cheapest wrong implementation reuses
    // `added_by: Option<Uuid>` and makes the ORDINARY on-air case — a relaying
    // station with no NetRoll account — unrepresentable. `parse_callsign` is
    // also the right guard rather than a bare `String`: it normalizes, so a
    // relaying station is comparable against a roster callsign without a second
    // rule.
    let app = test_app().await;
    let owner = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;
    let definition_id = create_net(&app, &owner).await;
    let session_id = start_session(&app, &owner, &definition_id).await;
    let check_in_id = add_check_in(&app, &owner, &session_id, "N1CCK").await;

    let (status, after) = send_json(
        app.router(),
        "PUT",
        &format!("/api/net-sessions/{session_id}/check-ins/{check_in_id}"),
        Some(json!({
            "callsign": "N1CCK",
            // Lower case, and no account in this app holds it.
            "relayedBy": "  w3rel  ",
            "expectedVersion": 1
        })),
        Some(&owner),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let entry = entry_of(&after, &check_in_id);
    assert_eq!(
        entry["relayedBy"], "W3REL",
        "normalized by the SAME guard the check-in's own callsign uses"
    );
    assert_ne!(
        entry["relayedBy"], entry["callsign"],
        "the relaying station is a DIFFERENT station from the one that checked in"
    );
}

#[tokio::test]
async fn the_way_in_and_the_relaying_station_never_read_or_write_each_other() {
    // Two fields on one entry, moved one at a time: changing
    // either must leave the other exactly as it was. The failure this guards
    // against is the one ruling #6 exists to prevent — the two facts sharing a
    // field and becoming ambiguous the first time they differ.
    let app = test_app().await;
    let owner = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;
    let definition_id = create_net(&app, &owner).await;
    let session_id = start_session(&app, &owner, &definition_id).await;
    let check_in_id = add_check_in(&app, &owner, &session_id, "N1CCK").await;

    // Set ONLY the relaying station; the way in must stay unrecorded.
    let (status, after) = send_json(
        app.router(),
        "PUT",
        &format!("/api/net-sessions/{session_id}/check-ins/{check_in_id}"),
        Some(json!({
            "callsign": "N1CCK", "relayedBy": "W3REL", "expectedVersion": 1
        })),
        Some(&owner),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let entry = entry_of(&after, &check_in_id);
    assert_eq!(entry["relayedBy"], "W3REL");
    assert!(
        entry.as_object().expect("obj").get("via").is_none(),
        "setting the relaying station did not invent a way in"
    );

    // Now set ONLY the way in; the relaying station must survive untouched.
    let (status, after) = send_json(
        app.router(),
        "PUT",
        &format!("/api/net-sessions/{session_id}/check-ins/{check_in_id}"),
        Some(json!({
            "callsign": "N1CCK",
            "via": { "kind": "unlisted", "text": "Bill's phone patch" },
            "expectedVersion": 2
        })),
        Some(&owner),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let entry = entry_of(&after, &check_in_id);
    assert_eq!(entry["via"]["text"], "Bill's phone patch");
    assert_eq!(
        entry["relayedBy"], "W3REL",
        "setting the way in did not read or write the relaying station"
    );

    // And clearing the way in leaves the relaying station alone.
    let (status, after) = send_json(
        app.router(),
        "PUT",
        &format!("/api/net-sessions/{session_id}/check-ins/{check_in_id}"),
        Some(json!({
            "callsign": "N1CCK", "via": Value::Null, "expectedVersion": 3
        })),
        Some(&owner),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let entry = entry_of(&after, &check_in_id);
    assert!(entry.as_object().expect("obj").get("via").is_none());
    assert_eq!(entry["relayedBy"], "W3REL");
}

#[tokio::test]
async fn an_edited_relaying_station_derives_its_own_correction_annotation() {
    // A relaying station is a corrected MIS-ENTRY — "it was
    // W1ABC who relayed her, not W1ABD" — which is the `via`/`callsign` class,
    // NOT the note class. The token is `relayedBy`, and it is what the console's
    // field-label map keys on.
    let app = test_app().await;
    let owner = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;
    let definition_id = create_net(&app, &owner).await;
    let session_id = start_session(&app, &owner, &definition_id).await;
    let check_in_id = add_check_in(&app, &owner, &session_id, "N1CCK").await;

    for (version, call) in [(1, "W1ABC"), (2, "W1ABD")] {
        let (status, _) = send_json(
            app.router(),
            "PUT",
            &format!("/api/net-sessions/{session_id}/check-ins/{check_in_id}"),
            Some(json!({
                "callsign": "N1CCK", "relayedBy": call, "expectedVersion": version
            })),
            Some(&owner),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
    }

    let (status, after) = send_json(
        app.router(),
        "GET",
        &format!("/api/net-sessions/{session_id}"),
        None,
        Some(&owner),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let entry = entry_of(&after, &check_in_id);
    let corrections: Vec<&Value> = entry["corrections"]
        .as_array()
        .expect("corrections")
        .iter()
        .filter(|c| c["field"] == "relayedBy")
        .collect();
    assert_eq!(
        corrections.len(),
        2,
        "a set and a change are both corrections"
    );
    assert_eq!(corrections[1]["from"], "W1ABC");
    assert_eq!(corrections[1]["to"], "W1ABD");
}
