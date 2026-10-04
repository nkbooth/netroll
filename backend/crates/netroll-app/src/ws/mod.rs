// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
//! WebSocket transport: snapshot, then delta-since-seq.
//!
//! A client receives the folded summary once, or nothing when resuming with
//! `?since=N>0`, then each newly-appended event as a delta. Fan-out is
//! in-process; Postgres remains the sole ordering authority and recovery source.

pub mod hub;
pub mod locks;
pub mod presence;
pub mod protocol;

use axum::Router;
use axum::extract::ws::WebSocketUpgrade;
use axum::extract::{Path, State};
use axum::http::HeaderMap;
use axum::response::Response;
use axum::routing::get;
use serde::Deserialize;
use uuid::Uuid;

use crate::http::net_sessions::load_owned_session;
use crate::http::problem::ApiError;
use crate::http::{AppQuery, AppState};
use crate::middleware::consent::ConsentedAccount;

/// Refuses a cross-origin WS upgrade when a browser sends an `Origin` that does
/// not match this deployment.
///
/// `SameSite=Lax` on the session cookie already blocks a cross-site page's
/// handshake from attaching it, but a WS handshake is a subresource fetch and
/// not same-origin-policy-protected, so this is defense-in-depth against
/// cross-site WebSocket hijacking. A request with NO `Origin` — any non-browser
/// client — passes: browsers always send one, so its absence is not itself a
/// hijacking signal.
fn origin_is_allowed(headers: &HeaderMap, public_base_url: &str) -> bool {
    match headers.get(axum::http::header::ORIGIN) {
        None => true,
        Some(value) => match value.to_str() {
            Ok(origin) => origin.eq_ignore_ascii_case(public_base_url),
            Err(_) => false,
        },
    }
}

/// The `?since=` resume cursor: the `seq` the client last folded. Absent → 0
/// → a full snapshot then live deltas; `N>0` → no snapshot, replay `seq>N`.
///
/// Read through [`crate::http::AppQuery`], not axum's `Query`, so a non-numeric
/// `?since=` is refused as a 400 problem+json rather than axum's `text/plain`
/// default. The public upgrade is account-less, so that rejection is
/// anonymously observable.
#[derive(Deserialize)]
pub(crate) struct WsQuery {
    #[serde(default)]
    since: u64,
}

impl crate::http::QueryKeyPolicy for WsQuery {
    const REASON: &'static str = "both WS upgrades serve a full snapshot then live events, \
         so a dropped ?since= replays a superset rather than answering something else";
}

impl crate::http::LenientQuery for WsQuery {}

/// The session-gated WebSocket route, merged into the protected tree behind
/// `require_session`.
pub fn ws_routes() -> Router<AppState> {
    Router::new().route("/api/net-sessions/{id}/ws", get(session_ws))
}

/// The PUBLIC, account-less WebSocket route, merged into
/// `public_read_routes()` so it rides the read IP governor and sits OUTSIDE
/// `require_session`. No cookie, no owner authz, and the snapshot and deltas
/// are REDACTED. Its `live/ws` literal is distinct from the owner `ws` literal,
/// so there is no matchit collision.
pub fn public_ws_routes() -> Router<AppState> {
    Router::new().route("/api/net-sessions/{id}/live/ws", get(public_session_ws))
}

/// `GET /api/net-sessions/{id}/ws` — upgrades to a WebSocket after the
/// `ViewConsole` capability check.
///
/// The authz decision happens BEFORE the upgrade, so a non-staff account gets a
/// 403, a missing session a 404 and an unauthenticated request a 401, all as
/// problem+json, and no socket ever opens.
///
/// The SAME check re-runs periodically for the life of the connection — see
/// `conn::handle_socket`'s keepalive-tick arm — so a co-owner removed, or a
/// staff member whose role is revoked mid-connection, loses `ViewConsole`
/// rather than keeping an already-open stream alive indefinitely. That tick
/// also re-checks `disabled_at`: the account-disabled gate and the bulk session
/// revocation both take effect only at HTTP-request time, so this, the one
/// long-lived authenticated surface, would otherwise survive a disable.
async fn session_ws(
    State(state): State<AppState>,
    headers: HeaderMap,
    ConsentedAccount(current): ConsentedAccount,
    Path(session_id): Path<Uuid>,
    AppQuery(params): AppQuery<WsQuery>,
    ws: WebSocketUpgrade,
) -> Result<Response, ApiError> {
    // Cross-site WebSocket hijacking defense-in-depth, before any DB work.
    if !origin_is_allowed(&headers, &state.public_base_url) {
        return Err(ApiError::Forbidden);
    }
    // ViewConsole capability over the session's roles (404-before-403).
    load_owned_session(&state, session_id, current).await?;
    let since = params.since;
    Ok(ws.on_upgrade(move |socket| conn::handle_socket(state, session_id, current, since, socket)))
}

