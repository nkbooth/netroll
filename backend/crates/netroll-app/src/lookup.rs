// SPDX-License-Identifier: RPL-1.5
// Copyright (C) 2026 Nick Booth, N1CCK. See LICENSE.
//! Callbook lookup orchestration and the in-process result cache. Resolve
//! credentials, check the cache, apply the per-account limit, try QRZ, fall
//! back to hamcall.dev, never surfacing a provider error. Cache and limiter
//! are deliberately in-process — the one seam a future Valkey adapter swaps
//! in without changing observable behavior.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use netroll_adapters::pg::qrz_credentials::QrzCredentialRepo;
use netroll_domain::lookup::{CallsignRecord, LookupError};
use netroll_domain::ports::{Clock, CredentialCipher, LookupProvider};
use netroll_domain::qrz::QrzCredentials;
use uuid::Uuid;

use crate::http::rate_limit::LookupRateLimiter;

/// Default positive-hit TTL: 24h. QRZ data changes slowly and hamcall is a
/// weekly FCC extract, so a day-long cache is safe and spares the upstreams.
const DEFAULT_TTL: Duration = Duration::from_secs(24 * 60 * 60);

/// One cached lookup result and the epoch-millis instant it expires.
struct CacheEntry {
    record: CallsignRecord,
    expires_at_millis: u64,
}

/// In-process, TTL-bounded result cache keyed by normalized callsign — the
/// Valkey swap-seam the module doc names. Caches POSITIVE hits only; a miss is
/// cheap to re-derive and never cached (a documented short-TTL negative cache
/// would slot in here). Stale entries are swept on write, mirroring the
/// `KeyedRateLimiter::retain_recent()` sweep, so memory stays bounded by live
/// keys. Time enters through the injected [`Clock`] so the TTL is testable
/// with a fake clock, no `sleep`.
pub struct LookupCache {
    entries: Mutex<HashMap<String, CacheEntry>>,
    ttl_millis: u64,
    clock: Arc<dyn Clock + Send + Sync>,
}

impl LookupCache {
    /// Builds a cache with the default 24h TTL over the injected clock.
    pub fn new(clock: Arc<dyn Clock + Send + Sync>) -> Self {
        Self::with_ttl(DEFAULT_TTL, clock)
    }

    /// Builds a cache with an explicit TTL (used by the fake-clock TTL tests).
    pub fn with_ttl(ttl: Duration, clock: Arc<dyn Clock + Send + Sync>) -> Self {
        Self {
            entries: Mutex::new(HashMap::new()),
            ttl_millis: ttl.as_millis() as u64,
            clock,
        }
    }

    /// Returns the cached record for `key` when present and unexpired, else
    /// `None` (dropping the entry if it has expired).
    fn get(&self, key: &str) -> Option<CallsignRecord> {
        let now = self.clock.now_epoch_millis();
        let mut entries = self.entries.lock().expect("lock");
        match entries.get(key) {
            Some(entry) if entry.expires_at_millis > now => Some(entry.record.clone()),
            Some(_) => {
                entries.remove(key);
                None
            }
            None => None,
        }
    }

    /// Caches `record` under `key` with a fresh TTL, sweeping expired entries.
    fn insert(&self, key: &str, record: CallsignRecord) {
        let now = self.clock.now_epoch_millis();
        let mut entries = self.entries.lock().expect("lock");
        entries.retain(|_, entry| entry.expires_at_millis > now);
        entries.insert(
            key.to_owned(),
            CacheEntry {
                record,
                expires_at_millis: now + self.ttl_millis,
            },
        );
    }
}

/// Orchestrates the provider-abstracted callbook lookup — the single seam the
/// check-in autofill calls.
pub struct LookupService {
    qrz: Arc<dyn LookupProvider + Send + Sync>,
    hamcall: Arc<dyn LookupProvider + Send + Sync>,
    credentials: QrzCredentialRepo,
    cipher: Arc<dyn CredentialCipher + Send + Sync>,
    cache: Arc<LookupCache>,
    rate_limiter: Arc<LookupRateLimiter>,
}

impl LookupService {
    /// Composes the service from the two providers (behind the port), the
    /// credential store + cipher, the shared result cache, and the rate limiter.
    pub fn new(
        qrz: Arc<dyn LookupProvider + Send + Sync>,
        hamcall: Arc<dyn LookupProvider + Send + Sync>,
        credentials: QrzCredentialRepo,
        cipher: Arc<dyn CredentialCipher + Send + Sync>,
        cache: Arc<LookupCache>,
        rate_limiter: Arc<LookupRateLimiter>,
    ) -> Self {
        Self {
            qrz,
            hamcall,
            credentials,
            cipher,
            cache,
            rate_limiter,
        }
    }

