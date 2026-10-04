// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
//! Full-loop WebSocket tests for the live session transport: real router, real
//! Postgres, a REAL bound server (WS cannot use `tower::oneshot`) and a real
//! `tokio-tungstenite` client carrying the owner's cookie on the handshake.
//! Asserts `seq` ordering, the snapshot-vs-replay branch, exactly-once under
//! concurrency, upgrade authz, payload parity and fan-out — never prose.

use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::Mutex;
use std::time::Duration;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use futures_util::{SinkExt, StreamExt};
use netroll_app::http::{AppState, api_router};
use netroll_domain::callsign::parse_callsign;
use netroll_domain::consent::CURRENT_TERMS_VERSION;
use netroll_domain::event::{SessionEvent, SessionEventBody};
use netroll_domain::fold::{SessionState, replay};
use netroll_domain::ports::{BoxFuture, MailError, Mailer};
use serde_json::{Value, json};
use sqlx::PgPool;
use sqlx::postgres::PgPoolOptions;
use testcontainers_modules::postgres::Postgres;
use testcontainers_modules::testcontainers::ContainerAsync;
use testcontainers_modules::testcontainers::runners::AsyncRunner;
use tokio::net::TcpListener;
use tokio_tungstenite::tungstenite::Error as WsError;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::protocol::CloseFrame;
use tower::ServiceExt;
use uuid::Uuid;

