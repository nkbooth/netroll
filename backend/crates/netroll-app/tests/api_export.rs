// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
//! Full-loop API tests for the CSV & ADIF session export: real
//! router, real Postgres (testcontainers), capturing mailer. Asserts status
//! codes, response headers, and the raw file bytes' STRUCTURE (header row,
//! neutralized cells, ADIF framing) — never message prose (house TDD rule).

use std::sync::Arc;
use std::sync::Mutex;

use axum::Router;
use axum::body::Body;
use axum::http::{HeaderMap, Request, StatusCode, header};
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
        pool: pool.clone(),
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

/// Issues a raw (non-JSON) request and returns the status, response headers, and
/// the raw body bytes — the file-download path the export endpoint serves.
async fn send_raw(
    app: &TestApp,
    uri: &str,
    cookie: Option<&str>,
) -> (StatusCode, HeaderMap, Vec<u8>) {
    let mut builder = Request::builder().method("GET").uri(uri);
    if let Some(cookie) = cookie {
        builder = builder.header(header::COOKIE, cookie);
    }
    let request = builder.body(Body::empty()).expect("build request");
    let response = app.router().oneshot(request).await.expect("route request");
    let status = response.status();
    let headers = response.headers().clone();
    let bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024)
        .await
        .expect("body");
    (status, headers, bytes.to_vec())
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

