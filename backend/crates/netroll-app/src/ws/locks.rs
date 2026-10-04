// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
//! The ephemeral per-record soft-lock registry.
//!
//! Never persisted and never on the public wire. It refuses a competing acquire
//! and names the holder, but the CORRECTNESS authority is the command-time
//! version CAS — the lock is advisory. Expiry is by the injected clock.

use std::collections::HashMap;
use std::sync::Mutex;

use uuid::Uuid;

/// The sliding TTL of a soft-lock lease, in millis (~15s). The
/// client re-`acquire`s on a heartbeat while the modal is open to slide it; a
/// holder that stops renewing lets the lease self-expire.
pub const LOCK_TTL_MILLIS: u64 = 15_000;

/// One held lease: who holds it and when it expires (epoch millis).
struct Lease {
    holder_account_id: Uuid,
    holder_callsign: String,
    expires_at_millis: u64,
}

/// The public view of an acquired/renewed lease, returned to the acquiring
/// client and broadcast to other consoles.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LeaseView {
    /// The holder's callsign (for the "X is editing…" indicator).
    pub holder_callsign: String,
    /// When the lease expires, epoch millis.
    pub expires_at_millis: u64,
}

/// The outcome of [`SessionLocks::acquire`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AcquireOutcome {
    /// The caller now holds (or renewed) the lease.
    Acquired(LeaseView),
    /// A DIFFERENT account holds a valid lease; its holder callsign is carried
    /// so the 409 `lock-held` detail can name who is editing.
    Held(String),
}

/// In-memory registry of per-session, per-check-in soft-lock leases.
///
/// Held in `AppState` as an `Arc<SessionLocks>`. The `std::sync::Mutex` is only
/// ever held for the momentary get-or-create / expiry-check — never across an
/// `.await` — so a synchronous std mutex (not a tokio mutex) is correct, exactly
/// as in [`super::hub::SessionHub`].
pub struct SessionLocks {
    leases: Mutex<HashMap<Uuid, HashMap<Uuid, Lease>>>,
}

impl SessionLocks {
    /// Creates an empty registry.
    pub fn new() -> Self {
        Self {
            leases: Mutex::new(HashMap::new()),
        }
    }

    /// Acquires — or, if the caller already holds it, RENEWS (slides the TTL) —
    /// the lease on `(session_id, check_in_id)`. Refuses with [`AcquireOutcome::Held`]
    /// only when a DIFFERENT account holds a still-VALID (unexpired) lease. An
    /// expired lease is silently reclaimable by anyone (lazy expiry).
    pub fn acquire(
        &self,
        session_id: Uuid,
        check_in_id: Uuid,
        account_id: Uuid,
        holder_callsign: &str,
        now_millis: u64,
    ) -> AcquireOutcome {
        let mut leases = self
            .leases
            .lock()
            .expect("session locks mutex is not poisoned");
        let per_session = leases.entry(session_id).or_default();
        if let Some(existing) = per_session.get(&check_in_id)
            && existing.expires_at_millis > now_millis
            && existing.holder_account_id != account_id
        {
            return AcquireOutcome::Held(existing.holder_callsign.clone());
        }
        let expires_at_millis = now_millis + LOCK_TTL_MILLIS;
        per_session.insert(
            check_in_id,
            Lease {
                holder_account_id: account_id,
                holder_callsign: holder_callsign.to_owned(),
                expires_at_millis,
            },
        );
        AcquireOutcome::Acquired(LeaseView {
            holder_callsign: holder_callsign.to_owned(),
            expires_at_millis,
        })
    }