/// `GET /api/net-sessions/{id}/live/ws` — the PUBLIC, account-less WebSocket.
///
/// Mirrors [`session_ws`] with NO `load_owned_session` authz and NO periodic
/// re-auth tick: the session id in the URL is the read capability. It KEEPS the
/// cross-site origin check and the invalid-`since` 1008 close. The snapshot and
/// every delta are redacted, so no operator account id and no internal net id
/// ever crosses this stream.
async fn public_session_ws(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(session_id): Path<Uuid>,
    AppQuery(params): AppQuery<WsQuery>,
    ws: WebSocketUpgrade,
) -> Result<Response, ApiError> {
    // Cross-site WebSocket hijacking defense-in-depth — checked before any DB
    // work (kept from the owner WS; the public stream carries no cookie, but a
    // cross-origin page opening it is still not this deployment's traffic).
    if !origin_is_allowed(&headers, &state.public_base_url) {
        return Err(ApiError::Forbidden);
    }
    let since = params.since;
    Ok(ws.on_upgrade(move |socket| conn::handle_public_socket(state, session_id, since, socket)))
}

mod conn {
    use axum::extract::ws::{CloseFrame, Message, WebSocket, close_code};
    use tokio::sync::broadcast::error::RecvError;
    use uuid::Uuid;

    use netroll_adapters::pg::is_unreplayable_log;

    use super::hub::HubMessage;
    use super::protocol::{PublicServerMessage, ServerMessage};
    use crate::http::AppState;
    use crate::http::net_sessions::{
        authorize_session_role, load_owned_session, public_view_from_row, session_summary_from_row,
    };
    use crate::http::problem::ApiError;
    use crate::middleware::session::CurrentAccount;
    use netroll_domain::authz::Capability;

    /// The reason a socket is closed when the session's stored log is too old
    /// to be replayed.
    ///
    /// Named, not silent. Every other fallible step on the snapshot path
    /// returns quietly BECAUSE the client recovers by reconnecting — but this
    /// failure is permanent, so a quiet drop would put the console into an
    /// endless reconnect loop against a log that will never load, showing
    /// nothing and explaining nothing. The HTTP surfaces answer 410 with a
    /// `detail`; this is the same answer in the transport's own vocabulary.
    const UNREPLAYABLE_LOG_CLOSE_REASON: &str =
        "this net's log predates its connection list and can no longer be opened";

    /// Closes the socket with a policy-violation frame and a short reason —
    /// the shared shape for every post-upgrade rejection this connection can
    /// raise against itself (an invalid resume cursor, a revoked
    /// authorization, a log that can never be replayed) once it's already
    /// streaming.
    async fn close_with_policy_violation(socket: &mut WebSocket, reason: &str) {
        let _ = socket
            .send(Message::Close(Some(CloseFrame {
                code: close_code::POLICY,
                reason: reason.into(),
            })))
            .await;
    }

    /// Records the commit→WS-send propagation latency: from `hub.publish`,
    /// post-commit, to this task's send of the `event` frame, excluding device
    /// render. Only the session id, seq and elapsed millis are emitted, never a
    /// callsign or payload.
    fn record_propagation(
        state: &AppState,
        session_id: Uuid,
        seq: u64,
        elapsed: std::time::Duration,
    ) {
        tracing::debug!(
            net_session_id = %session_id,
            seq,
            latency_ms = elapsed.as_millis() as u64,
            "ws event propagation latency"
        );
        if let Some(sink) = &state.ws_latency_sink {
            sink.lock()
                .expect("ws latency sink is not poisoned")
                .push(elapsed);
        }
    }