type WsStream =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

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

    /// Binds a real server (WS upgrades cannot use `tower::oneshot`) sharing the
    /// same `AppState` — hence the same pool AND the same `SessionHub` Arc, so a
    /// `publish` from an HTTP handler reaches these WS subscribers.
    async fn spawn_server(&self) -> SocketAddr {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let addr = listener.local_addr().expect("addr");
        let router = api_router(self.state.clone());
        tokio::spawn(async move {
            axum::serve(listener, router.into_make_service())
                .await
                .expect("serve");
        });
        addr
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

/// Signs in, records consent, reserves a callsign — the full owner gate.
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

fn definition_json(title: &str) -> Value {
    json!({
        "title": title,
        "description": "Weekly NTS traffic",
        "connections": [
            { "kind": "hf", "plannedFrequencyHz": 14_230_000, "band": "20m", "mode": "ssb" }
        ],
        "country": "USA",
        "state": "CT",
        "grid": "fn31pr",
        "netCategory": "traffic",
        "netType": "open",
        "expectedDuration": "90"
    })
}

async fn create_net(app: &TestApp, cookie: &str, title: &str) -> String {
    let (status, body) = send_json(
        app.router(),
        "POST",
        "/api/net-definitions",
        Some(definition_json(title)),
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

/// Creates a fresh net + started session, returning its id.
async fn fresh_started_session(app: &TestApp, cookie: &str, title: &str) -> String {
    let definition_id = create_net(app, cookie, title).await;
    start_session(app, cookie, &definition_id).await
}

async fn connect_ws(
    addr: SocketAddr,
    session_id: &str,
    since: Option<u64>,
    cookie: Option<&str>,
) -> Result<WsStream, WsError> {
    let query = match since {
        Some(n) => format!("?since={n}"),
        None => String::new(),
    };
    let url = format!("ws://{addr}/api/net-sessions/{session_id}/ws{query}");
    let mut request = url.into_client_request().expect("client request");
    if let Some(cookie) = cookie {
        request
            .headers_mut()
            .insert(header::COOKIE, cookie.parse().expect("cookie header"));
    }
    let (stream, _response) = tokio_tungstenite::connect_async(request).await?;
    Ok(stream)
}

/// Like [`connect_ws`], but sets an `Origin` header — the shape a real
/// browser's WS handshake carries, and the one dimension `connect_ws`
/// deliberately omits (a bare `tokio-tungstenite` client sends no `Origin` at
/// all, exercising the "non-browser client" allow-through path instead).
async fn connect_ws_with_origin(
    addr: SocketAddr,
    session_id: &str,
    cookie: &str,
    origin: &str,
) -> Result<WsStream, WsError> {
    let url = format!("ws://{addr}/api/net-sessions/{session_id}/ws");
    let mut request = url.into_client_request().expect("client request");
    request
        .headers_mut()
        .insert(header::COOKIE, cookie.parse().expect("cookie header"));
    request
        .headers_mut()
        .insert(header::ORIGIN, origin.parse().expect("origin header"));
    let (stream, _response) = tokio_tungstenite::connect_async(request).await?;
    Ok(stream)
}

/// Reads the next JSON text frame, skipping protocol Ping/Pong. Returns `None`
/// on timeout or close — never panics, so callers can assert "no further frame".
async fn try_next_json(ws: &mut WsStream, timeout_ms: u64) -> Option<Value> {
    loop {
        match tokio::time::timeout(Duration::from_millis(timeout_ms), ws.next()).await {
            Err(_) => return None,
            Ok(None) => return None,
            Ok(Some(Ok(Message::Text(text)))) => {
                return Some(serde_json::from_str(text.as_str()).expect("frame is JSON"));
            }
            Ok(Some(Ok(Message::Ping(_) | Message::Pong(_)))) => continue,
            Ok(Some(Ok(Message::Close(_)))) => return None,
            Ok(Some(Ok(_))) => continue,
            Ok(Some(Err(_))) => return None,
        }
    }
}

/// Reads the next JSON frame, asserting one arrives within a generous window.
async fn next_json(ws: &mut WsStream) -> Value {
    try_next_json(ws, 5_000)
        .await
        .expect("a frame arrives within the window")
}

/// Reads the next text frame's RAW bytes (skipping Ping/Pong), asserting one
/// arrives within a generous window — the raw-bytes counterpart to
/// [`next_json`], used to prove a redacted WS frame contains no forbidden
/// substring at the wire level (not just "the parsed struct has no field"),
/// mirroring the `/live` and `/live/events` raw-bytes redaction assertions.
async fn next_raw_text(ws: &mut WsStream, timeout_ms: u64) -> String {
    loop {
        match tokio::time::timeout(Duration::from_millis(timeout_ms), ws.next())
            .await
            .expect("a frame arrives within the window")
            .expect("stream is not closed")
            .expect("no transport error")
        {
            Message::Text(text) => return text.to_string(),
            Message::Ping(_) | Message::Pong(_) => continue,
            other => panic!("expected a text frame, got {other:?}"),
        }
    }
}

/// Reads frames until a protocol Close arrives, skipping Ping/Pong (a
/// keepalive tick that did NOT trigger eviction) — the raw-frame counterpart
/// to [`next_json`], used to assert a specific close code/reason.
async fn expect_policy_close(ws: &mut WsStream, timeout_ms: u64) -> CloseFrame<'_> {
    loop {
        match tokio::time::timeout(Duration::from_millis(timeout_ms), ws.next())
            .await
            .expect("a close frame arrives before the timeout")
        {
            Some(Ok(Message::Ping(_) | Message::Pong(_))) => continue,
            Some(Ok(Message::Close(Some(frame)))) => return frame,
            other => panic!("expected a policy-violation close frame, got {other:?}"),
        }
    }
}

/// Appends an event straight through the durable log and publishes it — the
/// exact POST-COMMIT hand-off a writer performs, used here to script deltas
/// (a stand-in for future 3a-8/3a-9 writers) without an HTTP mutation endpoint.
async fn append_and_publish(app: &TestApp, session_id: Uuid, at: u64) -> u64 {
    let body = SessionEventBody::FrequencyChanged {
        connection_id: Uuid::from_u128(0x16_04),
        operating_frequency_hz: 7_200_000,
    };
    let event = app
        .state
        .session_events
        .append(session_id, &body, None, at)
        .await
        .expect("append event");
    app.state.hub.publish(session_id, &event);
    event.seq
}

async fn session_event_count(app: &TestApp, session_id: Uuid) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM session_events WHERE session_id = $1")
        .bind(session_id)
        .fetch_one(&app.pool)
        .await
        .expect("count events")
}

#[tokio::test]
async fn owner_connect_fresh_yields_snapshot_then_ascending_deltas() {
    let app = test_app().await;
    let cookie = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;
    let session_id = fresh_started_session(&app, &cookie, "Snapshot Net").await;
    let sid = Uuid::parse_str(&session_id).unwrap();
    let addr = app.spawn_server().await;

    let mut ws = connect_ws(addr, &session_id, None, Some(&cookie))
        .await
        .expect("owner upgrades");

    // Exactly one snapshot first, carrying the folded live state + latest seq.
    let snapshot = next_json(&mut ws).await;
    assert_eq!(snapshot["type"], "snapshot");
    assert_eq!(snapshot["session"]["lifecycle"], "live");
    assert_eq!(snapshot["session"]["latestSeq"], 1);
    // The snapshot carries the session's ways IN, not one frequency.
    assert_eq!(snapshot["session"]["connections"][0]["kind"], "hf");

    // Two subsequently-appended events arrive as ascending deltas; the first
    // delta's seq is exactly snapshot.latestSeq + 1 (no gap, no overlap).
    let seq2 = append_and_publish(&app, sid, 1_700_000_100_000).await;
    let seq3 = append_and_publish(&app, sid, 1_700_000_200_000).await;
    assert_eq!((seq2, seq3), (2, 3));

    let d2 = next_json(&mut ws).await;
    assert_eq!(d2["type"], "event");
    assert_eq!(d2["seq"], 2);
    assert_eq!(d2["kind"], "frequency.changed");
    assert_eq!(d2["payload"]["operatingFrequencyHz"], 7_200_000);

    let d3 = next_json(&mut ws).await;
    assert_eq!(
        d3["seq"], 3,
        "deltas arrive in strictly ascending seq order"
    );
}

#[tokio::test]
async fn resume_since_n_sends_no_snapshot_and_only_greater_seq() {
    let app = test_app().await;
    let cookie = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;
    let session_id = fresh_started_session(&app, &cookie, "Resume Net").await;
    let sid = Uuid::parse_str(&session_id).unwrap();

    // Persist seq 2 and 3 BEFORE connecting (no live client yet).
    assert_eq!(append_and_publish(&app, sid, 1_700_000_100_000).await, 2);
    assert_eq!(append_and_publish(&app, sid, 1_700_000_200_000).await, 3);

    let addr = app.spawn_server().await;
    let mut ws = connect_ws(addr, &session_id, Some(1), Some(&cookie))
        .await
        .expect("owner upgrades");

    // No snapshot: the first frame is a delta, and only seq > 1 replays.
    let first = next_json(&mut ws).await;
    assert_eq!(first["type"], "event", "since>0 sends no snapshot");
    assert_eq!(first["seq"], 2);
    let second = next_json(&mut ws).await;
    assert_eq!(second["seq"], 3);

    // Live continues after the replay with no gap or duplicate at the boundary.
    let seq4 = append_and_publish(&app, sid, 1_700_000_300_000).await;
    assert_eq!(seq4, 4);
    let live = next_json(&mut ws).await;
    assert_eq!(
        live["seq"], 4,
        "live stream resumes after replay with no gap"
    );
}

#[tokio::test]
async fn concurrent_append_during_connect_is_delivered_exactly_once() {
    // The correctness core: an event committing concurrently with connect
    // is delivered exactly once and in order — it lands EITHER in the snapshot
    // OR as the first live delta, never both, never neither. Race connect vs
    // append across several fresh sessions to shake out the timing window.
    let app = test_app().await;
    let cookie = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;
    let addr = app.spawn_server().await;

    for i in 0..6 {
        let session_id = fresh_started_session(&app, &cookie, &format!("Race Net {i}")).await;
        let sid = Uuid::parse_str(&session_id).unwrap();

        let connect = async {
            let mut ws = connect_ws(addr, &session_id, None, Some(&cookie))
                .await
                .expect("owner upgrades");
            let snapshot = next_json(&mut ws).await;
            assert_eq!(snapshot["type"], "snapshot");
            let mut cursor = snapshot["session"]["latestSeq"].as_u64().unwrap();
            // Advance to seq 2, asserting no gap, whether it was in the snapshot
            // (cursor already 2) or arrives as the delta.
            while cursor < 2 {
                let delta = next_json(&mut ws).await;
                assert_eq!(delta["type"], "event");
                let seq = delta["seq"].as_u64().unwrap();
                assert_eq!(seq, cursor + 1, "no gap across snapshot→delta boundary");
                cursor = seq;
            }
            // No duplicate: the event already covered must not re-arrive. Any
            // further frame (there is none here) would have to be seq > 2.
            if let Some(extra) = try_next_json(&mut ws, 300).await {
                assert!(
                    extra["seq"].as_u64().unwrap() > 2,
                    "an already-delivered seq is never re-sent"
                );
            }
        };
        let append = async {
            append_and_publish(&app, sid, 1_700_000_100_000 + i as u64).await;
        };
        tokio::join!(connect, append);
    }
}

#[tokio::test]
async fn closing_a_session_broadcasts_a_session_closed_delta_post_commit() {
    // The real `close` writer publishes its `session.closed`
    // STRICTLY POST-COMMIT, and a connected owner observes it as a live delta.
    let app = test_app().await;
    let cookie = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;
    let session_id = fresh_started_session(&app, &cookie, "Close Net").await;
    let addr = app.spawn_server().await;

    let mut ws = connect_ws(addr, &session_id, None, Some(&cookie))
        .await
        .expect("owner upgrades");
    let snapshot = next_json(&mut ws).await;
    assert_eq!(snapshot["session"]["latestSeq"], 1);

    // Close over HTTP — the handler appends session.closed (seq 2) then publishes.
    let (status, _) = send_json(
        app.router(),
        "POST",
        &format!("/api/net-sessions/{session_id}/close"),
        None,
        Some(&cookie),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let delta = next_json(&mut ws).await;
    assert_eq!(delta["type"], "event");
    assert_eq!(delta["seq"], 2);
    assert_eq!(delta["kind"], "session.closed");
    assert_eq!(delta["payload"], json!({}));
}

#[tokio::test]
async fn changing_frequency_broadcasts_a_frequency_changed_delta_post_commit() {
    // The real frequency-change writer publishes its
    // frequency.changed STRICTLY POST-COMMIT, and a connected owner observes it
    // as a live delta carrying the new frequency — the first real mutation
    // command (beyond start/close) streaming over the shipped WS transport.
    let app = test_app().await;
    let cookie = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;
    let session_id = fresh_started_session(&app, &cookie, "Frequency Net").await;
    let addr = app.spawn_server().await;

    let mut ws = connect_ws(addr, &session_id, None, Some(&cookie))
        .await
        .expect("owner upgrades");
    let snapshot = next_json(&mut ws).await;
    assert_eq!(snapshot["session"]["latestSeq"], 1);
    let connection_id = snapshot["session"]["connections"][0]["id"]
        .as_str()
        .expect("the snapshot carries its connection ids")
        .to_owned();

    // Retune ONE connection over HTTP — the handler appends frequency.changed
    // (seq 2) then publishes it post-commit.
    let (status, _) = send_json(
        app.router(),
        "POST",
        &format!("/api/net-sessions/{session_id}/frequency"),
        Some(json!({ "connectionId": connection_id, "operatingFrequency": "7.200" })),
        Some(&cookie),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let delta = next_json(&mut ws).await;
    assert_eq!(delta["type"], "event");
    assert_eq!(delta["seq"], 2);
    assert_eq!(delta["kind"], "frequency.changed");
    // The delta names WHICH way in moved. Without it a three-way
    // net's subscriber learns a number and cannot tell what it belongs to.
    assert_eq!(delta["payload"]["connectionId"], connection_id);
    assert_eq!(delta["payload"]["operatingFrequencyHz"], 7_200_000);
}

#[tokio::test]
async fn adding_a_check_in_broadcasts_a_checkin_added_delta_post_commit() {
    // The real check-in writer publishes its checkin.added
    // STRICTLY POST-COMMIT, and a connected owner observes it as a live delta
    // carrying the callsign — the roster-add command streaming over the WS.
    let app = test_app().await;
    let cookie = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;
    let session_id = fresh_started_session(&app, &cookie, "Checkin Net").await;
    let addr = app.spawn_server().await;

    let mut ws = connect_ws(addr, &session_id, None, Some(&cookie))
        .await
        .expect("owner upgrades");
    let snapshot = next_json(&mut ws).await;
    assert_eq!(snapshot["session"]["latestSeq"], 1);

    // Add a check-in over HTTP — the handler appends checkin.added (seq 2) then
    // publishes it post-commit.
    let (status, _) = send_json(
        app.router(),
        "POST",
        &format!("/api/net-sessions/{session_id}/check-ins"),
        Some(json!({ "callsign": "N1CCK" })),
        Some(&cookie),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);

    let delta = next_json(&mut ws).await;
    assert_eq!(delta["type"], "event");
    assert_eq!(delta["seq"], 2);
    assert_eq!(delta["kind"], "checkin.added");
    assert_eq!(delta["payload"]["callsign"], "N1CCK");
}

#[tokio::test]
async fn two_clients_of_one_session_both_receive_a_delta() {
    // In-process fan-out reaches every connected client of the session.
    let app = test_app().await;
    let cookie = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;
    let session_id = fresh_started_session(&app, &cookie, "Fanout Net").await;
    let sid = Uuid::parse_str(&session_id).unwrap();
    let addr = app.spawn_server().await;

    let mut ws_a = connect_ws(addr, &session_id, None, Some(&cookie))
        .await
        .expect("client a upgrades");
    let mut ws_b = connect_ws(addr, &session_id, None, Some(&cookie))
        .await
        .expect("client b upgrades");
    assert_eq!(next_json(&mut ws_a).await["type"], "snapshot");
    assert_eq!(next_json(&mut ws_b).await["type"], "snapshot");

    append_and_publish(&app, sid, 1_700_000_100_000).await;

    assert_eq!(next_json(&mut ws_a).await["seq"], 2);
    assert_eq!(next_json(&mut ws_b).await["seq"], 2);
}

#[tokio::test]
async fn a_non_owner_is_refused_403_at_the_upgrade() {
    let app = test_app().await;
    let owner = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;
    let session_id = fresh_started_session(&app, &owner, "Authz Net").await;
    let stranger = sign_in_consent_callsign(&app, "stranger@example.com", "n1ale").await;
    let addr = app.spawn_server().await;

    let err = connect_ws(addr, &session_id, None, Some(&stranger))
        .await
        .expect_err("non-owner is refused before any socket opens");
    match err {
        WsError::Http(response) => assert_eq!(response.status(), StatusCode::FORBIDDEN),
        other => panic!("expected an HTTP 403 upgrade refusal, got {other:?}"),
    }
}

#[tokio::test]
async fn a_missing_session_is_404_and_no_cookie_is_401_at_the_upgrade() {
    let app = test_app().await;
    let owner = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;
    let addr = app.spawn_server().await;

    // Missing session → 404 (decided server-side before the upgrade).
    let missing = Uuid::now_v7().to_string();
    let err = connect_ws(addr, &missing, None, Some(&owner))
        .await
        .expect_err("missing session refused");
    match err {
        WsError::Http(response) => assert_eq!(response.status(), StatusCode::NOT_FOUND),
        other => panic!("expected 404, got {other:?}"),
    }

    // Unauthenticated (no cookie) → 401.
    let session_id = fresh_started_session(&app, &owner, "Unauth Net").await;
    let err = connect_ws(addr, &session_id, None, None)
        .await
        .expect_err("no cookie refused");
    match err {
        WsError::Http(response) => assert_eq!(response.status(), StatusCode::UNAUTHORIZED),
        other => panic!("expected 401, got {other:?}"),
    }
}

#[tokio::test]
async fn an_inbound_data_frame_never_mutates_and_the_stream_stays_live() {
    // The stream is structurally read-only. A client Text/Binary frame is
    // ignored — no session_events row results — and the connection stays open.
    let app = test_app().await;
    let cookie = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;
    let session_id = fresh_started_session(&app, &cookie, "ReadOnly Net").await;
    let sid = Uuid::parse_str(&session_id).unwrap();
    let addr = app.spawn_server().await;

    let mut ws = connect_ws(addr, &session_id, None, Some(&cookie))
        .await
        .expect("owner upgrades");
    assert_eq!(next_json(&mut ws).await["session"]["latestSeq"], 1);
    assert_eq!(session_event_count(&app, sid).await, 1);

    // Send a data frame that looks like a mutation command; it must be ignored.
    ws.send(Message::Text(
        json!({ "type": "close", "session": session_id }).to_string(),
    ))
    .await
    .expect("send inbound frame");

    // No new event row results; the connection is still alive (a real append
    // still streams through), proving the frame was ignored, not fatal.
    let seq2 = append_and_publish(&app, sid, 1_700_000_100_000).await;
    assert_eq!(seq2, 2);
    let delta = next_json(&mut ws).await;
    assert_eq!(delta["seq"], 2);
    assert_eq!(
        session_event_count(&app, sid).await,
        2,
        "the inbound data frame created no session_events row"
    );
}

#[tokio::test]
async fn a_lagged_receiver_recovers_a_gap_free_stream_from_postgres() {
    // A burst larger than the broadcast ring (256) while the client is not
    // reading forces the server's receiver to lag; recovery re-reads Postgres so
    // the client still ends up with a contiguous, duplicate-free 2..=N+1 stream.
    let app = test_app().await;
    let cookie = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;
    let session_id = fresh_started_session(&app, &cookie, "Lag Net").await;
    let sid = Uuid::parse_str(&session_id).unwrap();
    let addr = app.spawn_server().await;

    let mut ws = connect_ws(addr, &session_id, None, Some(&cookie))
        .await
        .expect("owner upgrades");
    assert_eq!(next_json(&mut ws).await["session"]["latestSeq"], 1);

    // Burst well past the ring capacity while the client does NOT read.
    const BURST: u64 = 900;
    for i in 0..BURST {
        append_and_publish(&app, sid, 1_700_000_100_000 + i).await;
    }
    let latest = 1 + BURST; // seq of the last appended event

    // Now drain: every delta must be contiguous and duplicate-free up to latest.
    let mut expected = 2;
    while expected <= latest {
        let delta = next_json(&mut ws).await;
        assert_eq!(
            delta["seq"].as_u64().unwrap(),
            expected,
            "recovered stream is contiguous and duplicate-free"
        );
        expected += 1;
    }
}

#[tokio::test]
async fn a_since_beyond_the_sessions_latest_seq_is_refused_with_a_policy_close() {
    // A stale/foreign `since` cursor must
    // not silently strand the connection open-but-mute forever (the live
    // loop's seq>last_seq_sent filter would never pass again) — the server
    // rejects it with an explicit protocol Close instead of hanging.
    let app = test_app().await;
    let cookie = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;
    // Only session.started (seq=1) exists on this fresh session.
    let session_id = fresh_started_session(&app, &cookie, "Stale Since Net").await;
    let addr = app.spawn_server().await;

    // Authz passes (the owner connects fine) — the cursor is rejected AFTER
    // the upgrade, inside the connect flow, not at authz.
    let mut ws = connect_ws(addr, &session_id, Some(999), Some(&cookie))
        .await
        .expect("owner upgrades; the stale cursor is rejected post-upgrade");

    let frame = expect_policy_close(&mut ws, 2_000).await;
    assert_eq!(
        u16::from(frame.code),
        1008,
        "policy-violation close code for an invalid resume cursor"
    );
}

#[tokio::test]
async fn a_since_equal_to_the_latest_seq_is_accepted_and_stream_continues_live() {
    // The boundary case adjacent to the rejection above: `since == latest_seq`
    // is a legitimately caught-up client (not stale), so it must be accepted
    // (no snapshot, no replay, live deltas continue normally).
    let app = test_app().await;
    let cookie = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;
    let session_id = fresh_started_session(&app, &cookie, "Caught Up Net").await;
    let sid = Uuid::parse_str(&session_id).unwrap();
    let addr = app.spawn_server().await;

    // Session's current latest_seq is 1 (session.started only).
    let mut ws = connect_ws(addr, &session_id, Some(1), Some(&cookie))
        .await
        .expect("owner upgrades and the caught-up cursor is accepted");

    let seq2 = append_and_publish(&app, sid, 1_700_000_100_000).await;
    assert_eq!(seq2, 2);
    let delta = next_json(&mut ws).await;
    assert_eq!(delta["type"], "event");
    assert_eq!(
        delta["seq"], 2,
        "live stream continues past an exact-match cursor"
    );
}

#[tokio::test]
async fn a_cross_origin_upgrade_is_refused_403() {
    // The session cookie rides the WS
    // handshake same as any other cookie-bearing request, so a browser page
    // on a different origin must not be able to ride the victim's cookie to
    // open this stream (cross-site WebSocket hijacking defense-in-depth,
    // alongside the existing SameSite=Lax cookie attribute).
    let app = test_app().await;
    let cookie = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;
    let session_id = fresh_started_session(&app, &cookie, "Origin Net").await;
    let addr = app.spawn_server().await;

    let err = connect_ws_with_origin(addr, &session_id, &cookie, "https://evil.example")
        .await
        .expect_err("a mismatched Origin is refused before any socket opens");
    match err {
        WsError::Http(response) => assert_eq!(response.status(), StatusCode::FORBIDDEN),
        other => panic!("expected an HTTP 403 upgrade refusal, got {other:?}"),
    }
}

#[tokio::test]
async fn a_co_owner_removed_mid_connection_is_evicted_on_the_next_recheck() {
    // The owner-only authz is decided
    // once at upgrade, but `DELETE .../owners/{account_id}` can revoke
    // a co-owner's authority while their WS connection stays open. The
    // keepalive-tick arm re-checks ownership on the same cadence as the
    // liveness Ping; a fast interval here proves the eviction happens
    // without a real 30-second wait.
    let mut app = test_app().await;
    app.state = app
        .state
        .clone()
        .with_ws_keepalive_interval_for_tests(Duration::from_millis(100));

    let owner_cookie = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;
    let coowner_cookie = sign_in_consent_callsign(&app, "coowner@example.com", "n1ale").await;
    let (_, coowner_me) = send_json(
        app.router(),
        "GET",
        "/api/accounts/me",
        None,
        Some(&coowner_cookie),
    )
    .await;
    let coowner_id = coowner_me["id"].as_str().expect("account id").to_owned();

    let definition_id = create_net(&app, &owner_cookie, "Reauth Net").await;
    let (status, _) = send_json(
        app.router(),
        "POST",
        &format!("/api/net-definitions/{definition_id}/owners"),
        Some(json!({ "callsign": "n1ale" })),
        Some(&owner_cookie),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "co-owner grant succeeds");

    let session_id = start_session(&app, &owner_cookie, &definition_id).await;
    let addr = app.spawn_server().await;

    // The co-owner connects while still an owner — upgrade succeeds.
    let mut ws = connect_ws(addr, &session_id, None, Some(&coowner_cookie))
        .await
        .expect("co-owner upgrades while still an owner");
    assert_eq!(next_json(&mut ws).await["type"], "snapshot");

    // The owner revokes the co-owner's authority over the definition.
    let (status, _) = send_json(
        app.router(),
        "DELETE",
        &format!("/api/net-definitions/{definition_id}/owners/{coowner_id}"),
        None,
        Some(&owner_cookie),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT, "co-owner removal succeeds");

    // The already-open connection is evicted on the next re-authorization
    // tick — it does not stay silently connected forever.
    let frame = expect_policy_close(&mut ws, 3_000).await;
    assert_eq!(
        u16::from(frame.code),
        1008,
        "policy-violation close code for a revoked owner"
    );
}

#[tokio::test]
async fn a_disabled_accounts_open_connection_is_evicted_on_the_next_recheck() {
    // `require_session`'s disabled-account
    // gate and the `disable` repo method's bulk session revocation only take
    // effect at HTTP-request time — a WS connection opened before an admin
    // disables the account captured its `CurrentAccount` before the disable and
    // would otherwise survive it indefinitely. The keepalive-tick arm re-checks
    // `disabled_at` on the same cadence as the ownership re-check (a fast
    // interval here proves the eviction happens without a real 30-second wait).
    let mut app = test_app().await;
    app.state = app
        .state
        .clone()
        .with_ws_keepalive_interval_for_tests(Duration::from_millis(100))
        .with_admin_allowlist(vec!["admin@example.com".to_owned()]);

    let owner_cookie = sign_in_consent_callsign(&app, "owner-ws@example.com", "w2aw").await;
    let owner_id = account_id(&app, &owner_cookie).await;
    let admin_cookie = sign_in_consent_callsign(&app, "admin@example.com", "n2ale").await;

    let definition_id = create_net(&app, &owner_cookie, "Disable Reauth Net").await;
    let session_id = start_session(&app, &owner_cookie, &definition_id).await;
    let addr = app.spawn_server().await;

    // The owner connects while still enabled — upgrade succeeds.
    let mut ws = connect_ws(addr, &session_id, None, Some(&owner_cookie))
        .await
        .expect("owner upgrades while still enabled");
    assert_eq!(next_json(&mut ws).await["type"], "snapshot");

    // An admin disables the connected owner's account (bounded
    // admin surface — the real HTTP path, not a direct repo call).
    let (status, _) = send_json(
        app.router(),
        "POST",
        &format!("/api/admin/accounts/{owner_id}/disable"),
        None,
        Some(&admin_cookie),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT, "disable succeeds");

    // The already-open connection is evicted on the next re-authorization
    // tick — a disabled account does not keep a live, authenticated stream
    // open forever.
    let frame = expect_policy_close(&mut ws, 3_000).await;
    assert_eq!(
        u16::from(frame.code),
        1008,
        "policy-violation close code for a disabled account"
    );
}

// ---------------------------------------------------------------------------
// HTTP catch-up endpoint, SLO proof, and extended-disconnect
// state-parity. Reuses the harness above (real Postgres + real bound server).
// ---------------------------------------------------------------------------

/// GETs the HTTP catch-up endpoint, returning `(status, body)`. An absent
/// `since` requests the whole log.
async fn get_events(
    app: &TestApp,
    session_id: &str,
    since: Option<u64>,
    cookie: Option<&str>,
) -> (StatusCode, Value) {
    let uri = match since {
        Some(n) => format!("/api/net-sessions/{session_id}/events?since={n}"),
        None => format!("/api/net-sessions/{session_id}/events"),
    };
    send_json(app.router(), "GET", &uri, None, cookie).await
}

/// Appends a `checkin.added` straight through the durable log and publishes it
/// (the POST-COMMIT writer hand-off), returning the minted seq. A stand-in for
/// a future check-in writer, used to script a varied event log.
async fn append_checkin(app: &TestApp, session_id: Uuid, at: u64, call: &str) -> u64 {
    let body = SessionEventBody::CheckinAdded {
        check_in_id: Uuid::now_v7(),
        callsign: parse_callsign(call).expect("valid callsign"),
        client_event_id: None,
        signal_report: None,
        staying: netroll_domain::check_in::StayingStatus::InAndOut,
        name: None,
        location: None,
        grid: None,
        source: netroll_domain::check_in::CheckInSource::Staff,
        via: None,
        relayed_by: None,
    };
    let event = app
        .state
        .session_events
        .append(session_id, &body, None, at)
        .await
        .expect("append checkin");
    app.state.hub.publish(session_id, &event);
    event.seq
}

/// Reconstructs a domain [`SessionEvent`] from one HTTP catch-up wire element —
/// the client-side reducer stand-in (the frontend ships the TS equivalent). Folding
/// these reproduces the authoritative state.
fn wire_to_event(v: &Value) -> SessionEvent {
    let seq = v["seq"].as_u64().expect("seq is a number");
    let actor_id = v
        .get("actorId")
        .and_then(|a| a.as_str())
        .map(|s| Uuid::parse_str(s).expect("actorId uuid"));
    let at = chrono::DateTime::parse_from_rfc3339(v["at"].as_str().expect("at string"))
        .expect("at is rfc3339")
        .timestamp_millis() as u64;
    let p = &v["payload"];
    let body = match v["kind"].as_str().expect("kind string") {
        "session.started" => SessionEventBody::SessionStarted {
            definition_id: Uuid::parse_str(p["definitionId"].as_str().expect("definitionId"))
                .expect("uuid"),
            definition_version: p["definitionVersion"].as_i64().expect("version") as i32,
        },
        "frequency.changed" => SessionEventBody::FrequencyChanged {
            connection_id: Uuid::parse_str(p["connectionId"].as_str().expect("connectionId"))
                .expect("uuid"),
            operating_frequency_hz: p["operatingFrequencyHz"].as_i64().expect("hz"),
        },
        "checkin.added" => SessionEventBody::CheckinAdded {
            check_in_id: Uuid::parse_str(p["checkInId"].as_str().expect("checkInId"))
                .expect("uuid"),
            callsign: parse_callsign(p["callsign"].as_str().expect("callsign"))
                .expect("valid callsign"),
            client_event_id: p
                .get("clientEventId")
                .and_then(|c| c.as_str())
                .map(|s| Uuid::parse_str(s).expect("clientEventId uuid")),
            signal_report: p.get("signalReport").and_then(|r| r.as_str()).map(|s| {
                netroll_domain::check_in::parse_signal_report(s)
                    .expect("valid stored report")
                    .expect("non-blank stored report")
            }),
            staying: p
                .get("staying")
                .and_then(|s| s.as_str())
                .map(|s| {
                    netroll_domain::check_in::StayingStatus::try_from(s)
                        .expect("known staying token")
                })
                .unwrap_or_default(),
            name: p.get("name").and_then(|n| n.as_str()).map(|s| {
                netroll_domain::check_in::parse_name(s)
                    .expect("valid stored name")
                    .expect("non-blank stored name")
            }),
            location: p.get("location").and_then(|l| l.as_str()).map(|s| {
                netroll_domain::check_in::parse_location(s)
                    .expect("valid stored location")
                    .expect("non-blank stored location")
            }),
            grid: p
                .get("grid")
                .and_then(|g| g.as_str())
                .map(|s| netroll_domain::profile::parse_grid(s).expect("valid stored grid")),
            source: p
                .get("source")
                .and_then(|s| s.as_str())
                .map(|s| {
                    netroll_domain::check_in::CheckInSource::try_from(s)
                        .expect("known source token")
                })
                .unwrap_or_default(),
            via: None,
            relayed_by: None,
        },
        "session.closed" => SessionEventBody::SessionClosed,
        other => panic!("unknown event kind {other}"),
    };
    SessionEvent {
        seq,
        actor_id,
        at,
        body,
    }
}

/// The server's authoritative folded state — `replay(events_since(id,0), 0)`.
async fn server_fold(app: &TestApp, session_id: Uuid) -> SessionState {
    let events = app
        .state
        .session_events
        .events_since(session_id, 0)
        .await
        .expect("load full log");
    replay(&events, 0)
}

#[tokio::test]
async fn http_catchup_returns_only_events_after_since_in_ascending_order() {
    // An owner holding state to seq=N gets exactly the events with seq>N,
    // ascending, each element byte-shape-identical to the WS `event` frame body.
    let app = test_app().await;
    let cookie = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;
    let session_id = fresh_started_session(&app, &cookie, "Catchup Net").await;
    let sid = Uuid::parse_str(&session_id).unwrap();

    // seq 2,3,4 after the seq-1 session.started.
    assert_eq!(
        append_checkin(&app, sid, 1_700_000_100_000, "n1ale").await,
        2
    );
    assert_eq!(append_and_publish(&app, sid, 1_700_000_200_000).await, 3);
    assert_eq!(
        append_checkin(&app, sid, 1_700_000_300_000, "k2xyz").await,
        4
    );

    let (status, body) = get_events(&app, &session_id, Some(2), Some(&cookie)).await;
    assert_eq!(status, StatusCode::OK);
    let arr = body.as_array().expect("catch-up returns a JSON array");
    let seqs: Vec<u64> = arr.iter().map(|e| e["seq"].as_u64().unwrap()).collect();
    assert_eq!(seqs, vec![3, 4], "only seq>since, in ascending order");

    // Element shape: bare event body, no `type` discriminator.
    let first = &arr[0];
    assert_eq!(first["kind"], "frequency.changed");
    assert!(first["at"].is_string());
    assert!(
        first.as_object().unwrap().get("type").is_none(),
        "the HTTP element carries no type discriminator"
    );
    assert_eq!(first["payload"]["operatingFrequencyHz"], 7_200_000);
}

#[tokio::test]
async fn http_catchup_absent_or_zero_since_returns_the_whole_log() {
    // `?since=0` or absent `since` returns every event from seq 1.
    let app = test_app().await;
    let cookie = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;
    let session_id = fresh_started_session(&app, &cookie, "Whole Log Net").await;
    let sid = Uuid::parse_str(&session_id).unwrap();
    append_and_publish(&app, sid, 1_700_000_100_000).await;

    let (status, body) = get_events(&app, &session_id, None, Some(&cookie)).await;
    assert_eq!(status, StatusCode::OK);
    let seqs: Vec<u64> = body
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["seq"].as_u64().unwrap())
        .collect();
    assert_eq!(seqs, vec![1, 2], "absent since = whole log from seq 1");

    let (status, body_zero) = get_events(&app, &session_id, Some(0), Some(&cookie)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body_zero, body, "since=0 is identical to absent since");
}

#[tokio::test]
async fn http_catchup_unknown_session_is_404_not_empty_200() {
    // Existence is confirmed via the authz find; `events_since`'s
    // empty-on-unknown silence is never trusted.
    let app = test_app().await;
    let cookie = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;
    // A real owner exists, but this session id was never created.
    let missing = Uuid::now_v7().to_string();

    let (status, _) = get_events(&app, &missing, Some(0), Some(&cookie)).await;
    assert_eq!(
        status,
        StatusCode::NOT_FOUND,
        "an unknown session is a 404, not an empty 200 array"
    );
}

#[tokio::test]
async fn http_catchup_non_owner_is_403_and_no_cookie_is_401() {
    // Owner-only, object-level; missing cookie is 401.
    let app = test_app().await;
    let owner = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;
    let session_id = fresh_started_session(&app, &owner, "Authz Catchup Net").await;
    let stranger = sign_in_consent_callsign(&app, "stranger@example.com", "n1ale").await;

    let (status, _) = get_events(&app, &session_id, Some(0), Some(&stranger)).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "a non-owner is refused 403");

    let (status, _) = get_events(&app, &session_id, Some(0), None).await;
    assert_eq!(
        status,
        StatusCode::UNAUTHORIZED,
        "an unauthenticated request is 401"
    );
}

#[tokio::test]
async fn http_catchup_since_at_or_beyond_latest_is_an_empty_200() {
    // A fully-caught-up (since==latest) or stale/foreign (since>latest)
    // cursor gets an empty 200 array — an HTTP GET has no stream to strand.
    let app = test_app().await;
    let cookie = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;
    let session_id = fresh_started_session(&app, &cookie, "Caught Up Catchup Net").await;
    let sid = Uuid::parse_str(&session_id).unwrap();
    assert_eq!(append_and_publish(&app, sid, 1_700_000_100_000).await, 2);
    // latest_seq is now 2.

    let (status, body) = get_events(&app, &session_id, Some(2), Some(&cookie)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        body.as_array().unwrap().len(),
        0,
        "since==latest returns an empty 200 array"
    );

    let (status, body) = get_events(&app, &session_id, Some(999), Some(&cookie)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        body.as_array().unwrap().len(),
        0,
        "since>latest returns an empty 200 array, not an error"
    );
}

#[tokio::test]
async fn http_catchup_since_i64_max_boundary_is_pinned() {
    // The `since > i64::MAX` coercion
    // is pinned at the EXACT boundary. `since == i64::MAX` must still reach
    // the adapter (no real seq is ever that high, so it legitimately answers
    // empty via `events_since`'s own bounds check); `since == i64::MAX + 1`
    // and `since == u64::MAX` must short-circuit in the handler before ever
    // reaching the adapter. All three are empty 200s, never an error — a
    // future off-by-one (`>=` vs `>`) at either boundary would be caught here.
    let app = test_app().await;
    let cookie = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;
    let session_id = fresh_started_session(&app, &cookie, "i64 Boundary Net").await;

    let (status, body) = get_events(&app, &session_id, Some(i64::MAX as u64), Some(&cookie)).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "since==i64::MAX reaches the adapter and is empty, not an error"
    );
    assert_eq!(body.as_array().unwrap().len(), 0);

    let (status, body) =
        get_events(&app, &session_id, Some(i64::MAX as u64 + 1), Some(&cookie)).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "since==i64::MAX+1 is coerced to empty in-handler"
    );
    assert_eq!(body.as_array().unwrap().len(), 0);

    let (status, body) = get_events(&app, &session_id, Some(u64::MAX), Some(&cookie)).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "since==u64::MAX is coerced to empty in-handler"
    );
    assert_eq!(body.as_array().unwrap().len(), 0);
}

