// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
//! The background presence sweep: stall detection, resume, and auto-close.
//!
//! Each tick reads every live session's denormalized control snapshot in one
//! indexed query, never folding a log, and cross-references the ephemeral
//! presence registry. A returning heartbeat resumes a stalled net by itself.

use std::sync::Arc;
use std::time::Duration;

use netroll_adapters::pg::net_sessions::{
    CloseOutcome, LiveSessionControl, NetSessionRepo, ResumeOutcome, StallOutcome,
};
use netroll_domain::event::SessionEvent;
use netroll_domain::fold::ControlState;
use netroll_domain::ports::Clock;

use crate::delivery::DeliveryService;
use crate::ws::hub::SessionHub;
use crate::ws::presence::SessionPresence;

/// How often the presence sweep ticks. Aligned with the 30s WS
/// keepalive — a finer cadence than the finalizer's 60s because the stall
/// threshold is ~90s, so at most one tick of lag before a dropped NCS is stalled.
pub const PRESENCE_MONITOR_TICK: Duration = Duration::from_secs(30);

/// The stall threshold in millis (resolves the open decision).
/// `90_000` = 3 × the 30s WS keepalive interval: the active NCS must have missed
/// THREE consecutive heartbeats before the net is declared stalled, so a single
/// dropped keepalive (a transient blip) never pauses a healthy net.
pub const STALL_THRESHOLD_MILLIS: u64 = 90_000;

/// The auto-close window in millis:
/// `900_000` = 15 minutes from the stall instant. A net abandoned this long with
/// no returned presence and no claim auto-closes and produces its summary.
pub const AUTO_CLOSE_MILLIS: u64 = 900_000;

/// Spawns the background presence sweep loop, mirroring
/// [`crate::finalizer::spawn_deletion_finalizer`]. Each tick runs one idempotent
/// [`run_presence_monitor_tick`] against the injected clock; a transient DB error
/// is logged (STATIC line — never PII) and the loop CONTINUES, self-healing
/// next tick. Killing the loop would leave nets stuck stalled or PII/summaries
/// unprocessed, so swallow-and-continue is the correct posture here.
///
/// No general job runner is coming to absorb this loop, for the same reason as
/// [`crate::finalizer::spawn_deletion_finalizer`]. Outbound delivery, which
/// this sweep's auto-close branch spawns, used to be the exception — a close
/// write now plans its delivery legs inside its own transaction, so a tick
/// that dies right after close owes nothing it cannot pay; see
/// [`crate::delivery`] and [`crate::delivery_sweeper`].
pub fn spawn_session_presence_monitor(
    net_sessions: NetSessionRepo,
    hub: Arc<SessionHub>,
    session_presence: Arc<SessionPresence>,
    clock: Arc<dyn Clock + Send + Sync>,
    delivery: DeliveryService,
) {
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(PRESENCE_MONITOR_TICK);
        loop {
            ticker.tick().await;
            if let Err(_e) = run_presence_monitor_tick(
                &net_sessions,
                &hub,
                &session_presence,
                clock.now_epoch_millis(),
                STALL_THRESHOLD_MILLIS,
                AUTO_CLOSE_MILLIS,
                &delivery,
            )
            .await
            {
                // Static line only — sqlx error text can embed row values.
                tracing::error!("presence monitor sweep failed; retrying next tick");
            }
        }
    });
}