    /// Best-effort lookup for the acting account. NEVER returns an error — a
    /// failure to look up degrades to `None` so the check-in proceeds on manual
    /// entry. Order: normalize → cache read → rate-limit → resolve
    /// credentials → QRZ (if credentials present) → hamcall → `None`.
    pub async fn lookup(&self, account_id: Uuid, callsign: &str) -> Option<CallsignRecord> {
        let key = callsign.trim().to_uppercase();
        if key.is_empty() {
            return None;
        }

        // Cache hit short-circuits BEFORE spending any rate-limit budget or
        // touching an upstream.
        if let Some(record) = self.cache.get(&key) {
            return Some(record);
        }

        // Over-threshold degrades to no lookup — never an error, never a 429.
        if self.rate_limiter.check(&account_id.to_string()).is_err() {
            return None;
        }

        let credentials = self.resolve_credentials(account_id).await;

        // QRZ needs a login, so it is tried ONLY when credentials are present.
        if let Some(credentials) = credentials.as_ref() {
            match self.qrz.lookup(&key, Some(credentials)).await {
                Ok(Some(record)) => {
                    self.cache.insert(&key, record.clone());
                    return Some(record);
                }
                Ok(None) | Err(LookupError::Unavailable) => {}
                Err(LookupError::InvalidCredentials) => {
                    // No secrets — just an account-scoped signal a future
                    // "your QRZ credentials look wrong" nudge can read.
                    tracing::debug!(%account_id, "qrz rejected the stored credentials");
                }
            }
        }

        match self.hamcall.lookup(&key, None).await {
            Ok(Some(record)) => {
                self.cache.insert(&key, record.clone());
                Some(record)
            }
            Ok(None) | Err(_) => None,
        }
    }

    /// Resolves the acting account's plaintext QRZ credentials, or `None` when
    /// none are stored or they cannot be opened (absent KEK, tamper, any error).
    /// NEVER propagates an error — a resolution failure simply means QRZ is
    /// skipped (hamcall-only), best-effort. The returned credentials zeroize on
    /// drop (`ZeroizeOnDrop`).
    async fn resolve_credentials(&self, account_id: Uuid) -> Option<QrzCredentials> {
        let sealed = match self.credentials.get(account_id).await {
            Ok(Some(sealed)) => sealed,
            Ok(None) => return None,
            Err(err) => {
                // Still degrades to hamcall-only (best-effort) — but a
                // repo-level failure (pool exhaustion, connectivity fault) is
                // NOT the same situation as "this account has no QRZ
                // credentials", so it gets a log line unlike the `Ok(None)`
                // case. No secrets here — account id + error only.
                tracing::warn!(
                    %account_id,
                    error = %err,
                    "qrz credential lookup failed; falling back to hamcall-only"
                );
                return None;
            }
        };
        self.cipher.open(account_id, &sealed).ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use netroll_domain::lookup::LookupSource;
    use std::sync::atomic::{AtomicU64, Ordering};

    /// A clock whose time the test advances explicitly.
    struct FakeClock(AtomicU64);
    impl Clock for FakeClock {
        fn now_epoch_millis(&self) -> u64 {
            self.0.load(Ordering::SeqCst)
        }
    }
    impl FakeClock {
        fn arc() -> Arc<Self> {
            Arc::new(Self(AtomicU64::new(1_000_000)))
        }
        fn advance(&self, millis: u64) {
            self.0.fetch_add(millis, Ordering::SeqCst);
        }
    }

    fn record(call: &str) -> CallsignRecord {
        CallsignRecord {
            callsign: call.to_owned(),
            name: Some("FRED".to_owned()),
            location: None,
            grid: None,
            source: LookupSource::Qrz,
        }
    }

    #[test]
    fn a_cached_record_is_returned_within_its_ttl() {
        let clock = FakeClock::arc();
        let cache = LookupCache::with_ttl(Duration::from_secs(3600), clock.clone());

        cache.insert("AA7BQ", record("AA7BQ"));
        clock.advance(3599 * 1000); // still inside the hour
        assert_eq!(cache.get("AA7BQ"), Some(record("AA7BQ")));
    }

    #[test]
    fn a_cached_record_expires_past_its_ttl() {
        let clock = FakeClock::arc();
        let cache = LookupCache::with_ttl(Duration::from_secs(3600), clock.clone());

        cache.insert("AA7BQ", record("AA7BQ"));
        clock.advance(3600 * 1000 + 1); // past the hour
        assert_eq!(cache.get("AA7BQ"), None);
    }

    #[test]
    fn a_write_sweeps_expired_entries() {
        let clock = FakeClock::arc();
        let cache = LookupCache::with_ttl(Duration::from_secs(3600), clock.clone());

        cache.insert("OLD", record("OLD"));
        clock.advance(3600 * 1000 + 1);
        cache.insert("NEW", record("NEW")); // sweeps OLD on write

        assert_eq!(cache.entries.lock().unwrap().len(), 1, "stale entry swept");
        assert!(cache.entries.lock().unwrap().contains_key("NEW"));
    }
}