#[tokio::test]
async fn http_catchup_malformed_since_is_rejected_not_silently_accepted() {
    // A non-numeric `since` never reaches
    // the handler's bounds logic — the query extractor refuses it pre-handler,
    // and a malformed cursor must never be silently treated as valid.
    //
    // The contract is problem+json, not a plain-text 400: an assertion that
    // covers only `status()` would let an `AppQuery` swap flip the media type
    // in silence, so the pin asserts the media type and the slug so it can
    // actually fire.
    let app = test_app().await;
    let cookie = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;
    let session_id = fresh_started_session(&app, &cookie, "Malformed Since Net").await;

    let (status, content_type, raw) = get_untyped(
        app.router(),
        &format!("/api/net-sessions/{session_id}/events?since=abc"),
        Some(&cookie),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "a non-numeric since is rejected, never silently treated as a valid cursor"
    );
    assert_eq!(
        content_type, "application/problem+json",
        "the catch-up query rejection answers in the problem+json contract"
    );
    let problem: Value = serde_json::from_str(&raw).expect("problem body is JSON");
    assert_eq!(problem["type"], "/errors/validation");
}

/// GET returning status, `content-type` and the raw body — for asserting an
/// error CONTRACT (media type + slug) rather than a decoded success payload.
async fn get_untyped(
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

#[tokio::test]
async fn a_malformed_ws_resume_cursor_answers_in_problem_json_on_both_upgrade_routes() {
    // The two WS UPGRADE handlers read `?since=` off the query string, so the
    // query-string contract governs them. `Query<WsQuery>` sits BEFORE
    // `WebSocketUpgrade` in both signatures, so a non-numeric `since` is
    // refused pre-handshake — no upgrade headers, and on the PUBLIC route no
    // cookie either, which makes this rejection anonymously observable on a
    // public path. Asserts the media type and the slug, never axum's prose.
    let app = test_app().await;
    let cookie = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;
    let session_id = fresh_started_session(&app, &cookie, "Malformed WS Cursor Net").await;

    // The PUBLIC upgrade — account-less, outside `require_session`.
    let (status, content_type, raw) = get_untyped(
        app.router(),
        &format!("/api/net-sessions/{session_id}/live/ws?since=abc"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(
        content_type, "application/problem+json",
        "the public WS query rejection answers in the problem+json contract"
    );
    let problem: Value = serde_json::from_str(&raw).expect("problem body is JSON");
    assert_eq!(problem["type"], "/errors/validation");
    assert_eq!(problem["status"], 400);

    // The OWNER upgrade — `ConsentedAccount` is extracted before the query, so
    // this needs a real cookie (Trap 5); asserting 400 rather than 401 is
    // itself proof the session gate cleared before extraction.
    let (status, content_type, raw) = get_untyped(
        app.router(),
        &format!("/api/net-sessions/{session_id}/ws?since=abc"),
        Some(&cookie),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(
        content_type, "application/problem+json",
        "the owner WS query rejection answers in the problem+json contract"
    );
    let problem: Value = serde_json::from_str(&raw).expect("problem body is JSON");
    assert_eq!(problem["type"], "/errors/validation");
    assert_eq!(problem["status"], 400);
}

#[tokio::test]
async fn event_committed_between_http_catchup_and_ws_handshake_is_not_lost() {
    // The reconnect choreography is
    // (a) HTTP GET catch-up to M, (b) open WS `?since=M`. This proves the
    // seam BETWEEN those two calls is safe: an event committed strictly after
    // the HTTP response returns and BEFORE the WS handshake completes is
    // still delivered exactly once — the WS resume branch re-reads
    // `events_since` fresh on every connect rather than depending on residual
    // broadcast state from a subscription that did not exist yet, so nothing
    // committed in this window is lost, and nothing is duplicated either.
    let app = test_app().await;
    let cookie = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;
    let session_id = fresh_started_session(&app, &cookie, "Handshake Race Net").await;
    let sid = Uuid::parse_str(&session_id).unwrap();

    let (status, body) = get_events(&app, &session_id, Some(0), Some(&cookie)).await;
    assert_eq!(status, StatusCode::OK);
    let m = body.as_array().unwrap().last().unwrap()["seq"]
        .as_u64()
        .unwrap();
    assert_eq!(m, 1, "only session.started so far");

    // Committed strictly between the HTTP response above and the WS handshake
    // below — before any socket for this session exists.
    let raced_seq = append_and_publish(&app, sid, 1_700_000_500_000).await;
    assert_eq!(raced_seq, m + 1);

    let addr = app.spawn_server().await;
    let mut ws = connect_ws(addr, &session_id, Some(m), Some(&cookie))
        .await
        .expect("resume from the pre-race cursor M");
    let delivered = next_json(&mut ws).await;
    assert_eq!(delivered["type"], "event");
    assert_eq!(
        delivered["seq"], raced_seq,
        "the event committed in the HTTP-to-WS handshake gap is delivered via the \
         resume replay, not lost"
    );
    assert!(
        try_next_json(&mut ws, 300).await.is_none(),
        "delivered exactly once — no duplicate from residual broadcast state"
    );
}

#[tokio::test]
async fn commit_to_ws_send_p95_latency_is_within_the_realtime_slo() {
    // The measurement point is fixed at server-commit (the instant
    // `hub.publish` runs, post-commit) → WS-send (the instant the task writes
    // the `event` frame), EXCLUDING device render. Under representative fan-out
    // (a few hundred concurrent receivers on one session, standing in for
    // ~15 sessions × 10–50 participants) the observed p95 must be
    // comfortably within ~2s. A test-only sink observes the exact server-side
    // commit→send samples the production tracing measurement point emits.
    let mut app = test_app().await;
    let sink = std::sync::Arc::new(std::sync::Mutex::new(Vec::<Duration>::new()));
    app.state = app
        .state
        .clone()
        .with_ws_latency_sink_for_tests(sink.clone());

    let cookie = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;
    let session_id = fresh_started_session(&app, &cookie, "SLO Net").await;
    let sid = Uuid::parse_str(&session_id).unwrap();
    let addr = app.spawn_server().await;

    const RECEIVERS: usize = 200;
    let mut clients = Vec::with_capacity(RECEIVERS);
    for _ in 0..RECEIVERS {
        let mut ws = connect_ws(addr, &session_id, None, Some(&cookie))
            .await
            .expect("receiver upgrades");
        assert_eq!(next_json(&mut ws).await["type"], "snapshot");
        clients.push(ws);
    }

    // One committed event fans out to every receiver; each WS task records a
    // commit→send sample at the fixed measurement point.
    let seq = append_and_publish(&app, sid, 1_700_000_100_000).await;
    assert_eq!(seq, 2);
    for ws in &mut clients {
        assert_eq!(
            next_json(ws).await["seq"],
            2,
            "every receiver gets the delta"
        );
    }

    // Let the post-send sample pushes settle (the push follows the send await).
    for _ in 0..100 {
        if sink.lock().unwrap().len() >= RECEIVERS {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    let mut samples: Vec<u128> = sink.lock().unwrap().iter().map(|d| d.as_millis()).collect();
    assert_eq!(
        samples.len(),
        RECEIVERS,
        "one commit→send sample recorded per receiver"
    );
    samples.sort_unstable();
    let p95 = samples[((samples.len() as f64) * 0.95) as usize];
    assert!(
        p95 < 2_000,
        "commit→WS-send p95 = {p95}ms must be comfortably within the ~2s real-time SLO"
    );
}

#[tokio::test]
async fn extended_disconnect_http_catchup_then_ws_resume_is_state_identical() {
    // The correctness core: a client folds a baseline to seq=N,
    // disconnects, a large gap (M ≫ N) accrues, it catches the gap over the
    // HTTP endpoint, folds it with the REAL domain fold, reconnects the WS with
    // ?since=M, takes one more live delta, and ends byte-equal to the server's
    // authoritative fold — no gap, no duplicate across the HTTP→WS boundary.
    let app = test_app().await;
    let cookie = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;
    let session_id = fresh_started_session(&app, &cookie, "Parity Net").await;
    let sid = Uuid::parse_str(&session_id).unwrap();

    // Pre-disconnect activity → N = 3.
    assert_eq!(
        append_checkin(&app, sid, 1_700_000_002_000, "n1ale").await,
        2
    );
    assert_eq!(append_and_publish(&app, sid, 1_700_000_003_000).await, 3);
    let n = 3u64;

    // Initial sync: connect fresh, confirm the snapshot cursor is N, then the
    // client's baseline folded state (what the snapshot projects) is the
    // server's authoritative fold at N. Disconnect (drop the socket).
    let addr = app.spawn_server().await;
    {
        let mut ws = connect_ws(addr, &session_id, None, Some(&cookie))
            .await
            .expect("owner upgrades for initial sync");
        assert_eq!(next_json(&mut ws).await["session"]["latestSeq"], n);
    }
    let mut client_state = server_fold(&app, sid).await;
    assert_eq!(client_state.last_seq, n);

    // Extended disconnect: a long gap of mixed events accrues (M = 48 ≫ N = 3).
    let mut at = 1_700_000_004_000u64;
    for i in 0..44 {
        append_checkin(&app, sid, at, &format!("w{i}abc")).await;
        at += 1_000;
    }
    let m = append_and_publish(&app, sid, at).await;
    assert_eq!(m, 48, "the gap grew the log to seq 48");

    // HTTP catch-up: fetch exactly the missed gap (seq > N), fold it with the
    // real domain fold onto the baseline.
    let (status, body) = get_events(&app, &session_id, Some(n), Some(&cookie)).await;
    assert_eq!(status, StatusCode::OK);
    let gap = body.as_array().expect("array");
    let gap_seqs: Vec<u64> = gap.iter().map(|e| e["seq"].as_u64().unwrap()).collect();
    let expected: Vec<u64> = (n + 1..=m).collect();
    assert_eq!(gap_seqs, expected, "catch-up is exactly the gap, in order");
    for e in gap {
        client_state = netroll_domain::fold::fold(client_state, &wire_to_event(e));
    }
    assert_eq!(client_state.last_seq, m, "client folded up to M via HTTP");

    // Reconnect the WS from the now-current cursor M; the first live delta must
    // be exactly M+1 (gap-free, duplicate-free across the HTTP→WS boundary).
    let mut ws = connect_ws(addr, &session_id, Some(m), Some(&cookie))
        .await
        .expect("owner resumes the WS from M");
    let live_seq = append_and_publish(&app, sid, at + 1_000).await;
    assert_eq!(live_seq, m + 1);
    let delta = next_json(&mut ws).await;
    assert_eq!(delta["type"], "event");
    assert_eq!(
        delta["seq"], live_seq,
        "the first live delta after catch-up is exactly M+1"
    );
    client_state = netroll_domain::fold::fold(client_state, &wire_to_event(&delta));

    // The client's fully reconstructed state equals the server's authoritative
    // fold of the whole log — byte-equal, no gap, no duplicate.
    let server_state = server_fold(&app, sid).await;
    assert_eq!(
        client_state, server_state,
        "client-folded state is identical to the server's authoritative fold at M+1"
    );
}

#[tokio::test]
async fn reconnect_to_a_session_closed_during_the_gap_folds_to_closed() {
    // A session that transitions to `closed` while the client is
    // disconnected — the catch-up replay includes the `session.closed` event in
    // order, so the client folds to the authoritative `closed` state, and a WS
    // reconnect at the caught-up cursor yields no further (duplicate) delta.
    let app = test_app().await;
    let cookie = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;
    let session_id = fresh_started_session(&app, &cookie, "Closed Gap Net").await;
    let sid = Uuid::parse_str(&session_id).unwrap();

    // Baseline at N = 1 (session.started only).
    let mut client_state = server_fold(&app, sid).await;
    assert_eq!(client_state.last_seq, 1);
    assert_eq!(
        client_state.lifecycle,
        netroll_domain::fold::SessionLifecycle::Live
    );

    // Gap: two check-ins, then a real HTTP close appends session.closed (seq 4).
    append_checkin(&app, sid, 1_700_000_002_000, "n1ale").await;
    append_checkin(&app, sid, 1_700_000_003_000, "k2xyz").await;
    let (status, _) = send_json(
        app.router(),
        "POST",
        &format!("/api/net-sessions/{session_id}/close"),
        None,
        Some(&cookie),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    // Catch up the gap; the closed event is present and folds to Closed.
    let (status, body) = get_events(&app, &session_id, Some(1), Some(&cookie)).await;
    assert_eq!(status, StatusCode::OK);
    let gap = body.as_array().unwrap();
    assert_eq!(
        gap.iter()
            .map(|e| e["seq"].as_u64().unwrap())
            .collect::<Vec<_>>(),
        vec![2, 3, 4]
    );
    assert_eq!(gap.last().unwrap()["kind"], "session.closed");
    for e in gap {
        client_state = netroll_domain::fold::fold(client_state, &wire_to_event(e));
    }
    assert_eq!(
        client_state.lifecycle,
        netroll_domain::fold::SessionLifecycle::Closed,
        "the client folds to the authoritative closed state"
    );
    assert_eq!(client_state, server_fold(&app, sid).await);

    // A WS reconnect at the caught-up cursor (M=4) serves the closed log
    // read-only and yields no duplicate/extra delta.
    let addr = app.spawn_server().await;
    let mut ws = connect_ws(addr, &session_id, Some(4), Some(&cookie))
        .await
        .expect("owner resumes a closed session read-only");
    assert!(
        try_next_json(&mut ws, 400).await.is_none(),
        "a caught-up reconnect to a closed session yields no further delta"
    );
}

#[tokio::test]
async fn reconnect_after_ownership_revoked_mid_disconnect_gets_403() {
    // An account removed from the definition's owner set while
    // disconnected gets 403 from the stateless HTTP catch-up — the request-time
    // `load_owned_session` check enforces CURRENT ownership, consistent with
    // the live WS's periodic re-auth eviction.
    let app = test_app().await;
    let owner_cookie = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;
    let coowner_cookie = sign_in_consent_callsign(&app, "coowner@example.com", "n1ale").await;
    let (_, coowner_me) = send_json(
        app.router(),
        "GET",
        "/api/accounts/me",
        None,
        Some(&coowner_cookie),
    )
    .await;
    let coowner_id = coowner_me["id"].as_str().expect("account id").to_owned();

    let definition_id = create_net(&app, &owner_cookie, "Revoke Catchup Net").await;
    let (status, _) = send_json(
        app.router(),
        "POST",
        &format!("/api/net-definitions/{definition_id}/owners"),
        Some(json!({ "callsign": "n1ale" })),
        Some(&owner_cookie),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "co-owner grant succeeds");
    let session_id = start_session(&app, &owner_cookie, &definition_id).await;

    // While still an owner, the co-owner can catch up.
    let (status, _) = get_events(&app, &session_id, Some(0), Some(&coowner_cookie)).await;
    assert_eq!(status, StatusCode::OK, "a current owner catches up fine");

    // The owner revokes the co-owner's authority over the definition.
    let (status, _) = send_json(
        app.router(),
        "DELETE",
        &format!("/api/net-definitions/{definition_id}/owners/{coowner_id}"),
        None,
        Some(&owner_cookie),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT, "co-owner removal succeeds");

    // Now the same catch-up call is refused 403 at request time.
    let (status, _) = get_events(&app, &session_id, Some(0), Some(&coowner_cookie)).await;
    assert_eq!(
        status,
        StatusCode::FORBIDDEN,
        "a revoked owner's stateless catch-up is refused 403 at request time"
    );
}

// ---------------------------------------------------------------------------
// The PUBLIC (account-less) redacted read surface: GET …/live
// (snapshot), GET …/live/events (catch-up), GET …/live/ws (WebSocket), all
// IP-rate-governed and OUTSIDE require_session. Redaction is mandatory:
// no operator account id, no internal net id on the wire (the byte-assertion
// posture). Reuses the real-server + real-Postgres harness above.
// ---------------------------------------------------------------------------

/// GET returning the RAW response body string (for substring-absence assertions
/// on the exact bytes on the wire — the `api_discovery.rs` pattern).
async fn get_raw(router: Router, uri: &str, cookie: Option<&str>) -> (StatusCode, String) {
    let mut builder = Request::builder().method("GET").uri(uri);
    if let Some(cookie) = cookie {
        builder = builder.header(header::COOKIE, cookie);
    }
    let request = builder.body(Body::empty()).expect("build request");
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

/// Adds a check-in over the owner HTTP endpoint, returning nothing — used to
/// seed a redacted roster row whose `addedBy` is a KNOWN operator account id.
async fn owner_add_check_in(app: &TestApp, session_id: &str, cookie: &str, callsign: &str) {
    let (status, _) = send_json(
        app.router(),
        "POST",
        &format!("/api/net-sessions/{session_id}/check-ins"),
        Some(json!({ "callsign": callsign })),
        Some(cookie),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "owner check-in succeeds");
}

/// The owner's account id (for redaction absence assertions).
async fn account_id(app: &TestApp, cookie: &str) -> String {
    let (_, me) = send_json(app.router(), "GET", "/api/accounts/me", None, Some(cookie)).await;
    me["id"].as_str().expect("account id").to_owned()
}

/// Connects the PUBLIC WebSocket (`/live/ws`) — no cookie (account-less).
async fn connect_public_ws(
    addr: SocketAddr,
    session_id: &str,
    since: Option<u64>,
) -> Result<WsStream, WsError> {
    let query = match since {
        Some(n) => format!("?since={n}"),
        None => String::new(),
    };
    let url = format!("ws://{addr}/api/net-sessions/{session_id}/live/ws{query}");
    let request = url.into_client_request().expect("client request");
    let (stream, _response) = tokio_tungstenite::connect_async(request).await?;
    Ok(stream)
}

#[tokio::test]
async fn public_live_snapshot_is_redacted_and_needs_no_cookie() {
    // The public /live read is account-less (no cookie) and carries
    // the callsign but NEITHER the operator account id NOR the internal net ids;
    // the RAW bytes contain neither the seeded owner account id nor the
    // definition id (the load-bearing no-leak property).
    let app = test_app().await;
    let cookie = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;
    let definition_id = create_net(&app, &cookie, "Public Net").await;
    let session_id = start_session(&app, &cookie, &definition_id).await;
    let owner_id = account_id(&app, &cookie).await;
    owner_add_check_in(&app, &session_id, &cookie, "N1CCK").await;

    // No cookie at all — the account-less read resolves.
    let (status, raw) = get_raw(
        app.router(),
        &format!("/api/net-sessions/{session_id}/live"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "the public read needs no cookie");

    let body: Value = serde_json::from_str(&raw).expect("json");
    assert_eq!(body["lifecycle"], "live");
    assert_eq!(body["roster"][0]["callsign"], "N1CCK");
    // Redaction: no operator/internal id fields at all.
    assert!(
        body["roster"][0].get("addedBy").is_none(),
        "a public roster entry has no addedBy"
    );
    assert!(
        body.get("definitionId").is_none() && body.get("definitionVersion").is_none(),
        "the public view omits the internal net ids"
    );
    // The load-bearing raw-bytes assertion.
    assert!(
        !raw.contains(&owner_id),
        "the public /live body must never leak the operator account id"
    );
    assert!(
        !raw.contains(&definition_id),
        "the public /live body must never leak the definition id"
    );
    // The net title (public net data) IS kept.
    assert_eq!(body["definition"]["title"], "Public Net");
}

#[tokio::test]
async fn public_live_missing_session_is_404() {
    let app = test_app().await;
    let missing = Uuid::now_v7().to_string();
    let (status, _) = get_raw(
        app.router(),
        &format!("/api/net-sessions/{missing}/live"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn public_live_events_are_redacted_no_actor_or_internal_ids() {
    // The public catch-up returns redacted WireEvents — the
    // checkin.added frame carries NO actorId, and the session.started frame
    // carries NO definitionId/definitionVersion in its payload. The raw bytes
    // contain neither the owner account id nor the definition id.
    let app = test_app().await;
    let cookie = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;
    let definition_id = create_net(&app, &cookie, "Public Events Net").await;
    let session_id = start_session(&app, &cookie, &definition_id).await;
    let owner_id = account_id(&app, &cookie).await;
    owner_add_check_in(&app, &session_id, &cookie, "N1CCK").await;

    let (status, raw) = get_raw(
        app.router(),
        &format!("/api/net-sessions/{session_id}/live/events?since=0"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let events: Value = serde_json::from_str(&raw).expect("json array");
    let arr = events.as_array().expect("array");
    // seq 1 = session.started (no definitionId/version), seq 2 = checkin.added.
    let started = &arr[0];
    assert_eq!(started["kind"], "session.started");
    assert!(
        started["payload"].get("definitionId").is_none()
            && started["payload"].get("definitionVersion").is_none(),
        "a public session.started frame omits the internal net ids"
    );
    let checkin = &arr[1];
    assert_eq!(checkin["kind"], "checkin.added");
    assert_eq!(checkin["payload"]["callsign"], "N1CCK");
    assert!(
        checkin.get("actorId").is_none(),
        "a public checkin.added frame carries no actorId"
    );
    assert!(
        !raw.contains(&owner_id),
        "the public events body must never leak the operator account id"
    );
    assert!(
        !raw.contains(&definition_id),
        "the public events body must never leak the definition id"
    );
}

#[tokio::test]
async fn public_ws_receives_redacted_snapshot_then_redacted_delta() {
    // An account-less public WS client receives a redacted snapshot,
    // then — after an owner adds a check-in over HTTP — a redacted checkin.added
    // delta carrying the callsign but NO actorId. Raw-bytes asserted on BOTH
    // frames (not just structural absence), mirroring the `/live` and
    // `/live/events` raw-bytes redaction rigor — the WS path shares the exact
    // same `public_view_from_row`/`WireEvent::from_event_public` serializers,
    // but this proves it at the wire level rather than assuming it.
    let app = test_app().await;
    let cookie = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;
    let definition_id = create_net(&app, &cookie, "Public WS Net").await;
    let session_id = start_session(&app, &cookie, &definition_id).await;
    let owner_id = account_id(&app, &cookie).await;
    let addr = app.spawn_server().await;

    // No cookie — the public stream is account-less.
    let mut ws = connect_public_ws(addr, &session_id, None)
        .await
        .expect("public client upgrades with no cookie");
    let snapshot_raw = next_raw_text(&mut ws, 5_000).await;
    let snapshot: Value = serde_json::from_str(&snapshot_raw).expect("snapshot is JSON");
    assert_eq!(snapshot["type"], "snapshot");
    assert_eq!(snapshot["session"]["lifecycle"], "live");
    // Redacted snapshot: no internal net ids.
    assert!(
        snapshot["session"].get("definitionId").is_none(),
        "the public WS snapshot omits the definition id"
    );
    assert!(
        !snapshot_raw.contains(&owner_id),
        "the public WS snapshot frame must never leak the operator account id"
    );
    assert!(
        !snapshot_raw.contains(&definition_id),
        "the public WS snapshot frame must never leak the definition id"
    );

    owner_add_check_in(&app, &session_id, &cookie, "N1CCK").await;

    let delta_raw = next_raw_text(&mut ws, 5_000).await;
    let delta: Value = serde_json::from_str(&delta_raw).expect("delta is JSON");
    assert_eq!(delta["type"], "event");
    assert_eq!(delta["seq"], 2);
    assert_eq!(delta["kind"], "checkin.added");
    assert_eq!(delta["payload"]["callsign"], "N1CCK");
    assert!(
        delta.get("actorId").is_none(),
        "a public WS delta carries no actorId"
    );
    assert!(
        delta["payload"].get("clientEventId").is_none(),
        "a public WS delta's payload carries no clientEventId"
    );
    assert!(
        !delta_raw.contains(&owner_id),
        "the public WS delta frame must never leak the operator account id"
    );
    assert!(
        !delta_raw.contains(&definition_id),
        "the public WS delta frame must never leak the definition id"
    );
}

#[tokio::test]
async fn public_ws_delta_omits_the_signal_report_but_carries_staying() {
    // `WireEvent::from_event_public`'s denylist must strip the staff-entered
    // `signalReport`, which once crossed the public WS delta unredacted.
    // `staying` deliberately DOES cross; both halves are asserted here so
    // neither can be relaxed without a red.
    let app = test_app().await;
    let cookie = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;
    let definition_id = create_net(&app, &cookie, "Public Report Net").await;
    let session_id = start_session(&app, &cookie, &definition_id).await;
    let addr = app.spawn_server().await;

    let mut ws = connect_public_ws(addr, &session_id, None)
        .await
        .expect("public client upgrades with no cookie");
    let _snapshot_raw = next_raw_text(&mut ws, 5_000).await;

    // The owner holds EditStaffFields (Owner ⊃ NCS ⊃ Logger) so this report
    // commits — proving the redaction holds even when the field is genuinely
    // populated, not merely absent.
    let (status, _) = send_json(
        app.router(),
        "POST",
        &format!("/api/net-sessions/{session_id}/check-ins"),
        Some(json!({
            "callsign": "N1CCK",
            "signalReport": "59",
            "staying": "staying-for-comments",
        })),
        Some(&cookie),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::CREATED,
        "owner report+staying check-in succeeds"
    );

    let delta_raw = next_raw_text(&mut ws, 5_000).await;
    let delta: Value = serde_json::from_str(&delta_raw).expect("delta is JSON");
    assert_eq!(delta["kind"], "checkin.added");
    // STILL TRUE: the report did not move.
    assert!(
        delta["payload"].get("signalReport").is_none(),
        "a public WS delta's payload carries no signalReport, even when the owner set one"
    );
    assert!(
        !delta_raw.contains("599"),
        "the raw public WS delta bytes never carry the report: {delta_raw}"
    );
    // NOW TRUE: staying crosses the public delta with the value the
    // operator set, so an observer's roster is not fed the reducer's default.
    assert_eq!(
        delta["payload"]["staying"], "staying-for-comments",
        "a public WS delta carries the staying the operator set"
    );

    // A MID-NET precedence change reaches the observer without a reload,
    // so the delta path is proven independently of the snapshot path.
    let check_in_id = delta["payload"]["checkInId"]
        .as_str()
        .expect("checkInId")
        .to_owned();
    let (status, _) = send_json(
        app.router(),
        "PUT",
        &format!("/api/net-sessions/{session_id}/check-ins/{check_in_id}"),
        Some(json!({
            "callsign": "N1CCK",
            "precedence": "emergency",
            "traffic": 2,
            "notes": "STAFF-ONLY commentary",
            "publicNote": "PUBLIC: emergency traffic for the county EOC",
            "expectedVersion": 1
        })),
        Some(&cookie),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let updated_raw = next_raw_text(&mut ws, 5_000).await;
    let updated: Value = serde_json::from_str(&updated_raw).expect("delta is JSON");
    assert_eq!(updated["kind"], "checkin.updated");
    assert_eq!(
        updated["payload"]["precedence"], "emergency",
        "the observer sees the precedence change live, not the `routine` default"
    );
    assert_eq!(updated["payload"]["traffic"], 2);
    assert_eq!(
        updated["payload"]["publicNote"],
        "PUBLIC: emergency traffic for the county EOC"
    );
    assert_eq!(updated["payload"]["staying"], "staying-for-comments");
    // STILL TRUE: the staff note and the report stay off the public delta.
    assert!(
        updated["payload"].get("notes").is_none(),
        "the STAFF note never crosses the public delta"
    );
    assert!(
        !updated_raw.contains("STAFF-ONLY commentary"),
        "the staff note never leaks in any shape: {updated_raw}"
    );
    assert!(
        updated["payload"].get("signalReport").is_none(),
        "the report never crosses the public delta"
    );
}

#[tokio::test]
async fn public_reads_are_ip_governed_but_bare_router_serves_them_ungoverned() {
    // Under `api_router_ip_limited` a flood of public /live reads
    // eventually 429s (the shipped read governor), while the bare
    // `api_router` (no ConnectInfo) serves them ungoverned — never 500ing on the
    // absent peer IP. Mirrors the discovery governor test.
    use axum::extract::ConnectInfo;
    use netroll_app::http::api_router_ip_limited;
    use netroll_app::http::rate_limit::READ_BURST;

    let app = test_app().await;
    let cookie = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;
    let definition_id = create_net(&app, &cookie, "Governed Net").await;
    let session_id = start_session(&app, &cookie, &definition_id).await;

    let governed = api_router_ip_limited(app.state.clone());
    let uri = format!("/api/net-sessions/{session_id}/live");
    let peer = SocketAddr::from(([127, 0, 0, 9], 41_000));
    let mut saw_429 = false;
    for _ in 0..(2 * READ_BURST + 5) {
        let mut request = Request::builder()
            .method("GET")
            .uri(&uri)
            .body(Body::empty())
            .expect("build request");
        request.extensions_mut().insert(ConnectInfo(peer));
        let response = governed.clone().oneshot(request).await.expect("route");
        if response.status() == StatusCode::TOO_MANY_REQUESTS {
            saw_429 = true;
        }
    }
    assert!(
        saw_429,
        "public /live reads from one IP must eventually 429"
    );

    // The bare router (no ConnectInfo) serves the same read ungoverned — 200,
    // never a 500 from the SmartIpKeyExtractor without a peer IP.
    let (status, _) = get_raw(app.router(), &uri, None).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "the bare api_router serves public reads ungoverned"
    );
}

#[tokio::test]
async fn an_owner_read_still_401s_without_a_cookie_no_surface_widened() {
    // The owner GET stays session-gated — no cookie is a 401 — while the
    // public /live read for the same session needs none. Proves the owner
    // surface was not widened.
    let app = test_app().await;
    let cookie = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;
    let definition_id = create_net(&app, &cookie, "Unwidened Net").await;
    let session_id = start_session(&app, &cookie, &definition_id).await;

    let (owner_status, _) = get_raw(
        app.router(),
        &format!("/api/net-sessions/{session_id}"),
        None,
    )
    .await;
    assert_eq!(
        owner_status,
        StatusCode::UNAUTHORIZED,
        "the owner GET still requires a session cookie"
    );

    let (public_status, _) = get_raw(
        app.router(),
        &format!("/api/net-sessions/{session_id}/live"),
        None,
    )
    .await;
    assert_eq!(
        public_status,
        StatusCode::OK,
        "the public /live read for the same session needs no cookie"
    );
}

#[tokio::test]
async fn an_unrecognised_query_parameter_is_ignored_on_both_ws_upgrade_routes() {
    // The LENIENT counter-direction on the two WS upgrades. Must pass BEFORE
    // and AFTER.
    //
    // TWO assertions, because the first alone does not prove the literal claim.
    //
    // 1. Non-upgrade GETs, compared: an unrecognised key must reach the SAME
    // answer as no query string at all — status, media type AND body — i.e.
    // the query extractor did not reject and the request fell through to
    // `WebSocketUpgrade`, which refuses a plain GET carrying no upgrade
    // headers. Both statuses are 400, so without the BODY comparison only the
    // media-type assertion could ever fail. Contrast
    // `a_malformed_ws_resume_cursor_answers_in_problem_json_on_both_upgrade_routes`
    // above, where `?since=abc` IS refused by the query extractor: the two
    // together are what distinguish "the key was ignored" from "nothing was
    // parsed at all".
    // 2. A REAL handshake on both routes, which is what the contract says
    // ("both WS upgrades still upgrade"): 101 Switching Protocols and a
    // snapshot frame, with the unrecognised key on the URL.
    let app = test_app().await;
    let cookie = sign_in_consent_callsign(&app, "lenient-ws@example.com", "w1ab").await;
    let session_id = fresh_started_session(&app, &cookie, "Lenient WS Key Net").await;

    for (label, path, auth) in [
        (
            "public",
            format!("/api/net-sessions/{session_id}/live/ws"),
            None,
        ),
        (
            "owner",
            format!("/api/net-sessions/{session_id}/ws"),
            Some(cookie.as_str()),
        ),
    ] {
        let (bare_status, bare_content_type, bare_raw) =
            get_untyped(app.router(), &path, auth).await;
        let (status, content_type, raw) =
            get_untyped(app.router(), &format!("{path}?sincee=5"), auth).await;
        assert_eq!(
            status, bare_status,
            "the {label} upgrade answers an unrecognised key exactly as no query string"
        );
        assert_ne!(
            content_type, "application/problem+json",
            "the {label} upgrade did not reject the query string: {raw}"
        );
        assert_eq!(
            content_type, bare_content_type,
            "the {label} upgrade's media type changed with an unrecognised key"
        );
        assert_eq!(
            raw, bare_raw,
            "the {label} upgrade's BODY changed with an unrecognised key — both rejections \
             are 400, so this is the assertion that discriminates"
        );
    }

    let addr = app.spawn_server().await;
    for (label, url, auth) in [
        (
            "public",
            format!("ws://{addr}/api/net-sessions/{session_id}/live/ws?sincee=5"),
            None,
        ),
        (
            "owner",
            format!("ws://{addr}/api/net-sessions/{session_id}/ws?sincee=5"),
            Some(cookie.as_str()),
        ),
    ] {
        let mut request = url.into_client_request().expect("client request");
        if let Some(auth) = auth {
            request
                .headers_mut()
                .insert(header::COOKIE, auth.parse().expect("cookie header"));
        }
        let (mut ws, response) = tokio_tungstenite::connect_async(request)
            .await
            .unwrap_or_else(|e| {
                panic!("the {label} upgrade must still complete with an unrecognised key: {e}")
            });
        assert_eq!(
            response.status().as_u16(),
            101,
            "the {label} upgrade did not switch protocols"
        );
        let snapshot = try_next_json(&mut ws, 5_000)
            .await
            .unwrap_or_else(|| panic!("the {label} upgrade sent no snapshot frame"));
        assert_eq!(
            snapshot["type"], "snapshot",
            "the {label} upgrade's first frame is the snapshot"
        );
    }
}

#[tokio::test]
async fn a_session_whose_log_can_no_longer_be_replayed_is_refused_by_name_on_both_sockets() {
    // The WS half, NAMED rather than inherited. Neither socket
    // may drop silently: every other fallible step on these paths returns
    // quietly BECAUSE the client recovers by reconnecting, but this failure is
    // permanent, so a quiet drop would put the console into an endless
    // reconnect loop against a log that will never load, showing nothing and
    // explaining nothing.
    //
    // The two sockets refuse at DIFFERENT points, and both are asserted:
    //
    // * the OWNER socket authorizes BEFORE the upgrade, and that authorization
    // loads the session row — so it answers the full 410 problem+json, with
    // the `detail`, and never upgrades at all;
    // * the PUBLIC socket has no pre-upgrade row read, so it upgrades and then
    // closes with a policy-violation frame carrying the reason.
    let app = test_app().await;
    let cookie = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;
    let session_id = fresh_started_session(&app, &cookie, "Unreplayable Net").await;
    // Plant the older snapshot shape: flat
    // band/mode and NO `connections` key.
    sqlx::query("UPDATE net_sessions SET definition_snapshot = $2 WHERE id = $1")
        .bind(uuid::Uuid::parse_str(&session_id).expect("uuid"))
        .bind(json!({
            "title": "Unreplayable Net",
            "plannedFrequencyHz": 14_230_000_i64,
            "band": "20m",
            "mode": "ssb",
            "netCategory": "traffic",
            "netType": "open"
        }))
        .execute(&app.pool)
        .await
        .expect("plant a snapshot with no connection set");
    let addr = app.spawn_server().await;

    match connect_ws(addr, &session_id, None, Some(&cookie)).await {
        Err(WsError::Http(response)) => {
            assert_eq!(
                response.status(),
                410,
                "the owner socket is refused by name, not dropped"
            );
            let body = response.body().as_ref().expect("a problem body");
            let problem: Value = serde_json::from_slice(body).expect("problem+json");
            assert_eq!(problem["type"], "/errors/unreplayable-log");
            assert!(
                problem["detail"].as_str().is_some_and(|d| !d.is_empty()),
                "the refusal names the fault"
            );
        }
        other => panic!("expected a 410 refusal, got {other:?}"),
    }

    let mut public = connect_public_ws(addr, &session_id, None)
        .await
        .expect("the public socket upgrades, then refuses post-upgrade");
    let frame = expect_policy_close(&mut public, 3_000).await;
    assert_eq!(
        u16::from(frame.code),
        1008,
        "a policy-violation close, not a silent drop"
    );
    assert!(
        frame.reason.contains("can no longer be opened"),
        "the close names the fault: {}",
        frame.reason
    );
}

#[tokio::test]
async fn the_public_frequency_delta_names_the_connection_it_moved() {
    // The FREQUENCY half, on the REAL public socket. `SessionStarted`
    // and `FrequencyChanged` shared ONE or-pattern arm in `public_payload`, and
    // the split was two decisions; this is the half a public subscriber can
    // actually observe as a delta. Without the `connectionId` a three-way net's
    // public viewer learns a number and cannot tell what it belongs to.
    //
    // Asserted here as well as in `ws::protocol`'s unit test because the unit
    // test is the only thing that was guarding it: the owner socket serializes
    // the FULL payload, so an owner-side assertion passes with the public
    // projection broken.
    //
    // ⚠️ THE OTHER HALF IS NOT REACHABLE FROM HERE, and this test used to claim
    // otherwise. `session.started` is a session's FIRST event (seq 1), and the
    // only `?since=` value whose replay would include it is 0 — which is the
    // FRESH-CONNECT branch that sends a snapshot instead of deltas. So no public
    // subscriber can ever see a `session.started` frame, and its `{}` projection
    // is pinned where it IS observable: `ws::protocol`'s
    // `public_wire_event_strips_actor_and_internal_ids_from_the_serialized_bytes`.
    let app = test_app().await;
    let cookie = sign_in_consent_callsign(&app, "owner@example.com", "w1aw").await;
    let session_id = fresh_started_session(&app, &cookie, "Public Split Net").await;
    let addr = app.spawn_server().await;

    // `?since=0` IS the fresh-connect branch (`ws/mod.rs`: `if since == 0`), so
    // what arrives first is the snapshot — asserted on the next line rather than
    // assumed, because the connection id the delta must name is read out of it.
    let mut ws = connect_public_ws(addr, &session_id, Some(0))
        .await
        .expect("public subscriber");
    let snapshot = next_json(&mut ws).await;
    assert_eq!(snapshot["type"], "snapshot");
    let connection_id = snapshot["session"]["connections"][0]["id"]
        .as_str()
        .expect("the public snapshot carries the connection set")
        .to_owned();

    let (status, _) = send_json(
        app.router(),
        "POST",
        &format!("/api/net-sessions/{session_id}/frequency"),
        Some(json!({ "connectionId": connection_id, "operatingFrequency": "7.200" })),
        Some(&cookie),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let delta = next_json(&mut ws).await;
    assert_eq!(delta["kind"], "frequency.changed");
    assert_eq!(delta["payload"]["connectionId"], connection_id);
    assert_eq!(delta["payload"]["operatingFrequencyHz"], 7_200_000);
    // The public projection is an ALLOWLIST, and its size is part of the
    // contract: a future field added to the event must be decided into the
    // public frame, not carried into it.
    assert_eq!(
        delta["payload"]
            .as_object()
            .expect("the public frequency delta is an object")
            .len(),
        2,
        "exactly the two keys the split decided on: {delta}"
    );
}