/// Runs ONE idempotent presence sweep tick. Separated from the
/// spawn loop and parameterized on `now_millis` + the thresholds so an
/// integration test drives exactly one tick with a fake clock and a seeded
/// presence registry — no sleeps, no real 30s wait. For each live session
/// it stalls, resumes, or auto-closes per the denormalized control snapshot and
/// the presence registry, publishing each minted event post-commit over the hub.
pub async fn run_presence_monitor_tick(
    net_sessions: &NetSessionRepo,
    hub: &SessionHub,
    session_presence: &SessionPresence,
    now_millis: u64,
    stall_threshold_millis: u64,
    auto_close_millis: u64,
    delivery: &DeliveryService,
) -> Result<(), sqlx::Error> {
    let live = net_sessions.list_live_control_states().await?;
    for snapshot in live {
        match snapshot.control_state {
            ControlState::Active => {
                if active_ncs_presence_is_stale(
                    session_presence,
                    &snapshot,
                    now_millis,
                    stall_threshold_millis,
                ) {
                    stall_session(net_sessions, hub, snapshot.session_id, now_millis).await?;
                }
            }
            ControlState::Stalled => {
                if !active_ncs_presence_is_stale(
                    session_presence,
                    &snapshot,
                    now_millis,
                    stall_threshold_millis,
                ) {
                    // The active NCS's presence returned before auto-close: resume.
                    resume_session(net_sessions, hub, snapshot.session_id, now_millis).await?;
                } else if let Some(stalled_at) = snapshot.stalled_at_millis
                    && now_millis.saturating_sub(stalled_at) >= auto_close_millis
                {
                    auto_close_session(
                        net_sessions,
                        hub,
                        snapshot.session_id,
                        now_millis,
                        delivery,
                    )
                    .await?;
                }
            }
        }
    }
    Ok(())
}

/// Whether the active NCS's presence is stale (missed the stall threshold) — the
/// stall/resume predicate. A session with NO active NCS (an older row, or one
/// whose NCS account was deleted → the FK SET NULL) is treated as stale: nobody
/// is running the net, so it is stallable (the safe reading).
fn active_ncs_presence_is_stale(
    session_presence: &SessionPresence,
    snapshot: &LiveSessionControl,
    now_millis: u64,
    stall_threshold_millis: u64,
) -> bool {
    match snapshot.active_ncs_account_id {
        None => true,
        Some(active_ncs) => match session_presence.last_seen(snapshot.session_id, active_ncs) {
            None => true,
            Some(last_seen) => now_millis.saturating_sub(last_seen) > stall_threshold_millis,
        },
    }
}

/// Mints `ncs.stalled` for a session (idempotently) and publishes it.
async fn stall_session(
    net_sessions: &NetSessionRepo,
    hub: &SessionHub,
    session_id: uuid::Uuid,
    now_millis: u64,
) -> Result<(), sqlx::Error> {
    if let StallOutcome::Stalled(event) = net_sessions.stall(session_id, now_millis).await? {
        publish(hub, session_id, &event);
        tracing::info!(net_session_id = %session_id, "net control stalled");
    }
    Ok(())
}

/// Mints `ncs.resumed` for a session (idempotently) and publishes it.
async fn resume_session(
    net_sessions: &NetSessionRepo,
    hub: &SessionHub,
    session_id: uuid::Uuid,
    now_millis: u64,
) -> Result<(), sqlx::Error> {
    if let ResumeOutcome::Resumed(event) = net_sessions.resume(session_id, now_millis).await? {
        publish(hub, session_id, &event);
        tracing::info!(net_session_id = %session_id, "net control resumed");
    }
    Ok(())
}

/// Auto-closes an abandoned stalled session via the SHIPPED close write and
/// publishes the `session.closed` (system-originated, `actor_id = None`).
async fn auto_close_session(
    net_sessions: &NetSessionRepo,
    hub: &SessionHub,
    session_id: uuid::Uuid,
    now_millis: u64,
    delivery: &DeliveryService,
) -> Result<(), sqlx::Error> {
    if let CloseOutcome::Closed(event) = net_sessions.close(session_id, now_millis, None).await? {
        publish(hub, session_id, &event);
        // The auto-close path delivers exactly like the manual close
        // — a net abandoned by its NCS still emails/POSTs its summary. Only on
        // CloseOutcome::Closed (a real close that appended an event); spawned and
        // never awaited, so a slow target cannot stall the sweep tick. The
        // stall/resume branches never deliver.
        delivery.spawn_for_closed_session(session_id);
        tracing::info!(net_session_id = %session_id, "abandoned net auto-closed");
    }
    Ok(())
}

/// Post-commit notify: publish the minted control event over the in-process hub.
fn publish(hub: &SessionHub, session_id: uuid::Uuid, event: &SessionEvent) {
    hub.publish(session_id, event);
}