    /// Releases the lease on `(session_id, check_in_id)` — but only when the
    /// caller holds it, OR the lease has already expired (idempotent). Returns
    /// `true` when a lease was actually removed (so the caller broadcasts a
    /// holder-null `lock` frame). Releasing an absent/foreign-held valid lease
    /// returns `false` (no broadcast, still a 204 at the HTTP layer).
    pub fn release(
        &self,
        session_id: Uuid,
        check_in_id: Uuid,
        account_id: Uuid,
        now_millis: u64,
    ) -> bool {
        let mut leases = self
            .leases
            .lock()
            .expect("session locks mutex is not poisoned");
        if let Some(per_session) = leases.get_mut(&session_id)
            && let Some(existing) = per_session.get(&check_in_id)
            && (existing.holder_account_id == account_id
                || existing.expires_at_millis <= now_millis)
        {
            per_session.remove(&check_in_id);
            return true;
        }
        false
    }

    /// Releases EVERY lease held by `account_id` in `session_id`, returning the
    /// freed `check_in_id`s. Wired to fire the instant the
    /// holder's WS connection drops or the presence monitor observes their
    /// presence gone — so their soft-locks release IMMEDIATELY (a competing
    /// operator can edit at once) instead of waiting out the ~15s sliding TTL.
    /// The TTL REMAINS the backstop for an unobserved half-open drop that never
    /// signals a clean disconnect (defense in depth). The caller broadcasts a
    /// holder-null `lock` frame per returned id. Foreign-held leases are
    /// untouched; a holder with no leases returns an empty vec (idempotent).
    pub fn release_all_for_holder(&self, session_id: Uuid, account_id: Uuid) -> Vec<Uuid> {
        let mut leases = self
            .leases
            .lock()
            .expect("session locks mutex is not poisoned");
        let Some(per_session) = leases.get_mut(&session_id) else {
            return Vec::new();
        };
        let freed: Vec<Uuid> = per_session
            .iter()
            .filter(|(_, lease)| lease.holder_account_id == account_id)
            .map(|(check_in_id, _)| *check_in_id)
            .collect();
        for check_in_id in &freed {
            per_session.remove(check_in_id);
        }
        freed
    }

    /// The current VALID holder of `(session_id, check_in_id)`, if any — the
    /// edit handler's lock check. An expired lease reads as no holder.
    pub fn holder(
        &self,
        session_id: Uuid,
        check_in_id: Uuid,
        now_millis: u64,
    ) -> Option<(Uuid, String)> {
        let leases = self
            .leases
            .lock()
            .expect("session locks mutex is not poisoned");
        leases
            .get(&session_id)
            .and_then(|per_session| per_session.get(&check_in_id))
            .filter(|lease| lease.expires_at_millis > now_millis)
            .map(|lease| (lease.holder_account_id, lease.holder_callsign.clone()))
    }
}

impl Default for SessionLocks {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ids() -> (Uuid, Uuid, Uuid, Uuid) {
        (
            Uuid::from_u128(1),  // session
            Uuid::from_u128(2),  // check-in
            Uuid::from_u128(10), // account A
            Uuid::from_u128(11), // account B
        )
    }

    #[test]
    fn acquire_grants_a_lease_with_the_sliding_ttl() {
        let (s, c, a, _b) = ids();
        let locks = SessionLocks::new();
        let outcome = locks.acquire(s, c, a, "W1AW", 1_000);
        assert_eq!(
            outcome,
            AcquireOutcome::Acquired(LeaseView {
                holder_callsign: "W1AW".to_owned(),
                expires_at_millis: 1_000 + LOCK_TTL_MILLIS,
            })
        );
    }

    #[test]
    fn a_competing_acquire_by_another_account_is_held() {
        let (s, c, a, b) = ids();
        let locks = SessionLocks::new();
        locks.acquire(s, c, a, "W1AW", 1_000);
        let outcome = locks.acquire(s, c, b, "W2BCD", 2_000);
        assert_eq!(outcome, AcquireOutcome::Held("W1AW".to_owned()));
    }