    /// Serializes and sends one server frame as a JSON text frame.
    async fn send_message(
        socket: &mut WebSocket,
        message: &ServerMessage,
    ) -> Result<(), axum::Error> {
        let text = serde_json::to_string(message).expect("ServerMessage serializes");
        socket.send(Message::Text(text.into())).await
    }

    /// Serializes and sends one REDACTED public server frame as a JSON text frame.
    async fn send_public_message(
        socket: &mut WebSocket,
        message: &PublicServerMessage,
    ) -> Result<(), axum::Error> {
        let text = serde_json::to_string(message).expect("PublicServerMessage serializes");
        socket.send(Message::Text(text.into())).await
    }

    /// Drives one PUBLIC, account-less connection: the redacted parallel of
    /// [`handle_socket`]. Same subscribe-before-read correctness and the same
    /// live loop, but the snapshot and every delta are redacted and the
    /// keepalive tick is a plain liveness Ping with NO owner re-authorization.
    pub(super) async fn handle_public_socket(
        state: AppState,
        session_id: Uuid,
        since: u64,
        mut socket: WebSocket,
    ) {
        // Subscribe BEFORE the DB read so an event committed during connect is
        // captured — no gap. The `seq > last_seq_sent` filter drops the
        // boundary duplicate (identical to the owner handler's core).
        let mut rx = state.hub.subscribe(session_id);
        let mut last_seq_sent = since;

        if since == 0 {
            let row = match state.net_sessions.find(session_id).await {
                Ok(Some(row)) => row,
                // The decode fails HERE, before the view is built, and a
                // permanent refusal must close with a REASON: every other
                // outcome is a quiet drop the client recovers from by
                // reconnecting, and this one never will.
                Err(err) if is_unreplayable_log(&err) => {
                    close_with_policy_violation(&mut socket, UNREPLAYABLE_LOG_CLOSE_REASON).await;
                    return;
                }
                _ => return,
            };
            let view = match public_view_from_row(&state, &row).await {
                Ok(view) => view,
                // A permanent refusal closes with a REASON;
                // every other error stays a quiet drop the client recovers from.
                Err(ApiError::UnreplayableLog) => {
                    close_with_policy_violation(&mut socket, UNREPLAYABLE_LOG_CLOSE_REASON).await;
                    return;
                }
                Err(_) => return,
            };
            last_seq_sent = view.latest_seq;
            let snapshot = PublicServerMessage::Snapshot {
                session: Box::new(view),
            };
            if send_public_message(&mut socket, &snapshot).await.is_err() {
                return;
            }
        } else {
            let row = match state.net_sessions.find(session_id).await {
                Ok(Some(row)) => row,
                // The decode fails HERE, before the view is built, and a
                // permanent refusal must close with a REASON: every other
                // outcome is a quiet drop the client recovers from by
                // reconnecting, and this one never will.
                Err(err) if is_unreplayable_log(&err) => {
                    close_with_policy_violation(&mut socket, UNREPLAYABLE_LOG_CLOSE_REASON).await;
                    return;
                }
                _ => return,
            };
            if since > row.last_seq as u64 {
                close_with_policy_violation(
                    &mut socket,
                    "since exceeds the session's latest sequence",
                )
                .await;
                return;
            }
            let events = match state.session_events.events_since(session_id, since).await {
                Ok(events) => events,
                // A resume whose log carries an unreplayable event is refused
                // the same way a fresh subscribe is.
                Err(ref err) if is_unreplayable_log(err) => {
                    close_with_policy_violation(&mut socket, UNREPLAYABLE_LOG_CLOSE_REASON).await;
                    return;
                }
                Err(_) => return,
            };
            for event in &events {
                if send_public_message(&mut socket, &PublicServerMessage::event(event))
                    .await
                    .is_err()
                {
                    return;
                }
                last_seq_sent = event.seq;
            }
        }

        let mut keepalive = tokio::time::interval(state.ws_keepalive_interval);
        keepalive.tick().await; // consume the immediate first tick

        loop {
            tokio::select! {
                inbound = socket.recv() => {
                    match inbound {
                        None | Some(Err(_)) => break,
                        Some(Ok(message)) => match message {
                            Message::Close(_) => break,
                            // Structurally read-only — ignore any client data frame.
                            Message::Text(_) | Message::Binary(_) => {}
                            Message::Ping(_) | Message::Pong(_) => {}
                        },
                    }
                }
                received = rx.recv() => {
                    match received {
                        Ok(HubMessage::Event(live)) => {
                            if live.event.seq > last_seq_sent {
                                if send_public_message(
                                    &mut socket,
                                    &PublicServerMessage::event(&live.event),
                                )
                                .await
                                .is_err()
                                {
                                    break;
                                }
                                last_seq_sent = live.event.seq;
                            }
                        }
                        // The PUBLIC stream NEVER emits lock frames: a soft-lock
                        // is an operator-console concern.
                        Ok(HubMessage::Lock(_)) => {}
                        // Lagged: recover the gap from Postgres (the ordering
                        // authority), redacting each recovered event.
                        Err(RecvError::Lagged(_)) => {
                            match state.session_events.events_since(session_id, last_seq_sent).await {
                                Ok(events) => {
                                    for event in &events {
                                        if event.seq > last_seq_sent {
                                            if send_public_message(
                                                &mut socket,
                                                &PublicServerMessage::event(event),
                                            )
                                            .await
                                            .is_err()
                                            {
                                                return;
                                            }
                                            last_seq_sent = event.seq;
                                        }
                                    }
                                }
                                Err(_) => break,
                            }
                        }
                        Err(RecvError::Closed) => break,
                    }
                }
                _ = keepalive.tick() => {
                    // A plain liveness Ping — NO owner re-authorization (the
                    // public stream has no owner to revoke).
                    if socket.send(Message::Ping(Vec::new().into())).await.is_err() {
                        break;
                    }
                }
            }
        }
    }

