// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
//! The ephemeral per-session NCS presence registry.
//!
//! Fed by the active NCS's WebSocket keepalive Pong, never persisted and never
//! on the wire; only the durable transitions it drives reach the log. The
//! authority is the control status stored on the session.

use std::collections::HashMap;
use std::sync::Mutex;

use uuid::Uuid;

/// In-memory registry of per-session, per-account presence heartbeats.
///
/// Held in `AppState` as an `Arc<SessionPresence>`. The `std::sync::Mutex` is
/// only ever held for the momentary record/read/drop — never across an `.await`
/// — so a synchronous std mutex (not a tokio mutex) is correct, exactly as in
/// [`super::hub::SessionHub`] / [`super::locks::SessionLocks`].
pub struct SessionPresence {
    /// `session_id → (account_id → last-heartbeat epoch millis)`.
    heartbeats: Mutex<HashMap<Uuid, HashMap<Uuid, u64>>>,
}

impl SessionPresence {
    /// Creates an empty registry.
    pub fn new() -> Self {
        Self {
            heartbeats: Mutex::new(HashMap::new()),
        }
    }

    /// Records (or renews) `account_id`'s presence in `session_id` at
    /// `now_millis` — called on connect and on each inbound keepalive Pong from
    /// the active NCS. Last-write-wins: a fresh beat overwrites the prior instant.
    pub fn heartbeat(&self, session_id: Uuid, account_id: Uuid, now_millis: u64) {
        let mut beats = self
            .heartbeats
            .lock()
            .expect("session presence mutex is not poisoned");
        beats
            .entry(session_id)
            .or_default()
            .insert(account_id, now_millis);
    }

    /// The last-heartbeat instant (epoch millis) recorded for `account_id` in
    /// `session_id`, or `None` if none is recorded (never connected, or dropped).
    /// The presence monitor compares `now - last_seen` against the stall
    /// threshold; a `None` active NCS is treated as "no presence" → stallable.
    pub fn last_seen(&self, session_id: Uuid, account_id: Uuid) -> Option<u64> {
        let beats = self
            .heartbeats
            .lock()
            .expect("session presence mutex is not poisoned");
        beats
            .get(&session_id)
            .and_then(|per_session| per_session.get(&account_id))
            .copied()
    }

    /// Eagerly clears `account_id`'s presence in `session_id` — called the
    /// instant their WS connection drops (a clean disconnect), so the monitor
    /// sees no stale heartbeat. A half-open drop that never signals is caught by
    /// the threshold instead (the heartbeat simply ages out). Idempotent: an
    /// absent entry is a no-op. Prunes the per-session map when it empties.
    pub fn clear(&self, session_id: Uuid, account_id: Uuid) {
        let mut beats = self
            .heartbeats
            .lock()
            .expect("session presence mutex is not poisoned");
        if let Some(per_session) = beats.get_mut(&session_id) {
            per_session.remove(&account_id);
            if per_session.is_empty() {
                beats.remove(&session_id);
            }
        }
    }
}

impl Default for SessionPresence {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ids() -> (Uuid, Uuid, Uuid) {
        (
            Uuid::from_u128(1),  // session
            Uuid::from_u128(10), // account A (the active NCS)
            Uuid::from_u128(11), // account B
        )
    }

    #[test]
    fn heartbeat_records_the_instant_and_last_seen_reads_it_back() {
        let (s, a, _b) = ids();
        let presence = SessionPresence::new();
        presence.heartbeat(s, a, 1_000);
        assert_eq!(presence.last_seen(s, a), Some(1_000));
    }

    #[test]
    fn a_later_heartbeat_renews_last_seen_last_write_wins() {
        let (s, a, _b) = ids();
        let presence = SessionPresence::new();
        presence.heartbeat(s, a, 1_000);
        presence.heartbeat(s, a, 90_000);
        assert_eq!(presence.last_seen(s, a), Some(90_000));
    }

    #[test]
    fn last_seen_is_none_for_an_account_that_never_beat() {
        let (s, a, b) = ids();
        let presence = SessionPresence::new();
        presence.heartbeat(s, a, 1_000);
        // B never beat in this session → no presence (stallable if B were active).
        assert_eq!(presence.last_seen(s, b), None);
    }

    #[test]
    fn drop_clears_presence_and_is_idempotent() {
        let (s, a, _b) = ids();
        let presence = SessionPresence::new();
        presence.heartbeat(s, a, 1_000);
        presence.clear(s, a);
        assert_eq!(presence.last_seen(s, a), None);
        // Dropping again is a harmless no-op.
        presence.clear(s, a);
        assert_eq!(presence.last_seen(s, a), None);
    }

    #[test]
    fn presence_is_scoped_per_session() {
        let a = Uuid::from_u128(10);
        let s1 = Uuid::from_u128(1);
        let s2 = Uuid::from_u128(2);
        let presence = SessionPresence::new();
        presence.heartbeat(s1, a, 1_000);
        // The same account in a DIFFERENT session has independent presence.
        assert_eq!(presence.last_seen(s2, a), None);
        assert_eq!(presence.last_seen(s1, a), Some(1_000));
    }
}