    #[test]
    fn the_holder_renewing_slides_the_ttl() {
        let (s, c, a, _b) = ids();
        let locks = SessionLocks::new();
        locks.acquire(s, c, a, "W1AW", 1_000);
        let renewed = locks.acquire(s, c, a, "W1AW", 5_000);
        assert_eq!(
            renewed,
            AcquireOutcome::Acquired(LeaseView {
                holder_callsign: "W1AW".to_owned(),
                expires_at_millis: 5_000 + LOCK_TTL_MILLIS,
            })
        );
    }

    #[test]
    fn an_expired_lease_is_reclaimable_by_a_different_account() {
        let (s, c, a, b) = ids();
        let locks = SessionLocks::new();
        locks.acquire(s, c, a, "W1AW", 1_000);
        // Past the TTL: B may now acquire it (the abandoned-holder proxy).
        let outcome = locks.acquire(s, c, b, "W2BCD", 1_000 + LOCK_TTL_MILLIS + 1);
        assert!(matches!(outcome, AcquireOutcome::Acquired(_)));
    }

    #[test]
    fn holder_reports_a_valid_lease_and_none_after_expiry() {
        let (s, c, a, _b) = ids();
        let locks = SessionLocks::new();
        locks.acquire(s, c, a, "W1AW", 1_000);
        assert_eq!(locks.holder(s, c, 2_000), Some((a, "W1AW".to_owned())));
        // After the TTL the lease is invalid: no holder.
        assert_eq!(locks.holder(s, c, 1_000 + LOCK_TTL_MILLIS + 1), None);
    }

    #[test]
    fn release_by_the_holder_frees_the_lease_and_reports_it() {
        let (s, c, a, _b) = ids();
        let locks = SessionLocks::new();
        locks.acquire(s, c, a, "W1AW", 1_000);
        assert!(locks.release(s, c, a, 2_000));
        assert_eq!(locks.holder(s, c, 2_000), None);
    }

    #[test]
    fn releasing_an_absent_lease_is_idempotent_false() {
        let (s, c, a, _b) = ids();
        let locks = SessionLocks::new();
        // Nothing to release.
        assert!(!locks.release(s, c, a, 1_000));
    }

    #[test]
    fn a_non_holder_cannot_release_a_valid_lease() {
        let (s, c, a, b) = ids();
        let locks = SessionLocks::new();
        locks.acquire(s, c, a, "W1AW", 1_000);
        // B is not the holder and the lease is still valid → no release.
        assert!(!locks.release(s, c, b, 2_000));
        assert_eq!(locks.holder(s, c, 2_000), Some((a, "W1AW".to_owned())));
    }

    #[test]
    fn release_all_for_holder_frees_only_the_holders_leases_and_returns_their_ids() {
        // An operator's presence drop releases EVERY lease
        // they hold in the session at once (immediate, not TTL-waited), leaving
        // other operators' leases untouched.
        let s = Uuid::from_u128(1);
        let a = Uuid::from_u128(10);
        let b = Uuid::from_u128(11);
        let c1 = Uuid::from_u128(100);
        let c2 = Uuid::from_u128(101);
        let c3 = Uuid::from_u128(102);
        let locks = SessionLocks::new();
        locks.acquire(s, c1, a, "W1AW", 1_000);
        locks.acquire(s, c2, a, "W1AW", 1_000);
        locks.acquire(s, c3, b, "W2BCD", 1_000);

        let mut freed = locks.release_all_for_holder(s, a);
        freed.sort();
        assert_eq!(freed, vec![c1, c2], "both of A's leases freed, returned");
        // A's leases are gone; B's lease is untouched.
        assert_eq!(locks.holder(s, c1, 2_000), None);
        assert_eq!(locks.holder(s, c2, 2_000), None);
        assert_eq!(locks.holder(s, c3, 2_000), Some((b, "W2BCD".to_owned())));
    }

    #[test]
    fn release_all_for_holder_with_no_leases_returns_empty() {
        let s = Uuid::from_u128(1);
        let a = Uuid::from_u128(10);
        let locks = SessionLocks::new();
        assert!(locks.release_all_for_holder(s, a).is_empty());
    }
}
