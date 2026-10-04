// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
//! A bounded, TTL'd set of addresses that recently completed a sign-in here.
//!
//! Membership is not an exemption from the magic-link budget but permission to
//! fall back to the reserve, so total outbound mail stays bounded. Only the
//! sign-in handler writes here, and addresses are stored as SHA-256 digests.

use std::collections::HashMap;
use std::sync::Mutex;

use netroll_domain::auth::normalize_email;
use sha2::{Digest, Sha256};

/// How long a completed sign-in keeps an address eligible for the reserve budget:
/// 30 days. Long enough
/// that a user who signs in roughly monthly is never caught by a campaign that
/// saturates the cap, short enough that the set tracks the CURRENT active
/// population rather than accumulating every address the instance ever saw.
const KNOWN_ADDRESS_TTL_MILLIS: u64 = 30 * 24 * 60 * 60 * 1_000;

/// Hard ceiling on retained fingerprints (~320 KB of hashes). A safety net only:
/// the set grows solely through real completed sign-ins, so a single-instance
/// deployment reaches this only with an active population far past anything this
/// app targets. Not env-configurable — only the cap itself is.
const KNOWN_ADDRESS_CAPACITY: usize = 10_000;

/// In-memory set of recently-signed-in addresses, held on `AppState` as an
/// `Arc<KnownAddresses>`. The `std::sync::Mutex` is held only for the momentary
/// insert/lookup — never across an `.await` — so a synchronous std mutex is
/// correct, exactly as in `ws::presence::SessionPresence`.
pub struct KnownAddresses {
    /// `SHA-256(normalized address) → the epoch-millis instant it lapses`.
    entries: Mutex<HashMap<[u8; 32], u64>>,
    ttl_millis: u64,
    capacity: usize,
}

impl KnownAddresses {
    /// Creates an empty set with the documented TTL and capacity.
    pub fn new() -> Self {
        Self::with_limits(KNOWN_ADDRESS_TTL_MILLIS, KNOWN_ADDRESS_CAPACITY)
    }

    fn with_limits(ttl_millis: u64, capacity: usize) -> Self {
        Self {
            entries: Mutex::new(HashMap::new()),
            ttl_millis,
            capacity,
        }
    }

    /// Records that `email` completed a sign-in at `now_millis`, letting it fall
    /// back to the reserve send budget for the TTL. Call this ONLY after proof of
    /// control (a consumed magic link) and only once the sign-in has actually
    /// been accepted; seeding it from an unauthenticated request would hand an
    /// attacker the reserve.
    ///
    /// Infallible by construction: a full set evicts rather than refusing, so a
    /// sign-in can never fail on account of this bookkeeping.
    pub fn remember(&self, email: &str, now_millis: u64) {
        let fingerprint = fingerprint(email);
        let expires_at = now_millis.saturating_add(self.ttl_millis);
        let mut entries = self
            .entries
            .lock()
            .expect("known addresses mutex is not poisoned");

        // Sweep piggybacked on insertion (the `KeyedRateLimiter` pattern):
        // reclaim lapsed entries before ever evicting a live one.
        if entries.len() >= self.capacity {
            entries.retain(|_, lapses_at| *lapses_at > now_millis);
        }
        if entries.len() >= self.capacity {
            let soonest = entries
                .iter()
                .min_by_key(|(_, lapses_at)| **lapses_at)
                .map(|(fingerprint, _)| *fingerprint);
            if let Some(soonest) = soonest {
                entries.remove(&soonest);
            }
        }
        entries.insert(fingerprint, expires_at);
    }

    /// Whether `email` completed a sign-in within the TTL as of `now_millis`.
    /// NOT an account-existence check: an account that has never signed in is
    /// absent here just like an address with no account at all, so a caller
    /// gains no enumeration oracle from this result. Read-only: a lapsed entry
    /// reads as absent and is reclaimed by the next insertion sweep.
    pub fn contains(&self, email: &str, now_millis: u64) -> bool {
        let entries = self
            .entries
            .lock()
            .expect("known addresses mutex is not poisoned");
        entries
            .get(&fingerprint(email))
            .is_some_and(|lapses_at| *lapses_at > now_millis)
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.entries
            .lock()
            .expect("known addresses mutex is not poisoned")
            .len()
    }
}

impl Default for KnownAddresses {
    fn default() -> Self {
        Self::new()
    }
}

/// Hashes an address to the stored form. Normalizes first so both call sites —
/// the sign-in that writes and the magic-link request that reads — agree on one
/// contract, the same normalization the accounts table stores.
fn fingerprint(email: &str) -> [u8; 32] {
    Sha256::digest(normalize_email(email).as_bytes()).into()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A short TTL and a tiny capacity so the bounds are reachable in a test
    /// without a million inserts.
    fn small_set() -> KnownAddresses {
        KnownAddresses::with_limits(60_000, 4)
    }

    #[test]
    fn an_address_is_unknown_until_it_is_remembered() {
        let known = small_set();
        assert!(!known.contains("op@example.com", 1_000));

        known.remember("op@example.com", 1_000);

        assert!(known.contains("op@example.com", 1_000));
    }

    #[test]
    fn an_address_lapses_back_to_unknown_once_the_ttl_has_elapsed() {
        let known = small_set();
        known.remember("op@example.com", 1_000);

        assert!(
            known.contains("op@example.com", 1_000 + 60_000 - 1),
            "still known just inside the TTL"
        );
        assert!(
            !known.contains("op@example.com", 1_000 + 60_000),
            "unknown again once the TTL has elapsed"
        );
    }

    #[test]
    fn remembering_one_address_does_not_make_another_known() {
        let known = small_set();
        known.remember("op@example.com", 1_000);

        assert!(!known.contains("other@example.com", 1_000));
    }

    #[test]
    fn the_set_never_grows_past_its_capacity_bound_and_evicts_the_oldest_first() {
        let known = small_set();

        // 100 sign-ins, each one millisecond after the last, into a set that
        // holds 4. Nothing lapses within the run (the TTL is 60 s), so every
        // insert past the fourth must evict a LIVE entry — which one it picks is
        // the whole behavior under test.
        for n in 0..100 {
            known.remember(&format!("op{n}@example.com"), 1_000 + n);
        }
        let now = 1_099;

        assert_eq!(
            known.len(),
            4,
            "the set must stay within its capacity bound"
        );
        // Recency-ordered eviction: the survivors are exactly the four most
        // recent sign-ins. An "evict the newest" or arbitrary-victim policy
        // keeps stale addresses and drops the ones a returning user needs, so
        // assert both halves — who survived AND who was evicted.
        for recent in 96..100 {
            assert!(
                known.contains(&format!("op{recent}@example.com"), now),
                "the most recent sign-ins must survive eviction (op{recent})"
            );
        }
        for evicted in [0, 50, 95] {
            assert!(
                !known.contains(&format!("op{evicted}@example.com"), now),
                "older entries must be the ones evicted (op{evicted})"
            );
        }
    }

    #[test]
    fn expired_entries_are_reclaimed_rather_than_evicting_live_ones() {
        let known = small_set();
        for n in 0..4 {
            known.remember(&format!("stale{n}@example.com"), 1_000);
        }

        // Every earlier entry has lapsed by now, so a fresh sign-in reclaims
        // their slots instead of pushing out a live address.
        let later = 1_000 + 60_000;
        known.remember("fresh@example.com", later);

        assert!(known.contains("fresh@example.com", later));
        assert!(!known.contains("stale0@example.com", later));
        assert_eq!(known.len(), 1, "lapsed entries are swept, not retained");
    }
}
