// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
//! In-process per-session broadcast fan-out — deliberately in-memory, since a
//! mutation and the WS task that must see it share a node today;
//! `SessionHub::publish` is the one seam a Valkey round-trip swaps in later.
//! A writer publishes STRICTLY POST-COMMIT. Notify-only: Postgres remains the
//! sole ordering authority, and a missed broadcast recovers from the durable log.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use netroll_domain::event::SessionEvent;
use tokio::sync::broadcast;
use uuid::Uuid;

/// A published event paired with the instant it was published — the commit
/// instant for the real-time SLO. `publish` runs STRICTLY
/// POST-COMMIT, so `published_at` marks server-commit; the WS task measures the
/// elapsed to its `event`-frame send, yielding the commit→WS-send propagation
/// latency (excludes device render). The `Arc` keeps per-subscriber fan-out
/// clones cheap.
#[derive(Clone)]
pub struct LiveEvent {
    /// The appended event to stream.
    pub event: Arc<SessionEvent>,
    /// The commit/publish instant, for the commit→WS-send SLO measurement.
    pub published_at: Instant,
}

/// An ephemeral soft-lock state change fanned out to OTHER operator consoles
/// A NON-event advisory signal, never folded into
/// `SessionState`, never persisted, never on the public wire. `holder_callsign`
/// / `expires_at_millis` are `None` on release (the entry is now free).
#[derive(Clone)]
pub struct LockDelta {
    /// The entry whose lock changed.
    pub check_in_id: Uuid,
    /// The holder's callsign while held; `None` on release.
    pub holder_callsign: Option<String>,
    /// The lease expiry, epoch millis, while held; `None` on release.
    pub expires_at_millis: Option<u64>,
}

/// A message fanned out over a session's broadcast channel: an
/// appended event delta, OR an ephemeral lock delta. Widening the payload to
/// this enum lets the ONE per-session fan-out carry both the seq-ordered event
/// stream and the non-event advisory lock signal — the WS owner task forwards
/// both (as distinct frame types), the public task forwards only events.
#[derive(Clone)]
pub enum HubMessage {
    /// A durable, seq-ordered appended event.
    Event(LiveEvent),
    /// An ephemeral, seq-less soft-lock advisory delta (owner consoles only).
    Lock(LockDelta),
}

/// Per-session broadcast ring capacity. Overflow makes a slow receiver observe
/// `RecvError::Lagged`, which the connection handler recovers from by
/// re-reading Postgres — so this bounds memory without costing
/// correctness. Tuned conservatively for the expected scale (~a few hundred
/// concurrent connections); revisit if that grows.
const WS_BROADCAST_CAPACITY: usize = 256;

/// In-process registry of per-session broadcast channels.
///
/// Held in `AppState` as an `Arc<SessionHub>`. The `std::sync::Mutex` is only
/// ever held for the momentary get-or-create / send — never across an `.await`
/// — so a synchronous std mutex (not a tokio mutex) is correct here.
pub struct SessionHub {
    channels: Mutex<HashMap<Uuid, broadcast::Sender<HubMessage>>>,
}

impl SessionHub {
    /// Creates an empty hub with no session channels.
    pub fn new() -> Self {
        Self {
            channels: Mutex::new(HashMap::new()),
        }
    }

    /// Subscribes to a session's live event stream, creating the channel on
    /// first subscribe. Every event published for `session_id` after this call
    /// returns is delivered to the returned receiver (until it lags past the
    /// ring or is dropped).
    pub fn subscribe(&self, session_id: Uuid) -> broadcast::Receiver<HubMessage> {
        let mut channels = self
            .channels
            .lock()
            .expect("session hub mutex is not poisoned");
        let sender = channels
            .entry(session_id)
            .or_insert_with(|| broadcast::channel(WS_BROADCAST_CAPACITY).0);
        sender.subscribe()
    }

    /// Publishes an event to a session's live subscribers, stamping the publish
    /// instant on the envelope for the commit→WS-send SLO measurement. A
    /// no-op when the session has no channel/receivers (a `SendError` is
    /// expected and ignored — a fresh connect reads the event from Postgres).
    /// MUST be called only after the persisting transaction has committed,
    /// so `published_at` is a faithful server-commit instant.
    pub fn publish(&self, session_id: Uuid, event: &SessionEvent) {
        let channels = self
            .channels
            .lock()
            .expect("session hub mutex is not poisoned");
        if let Some(sender) = channels.get(&session_id) {
            // A `SendError` means zero live receivers — expected and ignored; a
            // fresh connect reads the event from Postgres instead.
            let _ = sender.send(HubMessage::Event(LiveEvent {
                event: Arc::new(event.clone()),
                published_at: Instant::now(),
            }));
        }
    }