async fn grant(app: &TestApp, cookie: &str, session_id: &str, callsign: &str, role: &str) {
    let (status, _) = send_json(
        app.router(),
        "POST",
        &format!("/api/net-sessions/{session_id}/roles"),
        Some(json!({ "callsign": callsign, "role": role })),
        Some(cookie),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
}

async fn add_check_in(app: &TestApp, cookie: &str, session_id: &str, body: Value) -> StatusCode {
    send_json(
        app.router(),
        "POST",
        &format!("/api/net-sessions/{session_id}/check-ins"),
        Some(body),
        Some(cookie),
    )
    .await
    .0
}

async fn close_session(app: &TestApp, cookie: &str, session_id: &str) {
    let (status, _) = send_json(
        app.router(),
        "POST",
        &format!("/api/net-sessions/{session_id}/close"),
        None,
        Some(cookie),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
}

fn content_type(headers: &HeaderMap) -> String {
    headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_owned()
}

fn content_disposition(headers: &HeaderMap) -> String {
    headers
        .get(header::CONTENT_DISPOSITION)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_owned()
}

// --- CSV -------------------------------------------------------

#[tokio::test]
async fn owner_downloads_a_csv_with_the_header_and_a_roster_row() {
    let app = test_app().await;
    let owner = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;
    let definition_id = create_net(&app, &owner).await;
    let session_id = start_session(&app, &owner, &definition_id).await;
    assert_eq!(
        add_check_in(&app, &owner, &session_id, json!({ "callsign": "n1cck" })).await,
        StatusCode::CREATED
    );
    close_session(&app, &owner, &session_id).await;

    let (status, headers, bytes) = send_raw(
        &app,
        &format!("/api/net-sessions/{session_id}/export?format=csv"),
        Some(&owner),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(content_type(&headers), "text/csv; charset=utf-8");
    assert!(
        content_disposition(&headers).starts_with("attachment;"),
        "csv export is an attachment download"
    );
    let body = String::from_utf8(bytes).expect("csv is utf-8");
    let mut lines = body.lines();
    assert_eq!(
        lines.next().expect("header"),
        // The fifteenth column is `via`, the LABEL of the way in this
        // station arrived on — beside the notes, before the timestamp. Empty for
        // a check-in nobody recorded a way in for, which is a different fact
        // from the ADIF-export connection and is never filled in from it.
        //
        // The sixteenth is `relayed_by`, WHICH STATION passed that
        // station's traffic — a separate column because it is a separate fact,
        // and one the operator's own log keeps even though ADIF has no tag for
        // it and the public page never shows it. Empty means NOT RELAYED, and it
        // is never borrowed from `entering_operator`, which names the account
        // that typed the row.
        "callsign,name,location,grid,source,entering_operator,signal_report,staying,precedence,traffic,worked,notes,public_note,via,relayed_by,checked_in_at"
    );
    let row = lines.next().expect("a roster row");
    // The single check-in appears; the entering operator resolves to the owner's
    // callsign (staff-entered), proving the accounts lookup wired through.
    assert!(
        row.starts_with("N1CCK,"),
        "roster row for the check-in: {row}"
    );
    assert!(
        row.contains(",staff,W1AW,"),
        "entering operator resolves to the owner callsign: {row}"
    );
}

#[tokio::test]
async fn the_csv_and_adif_exports_carry_a_recorded_grid_end_to_end() {
    // The grid the operator recorded reaches the exported
    // bytes — CSV cell and ADIF <GRIDSQUARE> — through the real handler, fold
    // and serializer, not just the pure `to_csv` unit test.
    let app = test_app().await;
    let owner = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;
    let definition_id = create_net(&app, &owner).await;
    let session_id = start_session(&app, &owner, &definition_id).await;
    assert_eq!(
        add_check_in(
            &app,
            &owner,
            &session_id,
            json!({ "callsign": "n1cck", "location": "Hartford CT", "grid": "fn31PR" })
        )
        .await,
        StatusCode::CREATED
    );
    // A second station with NO grid, so the empty-cell claim is really exercised.
    assert_eq!(
        add_check_in(&app, &owner, &session_id, json!({ "callsign": "w9xyz" })).await,
        StatusCode::CREATED
    );
    close_session(&app, &owner, &session_id).await;

    let (status, _headers, bytes) = send_raw(
        &app,
        &format!("/api/net-sessions/{session_id}/export?format=csv"),
        Some(&owner),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let body = String::from_utf8(bytes).expect("csv is utf-8");
    let mut lines = body.lines().skip(1);
    let with_grid = lines.next().expect("the grid-bearing row");
    assert!(
        with_grid.starts_with("N1CCK,,Hartford CT,FN31pr,staff,"),
        "the canonical grid rides its own cell after location: {with_grid}"
    );
    let without_grid = lines.next().expect("the grid-less row");
    assert!(
        without_grid.starts_with("W9XYZ,,,,staff,"),
        "an absent grid is an EMPTY cell, never \"None\": {without_grid}"
    );

    let (status, _headers, bytes) = send_raw(
        &app,
        &format!("/api/net-sessions/{session_id}/export?format=adif"),
        Some(&owner),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let adif = String::from_utf8(bytes).expect("adif is utf-8");
    assert!(
        adif.contains("<GRIDSQUARE:6>FN31pr"),
        "the grid rides the ADIF record: {adif}"
    );
    assert_eq!(
        adif.matches("<GRIDSQUARE:").count(),
        1,
        "only the grid-bearing station emits the tag: {adif}"
    );
}

#[tokio::test]
async fn a_formula_injection_field_is_neutralized_in_the_raw_csv_bytes() {
    let app = test_app().await;
    let owner = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;
    let definition_id = create_net(&app, &owner).await;
    let session_id = start_session(&app, &owner, &definition_id).await;
    // A hostile name/location beginning with spreadsheet-formula characters.
    assert_eq!(
        add_check_in(
            &app,
            &owner,
            &session_id,
            json!({ "callsign": "n1cck", "name": "=cmd|calc", "location": "+ping" })
        )
        .await,
        StatusCode::CREATED
    );
    close_session(&app, &owner, &session_id).await;

    let (status, _headers, bytes) = send_raw(
        &app,
        &format!("/api/net-sessions/{session_id}/export?format=csv"),
        Some(&owner),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let body = String::from_utf8(bytes).expect("csv is utf-8");
    // The neutralizing apostrophe is present in the raw bytes so a spreadsheet
    // cannot evaluate the cell as a formula.
    assert!(
        body.contains(",'=cmd|calc,"),
        "name cell neutralized: {body}"
    );
    assert!(
        body.contains(",'+ping,"),
        "location cell neutralized: {body}"
    );
}

// --- ADIF ------------------------------------------------------------

#[tokio::test]
async fn owner_downloads_a_valid_adif_document() {
    let app = test_app().await;
    let owner = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;
    let definition_id = create_net(&app, &owner).await;
    let session_id = start_session(&app, &owner, &definition_id).await;
    assert_eq!(
        add_check_in(&app, &owner, &session_id, json!({ "callsign": "n1cck" })).await,
        StatusCode::CREATED
    );
    close_session(&app, &owner, &session_id).await;

    let (status, headers, bytes) = send_raw(
        &app,
        &format!("/api/net-sessions/{session_id}/export?format=adif"),
        Some(&owner),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(content_type(&headers), "text/plain; charset=utf-8");
    assert!(content_disposition(&headers).contains(".adi"));
    let body = String::from_utf8(bytes).expect("adif is utf-8");
    assert!(body.contains("<EOH>"), "header terminated by EOH");
    assert!(body.contains("<EOR>"), "one record terminated by EOR");
    assert!(body.contains("<CALL:"), "the record carries a CALL field");
    // The 20m/ssb definition maps to real ADIF band/mode enums.
    // band/mode/FREQ are the session's POSITION-ZERO CONNECTION's — the
    // top-level snapshot fields they used to be read from no longer exist, so
    // this assertion is what proves the export followed them across.
    assert!(body.contains("<BAND:3>20m"));
    assert!(body.contains("<MODE:3>SSB"));
    assert!(
        body.contains("<FREQ:5>14.23"),
        "FREQ comes from the connection, not from a retired session column: {body}"
    );
}

#[tokio::test]
async fn an_adif_export_finds_the_rf_way_in_even_when_it_is_not_first() {
    // Both ADIF call sites once took
    // `connections.first()`, so a net whose owner leads with EchoLink and lists
    // HF SECOND exported no BAND, no MODE and no FREQ — discarding an RF
    // connection that genuinely exists — and still answered 200 with a `.adi`
    // most logbook software rejects. Position zero is the way the owner leads
    // with; it is not "the radio way", and the browser's own presenter
    // (`rfConnection`) has always searched for the first RF entry rather than
    // assuming position zero.
    let app = test_app().await;
    let owner = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;
    let definition_id = create_net(&app, &owner).await;
    let version: i64 = {
        let (status, body) = send_json(
            app.router(),
            "GET",
            &format!("/api/net-definitions/{definition_id}"),
            None,
            Some(&owner),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        body["definitionVersion"].as_i64().expect("version")
    };
    let (status, body) = send_json(
        app.router(),
        "PUT",
        &format!("/api/net-definitions/{definition_id}/connections"),
        Some(json!({
            "expectedDefinitionVersion": version,
            "connections": [
                { "kind": "echolink", "node": "12345" },
                { "kind": "hf", "plannedFrequencyHz": 14_230_000, "band": "20m", "mode": "ssb" }
            ]
        })),
        Some(&owner),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "connection replace: {body}");

    let session_id = start_session(&app, &owner, &definition_id).await;
    assert_eq!(
        add_check_in(&app, &owner, &session_id, json!({ "callsign": "n1cck" })).await,
        StatusCode::CREATED
    );
    close_session(&app, &owner, &session_id).await;

    let (status, _headers, bytes) = send_raw(
        &app,
        &format!("/api/net-sessions/{session_id}/export?format=adif"),
        Some(&owner),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let body = String::from_utf8(bytes).expect("adif is utf-8");
    assert!(
        body.contains("<BAND:3>20m"),
        "the RF way in is at position ONE and still describes the QSO: {body}"
    );
    assert!(body.contains("<MODE:3>SSB"), "{body}");
    assert!(body.contains("<FREQ:5>14.23"), "{body}");
}

/// The id of the net's position-zero connection, read from the definition the
/// session snapshot was frozen from (the snapshot keeps the definition's ids).
async fn first_connection_id(app: &TestApp, cookie: &str, definition_id: &str) -> String {
    let (status, body) = send_json(
        app.router(),
        "GET",
        &format!("/api/net-definitions/{definition_id}"),
        None,
        Some(cookie),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    body["connections"][0]["id"]
        .as_str()
        .expect("connection id")
        .to_owned()
}

/// The fixture shape: one station checked in over HF, the net QSYs
/// 14.230 → 14.250 on that same connection, a second station checks in over it.
/// Only a move BETWEEN two check-ins separates the frozen, the live and the
/// correct answer — a never-moving fixture is green under all three.
async fn closed_session_with_a_qsy_between_two_check_ins(app: &TestApp, owner: &str) -> String {
    let definition_id = create_net(app, owner).await;
    let hf = first_connection_id(app, owner, &definition_id).await;
    let session_id = start_session(app, owner, &definition_id).await;
    let via = json!({ "kind": "connection", "connectionId": hf });
    assert_eq!(
        add_check_in(
            app,
            owner,
            &session_id,
            json!({ "callsign": "n1cck", "via": via })
        )
        .await,
        StatusCode::CREATED
    );
    let (status, body) = send_json(
        app.router(),
        "POST",
        &format!("/api/net-sessions/{session_id}/frequency"),
        Some(json!({ "connectionId": hf, "operatingFrequency": "14.250" })),
        Some(owner),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        add_check_in(
            app,
            owner,
            &session_id,
            json!({ "callsign": "w1abc", "via": via })
        )
        .await,
        StatusCode::CREATED
    );
    close_session(app, owner, &session_id).await;
    session_id
}

#[tokio::test]
async fn an_adif_export_stamps_each_qso_with_the_frequency_it_was_worked_on() {
    // If the net changes frequency after traffic is passed, the worked
    // stations should not change. This file reaches LoTW and QRZ and cannot be
    // recalled.
    let app = test_app().await;
    let owner = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;
    let session_id = closed_session_with_a_qsy_between_two_check_ins(&app, &owner).await;

    let (status, _headers, bytes) = send_raw(
        &app,
        &format!("/api/net-sessions/{session_id}/export?format=adif"),
        Some(&owner),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let body = String::from_utf8(bytes).expect("adif is utf-8");
    let records: Vec<&str> = body
        .split("<EOR>")
        .filter(|record| record.contains("<CALL:"))
        .collect();
    assert_eq!(records.len(), 2, "two QSO records: {body}");
    let (before_move, after_move) = (records[0], records[1]);
    assert!(before_move.contains("<CALL:5>N1CCK"), "{before_move}");
    assert!(after_move.contains("<CALL:5>W1ABC"), "{after_move}");
    assert!(
        before_move.contains("<FREQ:5>14.23"),
        "the pre-move QSO keeps the frequency it was worked on: {before_move}"
    );
    assert!(
        after_move.contains("<FREQ:5>14.25"),
        "the post-move QSO carries the frequency the net moved to: {after_move}"
    );
    // BAND is the owner's token on both and is not re-derived.
    assert!(before_move.contains("<BAND:3>20m") && after_move.contains("<BAND:3>20m"));
}

#[tokio::test]
async fn a_csv_exports_via_cell_names_the_frequency_the_station_was_worked_on() {
    // The CSV half. The `via` column embeds the frequency for
    // an RF way in, so a label resolved against the net's FINAL set tells a
    // pre-QSY station it was worked on a frequency it never used.
    let app = test_app().await;
    let owner = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;
    let session_id = closed_session_with_a_qsy_between_two_check_ins(&app, &owner).await;

    let (status, _headers, bytes) = send_raw(
        &app,
        &format!("/api/net-sessions/{session_id}/export?format=csv"),
        Some(&owner),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let body = String::from_utf8(bytes).expect("csv is utf-8");
    let via_cell = |line: usize| -> &str {
        body.lines()
            .nth(line)
            .expect("a data row")
            .split(',')
            .nth(13)
            .expect("the via cell")
    };
    let (before_move, after_move) = (via_cell(1), via_cell(2));
    assert_ne!(
        before_move, after_move,
        "two stations either side of a QSY must not read the same frequency"
    );
    assert_eq!(before_move, "HF — 14.230 MHz");
    assert_eq!(after_move, "HF — 14.250 MHz");
}

#[tokio::test]
async fn an_internet_only_nets_adif_omits_band_mode_and_freq_rather_than_inventing_them() {
    // The other half, and the case that must stay true: a net with NO RF
    // way in has no band, no mode and no frequency, and ADIF omits a tag whose
    // value it has no equivalent for rather than emitting an empty one.
    let app = test_app().await;
    let owner = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;
    let definition_id = create_net(&app, &owner).await;
    let version: i64 = {
        let (status, body) = send_json(
            app.router(),
            "GET",
            &format!("/api/net-definitions/{definition_id}"),
            None,
            Some(&owner),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        body["definitionVersion"].as_i64().expect("version")
    };
    let (status, body) = send_json(
        app.router(),
        "PUT",
        &format!("/api/net-definitions/{definition_id}/connections"),
        Some(json!({
            "expectedDefinitionVersion": version,
            "connections": [
                { "kind": "echolink", "node": "12345" },
                { "kind": "dmr", "talkgroup": "31337", "network": "Brandmeister" }
            ]
        })),
        Some(&owner),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "connection replace: {body}");

    let session_id = start_session(&app, &owner, &definition_id).await;
    assert_eq!(
        add_check_in(&app, &owner, &session_id, json!({ "callsign": "n1cck" })).await,
        StatusCode::CREATED
    );
    close_session(&app, &owner, &session_id).await;

    let (status, _headers, bytes) = send_raw(
        &app,
        &format!("/api/net-sessions/{session_id}/export?format=adif"),
        Some(&owner),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let body = String::from_utf8(bytes).expect("adif is utf-8");
    assert!(body.contains("<CALL:"), "the QSO is still exported: {body}");
    for tag in ["<BAND:", "<MODE:", "<FREQ:"] {
        assert!(
            !body.contains(tag),
            "{tag} is omitted, never emitted empty: {body}"
        );
    }
}

// --- Authorization ---------------------------------------------------

#[tokio::test]
async fn a_logger_is_denied_export_with_403() {
    let app = test_app().await;
    let owner = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;
    let definition_id = create_net(&app, &owner).await;
    let session_id = start_session(&app, &owner, &definition_id).await;
    let logger = sign_in_consent_callsign(&app, "logger@example.com", "n1ale").await;
    grant(&app, &owner, &session_id, "n1ale", "logger").await;

    for format in ["csv", "adif"] {
        let (status, _headers, bytes) = send_raw(
            &app,
            &format!("/api/net-sessions/{session_id}/export?format={format}"),
            Some(&logger),
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN, "logger denied {format}");
        // A denied export returns no file body — the problem+json is not a file.
        let body = String::from_utf8(bytes).expect("utf-8");
        assert!(!body.contains("<EOH>"));
        assert!(!body.contains("callsign,name,location"));
    }
}

#[tokio::test]
async fn a_participant_is_denied_export_with_403() {
    let app = test_app().await;
    let owner = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;
    let definition_id = create_net(&app, &owner).await;
    let session_id = start_session(&app, &owner, &definition_id).await;
    // A signed-in, consented, callsign-holding but ungranted account: Participant.
    let participant = sign_in_consent_callsign(&app, "participant@example.com", "n1ale").await;

    let (status, _headers, _bytes) = send_raw(
        &app,
        &format!("/api/net-sessions/{session_id}/export?format=csv"),
        Some(&participant),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn a_missing_session_is_404_before_the_403_authz_check() {
    let app = test_app().await;
    // A callsign-holding account that owns nothing; the session id does not exist.
    let stranger = sign_in_consent_callsign(&app, "stranger@example.com", "n1ale").await;
    let missing = uuid::Uuid::now_v7();
    let (status, _headers, _bytes) = send_raw(
        &app,
        &format!("/api/net-sessions/{missing}/export?format=csv"),
        Some(&stranger),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn an_unauthenticated_export_is_401() {
    let app = test_app().await;
    let owner = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;
    let definition_id = create_net(&app, &owner).await;
    let session_id = start_session(&app, &owner, &definition_id).await;

    let (status, _headers, _bytes) = send_raw(
        &app,
        &format!("/api/net-sessions/{session_id}/export?format=csv"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn an_absent_or_unknown_format_is_400() {
    let app = test_app().await;
    let owner = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;
    let definition_id = create_net(&app, &owner).await;
    let session_id = start_session(&app, &owner, &definition_id).await;

    for uri in [
        format!("/api/net-sessions/{session_id}/export"),
        format!("/api/net-sessions/{session_id}/export?format=xlsx"),
    ] {
        // ApiError::Validation maps to 400 in this codebase's problem+json layer.
        let (status, _headers, _bytes) = send_raw(&app, &uri, Some(&owner)).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "bad format {uri}");
    }
}

// Keep the pool handle used so the harness struct field is not dead in some
// configurations (the DB side-effect assertions above already read via HTTP).
#[tokio::test]
async fn export_reads_do_not_mutate_the_session_row() {
    let app = test_app().await;
    let owner = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;
    let definition_id = create_net(&app, &owner).await;
    let session_id = start_session(&app, &owner, &definition_id).await;
    close_session(&app, &owner, &session_id).await;

    let last_seq_before: i64 =
        sqlx::query_scalar("SELECT last_seq FROM net_sessions WHERE id = $1")
            .bind(uuid::Uuid::parse_str(&session_id).unwrap())
            .fetch_one(&app.pool)
            .await
            .expect("last_seq");

    let _ = send_raw(
        &app,
        &format!("/api/net-sessions/{session_id}/export?format=adif"),
        Some(&owner),
    )
    .await;

    let last_seq_after: i64 = sqlx::query_scalar("SELECT last_seq FROM net_sessions WHERE id = $1")
        .bind(uuid::Uuid::parse_str(&session_id).unwrap())
        .fetch_one(&app.pool)
        .await
        .expect("last_seq");
    assert_eq!(last_seq_before, last_seq_after, "export is a pure read");
}