    /// Drives one WebSocket connection: subscribe-before-read snapshot and
    /// replay, then a single `tokio::select!` loop over inbound frames, the
    /// broadcast receiver and a keepalive Ping, with Lagged recovery from
    /// Postgres. The connection never appends events.
    pub(super) async fn handle_socket(
        state: AppState,
        session_id: Uuid,
        current: CurrentAccount,
        since: u64,
        mut socket: WebSocket,
    ) {
        // Capture the operator's account id before `current`
        // is moved into authz, so the presence feed and the disconnect lock-
        // release can key on it.
        let account_id = current.account_id;
        // Whether THIS connection belongs to the session's active NCS: only that
        // liveness feeds the stall heartbeat, never a plain viewer's. Assigned
        // once `row` is folded, in both the fresh-connect and resume branches.
        let is_active_ncs: bool;

        // The correctness core: subscribe BEFORE the DB read so an event
        // committed during connect is captured in the channel buffer, leaving no
        // gap. The `seq > last_seq_sent` filter drops the boundary duplicate.
        let mut rx = state.hub.subscribe(session_id);
        let mut last_seq_sent = since;

        if since == 0 {
            // Fresh connect: send the folded snapshot once, first, through the
            // same projection the HTTP summary uses. The viewer's OWN role is
            // re-resolved alongside the row so the snapshot carries `viewerRole`
            // exactly as `get_session` does, and the console reads the same
            // field over either transport. A vanished session or lost authority
            // closes quietly and the client recovers from Postgres.
            let (row, viewer_role) =
                match authorize_session_role(&state, session_id, current, Capability::ViewConsole)
                    .await
                {
                    Ok(pair) => pair,
                    Err(_) => return,
                };
            is_active_ncs = row.active_ncs_account_id == Some(account_id);
            let summary = match session_summary_from_row(&state, &row, viewer_role).await {
                Ok(summary) => summary,
                // See the public path above.
                Err(ApiError::UnreplayableLog) => {
                    close_with_policy_violation(&mut socket, UNREPLAYABLE_LOG_CLOSE_REASON).await;
                    return;
                }
                Err(_) => return,
            };
            // Record a beat on connect so a just-reconnected NCS is immediately
            // fresh, which drives the presence-driven resume. ONLY after every
            // fallible step above has succeeded: a heartbeat registered ahead of
            // a `return` that skips the disconnect cleanup leaks a stale entry.
            if is_active_ncs {
                state.session_presence.heartbeat(
                    session_id,
                    account_id,
                    state.clock.now_epoch_millis(),
                );
            }
            last_seq_sent = summary.latest_seq;
            let snapshot = ServerMessage::Snapshot {
                session: Box::new(summary),
            };
            if send_message(&mut socket, &snapshot).await.is_err() {
                state.session_presence.clear(session_id, account_id);
                return;
            }
        } else {
            // Resume: validate the client's cursor BEFORE trusting it. A
            // `since` beyond the session's true latest seq — a stale cursor, one
            // copied from another session's numbering, or a client bug — makes
            // `events_since` legitimately return zero rows and pins
            // `last_seq_sent` at an unreachable high-water mark, so the live
            // loop's filter never passes again and the connection is stranded
            // open-but-mute for its whole lifetime with no error and no signal
            // to reconnect. Reject it explicitly instead of hanging.
            let row = match state.net_sessions.find(session_id).await {
                Ok(Some(row)) => row,
                // Unreachable today: the owner upgrade authorizes first and
                // reads this same row, so an unreplayable log has already
                // answered 410 before the socket exists. Guarded anyway — the
                // authorization order is not something this function can see,
                // and the failure it prevents is a console reconnecting forever
                // against a log that will never load.
                Err(err) if is_unreplayable_log(&err) => {
                    close_with_policy_violation(&mut socket, UNREPLAYABLE_LOG_CLOSE_REASON).await;
                    return;
                }
                // Vanished between authz and now, or a read error: close
                // quietly — the client reconnects and recovers from Postgres.
                _ => return,
            };
            if since > row.last_seq as u64 {
                close_with_policy_violation(
                    &mut socket,
                    "since exceeds the session's latest sequence",
                )
                .await;
                return;
            }
            is_active_ncs = row.active_ncs_account_id == Some(account_id);
            // Resume: NO snapshot; replay events with seq > since in order.
            let events = match state.session_events.events_since(session_id, since).await {
                Ok(events) => events,
                // A resume whose log carries an unreplayable event is refused
                // the same way a fresh subscribe is.
                Err(ref err) if is_unreplayable_log(err) => {
                    close_with_policy_violation(&mut socket, UNREPLAYABLE_LOG_CLOSE_REASON).await;
                    return;
                }
                Err(_) => return,
            };
            // A resuming active NCS beats on reconnect too, ONLY after
            // `events_since` has succeeded: an early `return` on a fallible step
            // must never leave a heartbeat registered without a matching clear.
            if is_active_ncs {
                state.session_presence.heartbeat(
                    session_id,
                    account_id,
                    state.clock.now_epoch_millis(),
                );
            }
            for event in &events {
                if send_message(&mut socket, &ServerMessage::event(event))
                    .await
                    .is_err()
                {
                    if is_active_ncs {
                        state.session_presence.clear(session_id, account_id);
                    }
                    return;
                }
                last_seq_sent = event.seq;
            }
        }

        let mut keepalive = tokio::time::interval(state.ws_keepalive_interval);
        keepalive.tick().await; // consume the immediate first tick

        // Labeled so the nested Lagged-recovery send-failure can break ALL the
        // way out to the disconnect cleanup below rather than `return`ing past
        // it: the lock-release must fire on every exit.
        'conn: loop {
            tokio::select! {
                inbound = socket.recv() => {
                    match inbound {
                        // Peer closed or the transport errored: end the task; the
                        // receiver drops naturally (registry reap is a follow-up).
                        None | Some(Err(_)) => break,
                        Some(Ok(message)) => match message {
                            Message::Close(_) => break,
                            // The stream is structurally read-only — ignore any
                            // client data frame; never treat it as a mutation.
                            Message::Text(_) | Message::Binary(_) => {}
                            // axum auto-Pongs a Ping; a Pong is a liveness ack,
                            // and one from the active NCS refreshes the stall
                            // heartbeat.
                            Message::Ping(_) | Message::Pong(_) => {
                                if is_active_ncs {
                                    state.session_presence.heartbeat(
                                        session_id,
                                        account_id,
                                        state.clock.now_epoch_millis(),
                                    );
                                }
                            }
                        },
                    }
                }
                received = rx.recv() => {
                    match received {
                        Ok(HubMessage::Event(live)) => {
                            // Dedup against what the snapshot/replay already sent.
                            if live.event.seq > last_seq_sent {
                                if send_message(&mut socket, &ServerMessage::event(&live.event))
                                    .await
                                    .is_err()
                                {
                                    break;
                                }
                                last_seq_sent = live.event.seq;
                                // The elapsed span is exactly commit → this
                                // WS-send, excluding device render. Only the
                                // live-broadcast arm is steady-state
                                // propagation; the snapshot, replay and
                                // Lagged-recovery paths are catch-up reads.
                                record_propagation(
                                    &state,
                                    session_id,
                                    live.event.seq,
                                    live.published_at.elapsed(),
                                );
                            }
                        }
                        // An ephemeral soft-lock delta. It carries NO seq and
                        // never advances `last_seq_sent`: advisory UI state, not
                        // part of the folded event stream.
                        Ok(HubMessage::Lock(delta)) => {
                            if send_message(&mut socket, &ServerMessage::lock(&delta))
                                .await
                                .is_err()
                            {
                                break;
                            }
                        }
                        // The receiver fell behind the ring and dropped
                        // messages. Recover the gap from Postgres (the ordering
                        // authority), forward it, then resume — never drop the
                        // connection over a best-effort notification miss.
                        Err(RecvError::Lagged(_)) => {
                            match state.session_events.events_since(session_id, last_seq_sent).await {
                                Ok(events) => {
                                    for event in &events {
                                        if event.seq > last_seq_sent {
                                            if send_message(&mut socket, &ServerMessage::event(event))
                                                .await
                                                .is_err()
                                            {
                                                break 'conn;
                                            }
                                            last_seq_sent = event.seq;
                                        }
                                    }
                                }
                                Err(_) => break,
                            }
                        }
                        // The session's sender was dropped (no writers left): end.
                        Err(RecvError::Closed) => break,
                    }
                }
                _ = keepalive.tick() => {
                    // Re-verify the account is not disabled on the same cadence:
                    // `require_session` gates only the initial upgrade, and the
                    // bulk session revocation cannot reach an already-open socket
                    // that captured its `CurrentAccount` first. Checked BEFORE
                    // the role re-check so a disabled staff account is evicted
                    // even with its grant untouched. A transient DB error reads
                    // as "not disabled", never a false eviction.
                    match state.accounts.is_disabled(account_id).await {
                        Ok(true) => {
                            close_with_policy_violation(&mut socket, "account disabled").await;
                            break;
                        }
                        Ok(false) => {}
                        Err(_) => {}
                    }
                    // Re-verify `ViewConsole` on the same cadence as the
                    // liveness Ping: the check ran once at upgrade, but this
                    // connection can outlive an ownership change or a role
                    // revocation for as long as it stays open. Only a DEFINITIVE
                    // 403 or 404 evicts; a transient DB error is inconclusive
                    // and must not disconnect a legitimate viewer.
                    match load_owned_session(&state, session_id, current).await {
                        Ok(_) => {}
                        Err(err @ (ApiError::Forbidden | ApiError::NetSessionNotFound)) => {
                            let reason = match err {
                                ApiError::Forbidden => "no longer authorized to view this session",
                                _ => "session no longer exists",
                            };
                            close_with_policy_violation(&mut socket, reason).await;
                            break;
                        }
                        Err(_) => {}
                    }
                    if socket.send(Message::Ping(Vec::new().into())).await.is_err() {
                        break;
                    }
                    // The tick also refreshes the active NCS's heartbeat: a
                    // still-connected NCS whose client never sends an explicit
                    // Pong is nonetheless live.
                    if is_active_ncs {
                        state.session_presence.heartbeat(
                            session_id,
                            account_id,
                            state.clock.now_epoch_millis(),
                        );
                    }
                }
            }
        }

        // On disconnect, clear the active NCS's presence so the monitor sees no
        // stale heartbeat, and release EVERY soft-lock this operator held,
        // broadcasting a holder-null `lock` frame per freed entry so a competing
        // operator can edit at once rather than waiting out the TTL, which
        // remains the backstop for an unobserved drop.
        if is_active_ncs {
            state.session_presence.clear(session_id, account_id);
        }
        let freed = state
            .session_locks
            .release_all_for_holder(session_id, account_id);
        for check_in_id in freed {
            state.hub.publish_lock(
                session_id,
                crate::ws::hub::LockDelta {
                    check_in_id,
                    holder_callsign: None,
                    expires_at_millis: None,
                },
            );
        }
    }
}