    /// Publishes an ephemeral soft-lock delta to a session's live subscribers
    /// A no-op when the session has no channel/receivers — an
    /// advisory signal, so a miss is harmless (unlike an event, there is nothing
    /// to recover from Postgres; a lock is never persisted). Owner WS tasks
    /// forward it as a `lock` frame; the public WS task drops it.
    pub fn publish_lock(&self, session_id: Uuid, delta: LockDelta) {
        let channels = self
            .channels
            .lock()
            .expect("session hub mutex is not poisoned");
        if let Some(sender) = channels.get(&session_id) {
            let _ = sender.send(HubMessage::Lock(delta));
        }
    }
}

impl Default for SessionHub {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use netroll_domain::event::SessionEventBody;

    fn started_event(seq: u64) -> SessionEvent {
        SessionEvent {
            seq,
            actor_id: Some(Uuid::from_u128(100)),
            at: 1_700_000_000_000,
            body: SessionEventBody::SessionStarted {
                definition_id: Uuid::from_u128(7),
                definition_version: 3,
            },
        }
    }

    /// Unwraps a `HubMessage::Event`, panicking on a `Lock` — the test helper
    /// for the event-stream assertions.
    fn expect_event(message: HubMessage) -> LiveEvent {
        match message {
            HubMessage::Event(live) => live,
            HubMessage::Lock(_) => panic!("expected an event, got a lock delta"),
        }
    }

    #[tokio::test]
    async fn publish_reaches_a_subscriber_in_order() {
        let hub = SessionHub::new();
        let session = Uuid::from_u128(1);
        let mut rx = hub.subscribe(session);

        hub.publish(session, &started_event(1));

        let received = expect_event(rx.recv().await.expect("subscriber receives the event"));
        assert_eq!(received.event.seq, 1);
    }

    #[tokio::test]
    async fn publish_lock_reaches_a_subscriber_as_a_lock_message() {
        let hub = SessionHub::new();
        let session = Uuid::from_u128(1);
        let mut rx = hub.subscribe(session);

        hub.publish_lock(
            session,
            LockDelta {
                check_in_id: Uuid::from_u128(42),
                holder_callsign: Some("W1AW".to_owned()),
                expires_at_millis: Some(1_700_000_015_000),
            },
        );

        match rx.recv().await.expect("subscriber receives the lock") {
            HubMessage::Lock(delta) => {
                assert_eq!(delta.check_in_id, Uuid::from_u128(42));
                assert_eq!(delta.holder_callsign.as_deref(), Some("W1AW"));
            }
            HubMessage::Event(_) => panic!("expected a lock delta, got an event"),
        }
    }

    #[tokio::test]
    async fn publish_with_no_subscribers_is_a_no_op() {
        let hub = SessionHub::new();
        // No subscribe: publish must not panic and simply drops the event.
        hub.publish(Uuid::from_u128(2), &started_event(1));
    }

    #[tokio::test]
    async fn two_subscribers_of_one_session_both_receive() {
        let hub = SessionHub::new();
        let session = Uuid::from_u128(3);
        let mut rx_a = hub.subscribe(session);
        let mut rx_b = hub.subscribe(session);

        hub.publish(session, &started_event(5));

        assert_eq!(
            expect_event(rx_a.recv().await.expect("a receives"))
                .event
                .seq,
            5
        );
        assert_eq!(
            expect_event(rx_b.recv().await.expect("b receives"))
                .event
                .seq,
            5
        );
    }

    #[tokio::test]
    async fn distinct_sessions_are_isolated() {
        let hub = SessionHub::new();
        let session_a = Uuid::from_u128(10);
        let session_b = Uuid::from_u128(11);
        let mut rx_a = hub.subscribe(session_a);
        let _rx_b = hub.subscribe(session_b);

        hub.publish(session_b, &started_event(1));

        // A's receiver must see nothing — the B publish is isolated to B.
        assert!(
            rx_a.try_recv().is_err(),
            "a session's subscriber never observes another session's events"
        );
    }
}
